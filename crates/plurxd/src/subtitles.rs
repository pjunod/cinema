//! Persistent extraction cache for embedded text subtitles.
//!
//! ffmpeg/libass must read an embedded text track to EOF before it can render
//! the first frame. On a large MKV over a NAS that can exceed a player's
//! preparation timeout. Extract once to a small WebVTT sidecar: the web
//! subtitle endpoint always uses it, and burned transcodes reuse it for simple
//! text codecs whose authored styling is not lost by WebVTT conversion.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use plurx_core::domain::MediaFile;

use crate::ffmpeg::ffmpeg_bin;

/// Entries the subtitle cache may hold before the oldest are trimmed, and how
/// far a trim takes it. WebVTT files are normally only kilobytes.
const MAX_ENTRIES: usize = 256;
const TRIM_TO: usize = 224;
/// Window sidecars are cheap, disposable and always the freshest files in the
/// cache, so they get their own small budget rather than competing with the
/// whole-track sidecars they exist to bridge to.
const MAX_WINDOW_ENTRIES: usize = 64;

/// How long one extraction may run before it is abandoned and its ffmpeg
/// killed. A cold extraction is a full-source read: on a large MKV over a
/// congested NAS that has legitimately taken ~180 s, so the bound is set well
/// clear of it — 10 minutes is over three times the worst honest case, which
/// means nothing that would have succeeded is cut off, while a wedged child
/// (a stalled mount that never returns bytes and never errors) can no longer
/// park every waiter forever.
const EXTRACTION_TIMEOUT: Duration = Duration::from_secs(600);

/// How long a failure is remembered. AVPlayer asks for a VTT segment roughly
/// every segment duration (~6 s), and before this memo each of those requests
/// relaunched a full-source read of a file that had just failed. Two minutes
/// turns that storm into one attempt per window — about a twentieth of the
/// scans — while still being short enough that fixing the cause (remounting
/// the NAS, replacing the file) is picked up inside a viewer's patience
/// rather than needing a restart.
const NEGATIVE_TTL: Duration = Duration::from_secs(120);

/// Failures remembered at once. A memo is a path, a deadline and a short
/// message, so the cap is about refusing unbounded growth rather than saving
/// bytes: a library full of broken tracks must not turn the memo into a leak.
const MAX_NEGATIVE_ENTRIES: usize = 128;

/// The largest sidecar publish will accept. Real WebVTT is kilobytes — a
/// dense SDH track for a three-hour film lands near 200 KB — and this file is
/// re-read whole for every subtitle segment request, so the cap is chosen to
/// stay cheap to re-read while leaving two orders of magnitude of headroom
/// above anything legitimate. Above it the track is pathological (a
/// mislabelled stream, a runaway conversion) and is refused rather than
/// allowed to fill the cache directory.
const MAX_SIDECAR_BYTES: u64 = 8 * 1024 * 1024;

/// The bounds an extraction runs under. Held in one struct so tests can shrink
/// all three to something a suite can prove in milliseconds, while production
/// keeps the constants above.
#[derive(Clone, Copy)]
struct ExtractionLimits {
    timeout: Duration,
    negative_ttl: Duration,
    max_sidecar_bytes: u64,
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            timeout: EXTRACTION_TIMEOUT,
            negative_ttl: NEGATIVE_TTL,
            max_sidecar_bytes: MAX_SIDECAR_BYTES,
        }
    }
}

/// The stable path for one source fingerprint and subtitle stream.
pub fn vtt_path(dir: &Path, file: &MediaFile, index: i64) -> PathBuf {
    vtt_path_for_identity(dir, file.id, index, file.size, file.mtime)
}

/// Resolve a published sidecar after the source file row has been replaced by
/// a rescan. Offline packages snapshot this exact identity at request time.
pub fn vtt_path_for_identity(
    dir: &Path,
    file_id: i64,
    index: i64,
    size: i64,
    mtime: i64,
) -> PathBuf {
    dir.join(vtt_name(file_id, index, size, mtime))
}

fn vtt_name(file_id: i64, index: i64, size: i64, mtime: i64) -> String {
    format!("f{file_id}-s{index}-{size}-{mtime}.vtt")
}

/// How long one window sidecar covers, in seconds.
///
/// The size barely moves the cost — see [`windowing_is_worthwhile`] for what
/// does — so this is chosen to outlive several segment requests without being
/// so long that a viewer seeks out of it immediately.
pub const WINDOW_SECONDS_DEFAULT: i64 = 200;
/// Below this a window dies inside one AVPlayer retry cycle; above it the
/// extraction stops beating the whole-track warm on a slow source and the
/// bridge loses its point.
pub const WINDOW_SECONDS_MIN: i64 = 30;
pub const WINDOW_SECONDS_MAX: i64 = 900;

pub fn bounded_window_seconds(configured: i64) -> i64 {
    configured.clamp(WINDOW_SECONDS_MIN, WINDOW_SECONDS_MAX)
}

/// Extra span extracted past the window's nominal end.
///
/// A segment is served from the window its *start* lands in, but a segment
/// straddles the grid boundary whenever `demand % window` is within a segment
/// of the end — with 6-second segments and a 200-second window that is about
/// 3% of them. Without slack the tail of such a segment is outside the
/// extracted span and its cues are silently missing, and the short window
/// publishes successfully because it is not empty.
///
/// One minute covers any segment duration this server produces with room to
/// spare, and costs nothing measurable: the extraction reads from the start of
/// the container either way.
pub const WINDOW_SLACK_SECONDS: i64 = 60;

/// Rewrite window cue times to absolute source time.
///
/// **The timestamp base of a windowed extraction is not a constant — it
/// depends on the ffmpeg build.** Measured: ffmpeg 6.1.1 returns absolute
/// times for `-i src -ss A -to B`, and ffmpeg 4.4.2 returns them rebased to
/// zero. The Dockerfile installs jellyfin-ffmpeg deliberately unpinned so it
/// can track upstream, so the base can change under a running server without
/// anything in this repository changing.
///
/// Depending on either behaviour is therefore the bug, and detecting it is
/// cheap and exact rather than a heuristic. A rebased window's cues lie in
/// `[0, span)`; an absolute window's lie in `[anchor, anchor + span)`. Anchors
/// are grid multiples, so a non-zero anchor is at least one window long and
/// the two ranges cannot overlap. At anchor zero the two bases are the same
/// answer and there is nothing to decide.
///
/// `slice_webvtt` and the `media_origin_seconds` shift both work in absolute
/// source time, so this is what lets a window feed the same reader as the
/// whole-track sidecar.
pub fn normalize_window_cues(bytes: &[u8], anchor_seconds: i64) -> Vec<u8> {
    if anchor_seconds <= 0 {
        return bytes.to_vec();
    }
    let text = String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let anchor = anchor_seconds as f64;
    let already_absolute = text
        .lines()
        .filter_map(|line| line.split_once(" --> "))
        .filter_map(|(start, _)| parse_window_timestamp(start))
        .any(|start| start >= anchor);
    if already_absolute {
        return bytes.to_vec();
    }
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n');
        let rewritten = trimmed.split_once(" --> ").and_then(|(start, rest)| {
            // Cue settings ride along after the end timestamp and must survive
            // untouched; only the two times move.
            let (end, settings) = match rest.split_once(' ') {
                Some((end, settings)) => (end, Some(settings)),
                None => (rest, None),
            };
            let start = parse_window_timestamp(start)? + anchor;
            let end = parse_window_timestamp(end)? + anchor;
            Some(match settings {
                Some(settings) => format!(
                    "{} --> {} {settings}",
                    format_window_timestamp(start),
                    format_window_timestamp(end)
                ),
                None => format!(
                    "{} --> {}",
                    format_window_timestamp(start),
                    format_window_timestamp(end)
                ),
            })
        });
        match rewritten {
            Some(rewritten) => {
                out.push_str(&rewritten);
                if line.ends_with('\n') {
                    out.push('\n');
                }
            }
            None => out.push_str(line),
        }
    }
    out.into_bytes()
}

fn parse_window_timestamp(raw: &str) -> Option<f64> {
    let fields: Vec<&str> = raw.trim().split(':').collect();
    let (hours, minutes, seconds) = match fields.as_slice() {
        [minutes, seconds] => (
            0.0,
            minutes.parse::<f64>().ok()?,
            seconds.parse::<f64>().ok()?,
        ),
        [hours, minutes, seconds] => (
            hours.parse::<f64>().ok()?,
            minutes.parse::<f64>().ok()?,
            seconds.parse::<f64>().ok()?,
        ),
        _ => return None,
    };
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

fn format_window_timestamp(seconds: f64) -> String {
    let millis = (seconds.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        millis / 3_600_000,
        millis / 60_000 % 60,
        millis / 1000 % 60,
        millis % 1000
    )
}

/// Snap a demand position down onto the window grid.
///
/// Requests inside one window must share a cache key or every segment starts
/// its own extraction — the grid is what makes the dedup registry able to see
/// that two requests want the same bytes.
pub fn window_anchor_seconds(position_seconds: i64, window_seconds: i64) -> i64 {
    let window = bounded_window_seconds(window_seconds);
    (position_seconds.max(0) / window) * window
}

/// Whether extracting a window is cheaper than extracting the whole track.
///
/// Measured, not assumed. A subtitle-only extraction reads the container from
/// its start to the end of the requested span, so the cost is set by *where*
/// the window sits and barely at all by how long it is. On a one-hour MKV, a
/// 200-second window costs 7% of the file at the head, 57% at the midpoint,
/// and 101% — the whole thing — near the end.
///
/// So past the midpoint a window is strictly worse than the whole-track warm:
/// the same bytes are read and what comes back is disposable rather than
/// authoritative. Declining is not a fallback here, it is the better answer.
///
/// This is also why the bridge is at its best exactly where it is needed: the
/// defect it exists to fix is that the *first* minutes of a large file play
/// with no subtitles, and that is the 7% case.
pub fn windowing_is_worthwhile(
    anchor_seconds: i64,
    duration_seconds: i64,
    window_seconds: i64,
) -> bool {
    let window = bounded_window_seconds(window_seconds);
    // A file barely longer than one window has no head to bridge: the window
    // would extract effectively the whole track, concurrently with the
    // whole-track warm doing the identical scan, and publish the loser under a
    // disposable key. Require enough runtime that a window is a fraction of it.
    duration_seconds > window.saturating_mul(2)
        && anchor_seconds.max(0).saturating_mul(2) < duration_seconds
}

fn vtt_window_name(
    file_id: i64,
    index: i64,
    size: i64,
    mtime: i64,
    anchor_seconds: i64,
    window_seconds: i64,
) -> String {
    // The span is in the name because a window sidecar is only valid for the
    // span it covers, and the configured length can change under a running
    // server. Two different spans are two different answers, not one answer to
    // overwrite.
    format!("f{file_id}-s{index}-{size}-{mtime}-w{anchor_seconds}-{window_seconds}.vtt")
}

pub fn vtt_window_path(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    anchor_seconds: i64,
    window_seconds: i64,
) -> PathBuf {
    dir.join(vtt_window_name(
        file.id,
        index,
        file.size,
        file.mtime,
        anchor_seconds,
        bounded_window_seconds(window_seconds),
    ))
}

struct Extraction {
    result: tokio::sync::Mutex<Option<Result<PathBuf, String>>>,
    ready: tokio::sync::Notify,
}

type Extractions = tokio::sync::Mutex<HashMap<PathBuf, Arc<Extraction>>>;

fn extractions() -> &'static Extractions {
    static EXTRACTIONS: OnceLock<Extractions> = OnceLock::new();
    EXTRACTIONS.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

/// Detached warmers by cache key. `ensure_vtt` already deduplicates the actual
/// ffmpeg extraction; this smaller registry also deduplicates the tasks waiting
/// on it, so a player reloading an EVENT subtitle playlist cannot accumulate a
/// new ten-minute waiter on every refresh.
type Warmups = tokio::sync::Mutex<HashSet<PathBuf>>;

fn warmups() -> &'static Warmups {
    static WARMUPS: OnceLock<Warmups> = OnceLock::new();
    WARMUPS.get_or_init(|| tokio::sync::Mutex::new(HashSet::new()))
}

/// Why a cache key failed, and when that answer stops being reused.
struct NegativeMemo {
    why: String,
    expires_at: tokio::time::Instant,
}

type NegativeMemos = tokio::sync::Mutex<HashMap<PathBuf, NegativeMemo>>;

fn negative_memos() -> &'static NegativeMemos {
    static MEMOS: OnceLock<NegativeMemos> = OnceLock::new();
    MEMOS.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

/// The remembered failure for a key, if one is still inside its window. The
/// clock is tokio's, so a paused-clock test moves it without waiting.
async fn remembered_failure(cached: &Path) -> Option<String> {
    let memos = negative_memos().lock().await;
    let memo = memos.get(cached)?;
    if memo.expires_at > tokio::time::Instant::now() {
        Some(memo.why.clone())
    } else {
        None
    }
}

/// Remember a failure, evicting to stay under the cap: expired entries first,
/// since they are already dead weight, and only then the memo closest to
/// expiry — the one whose loss costs the fewest avoided rescans.
async fn remember_failure(cached: &Path, why: &str, ttl: Duration) {
    let mut memos = negative_memos().lock().await;
    let now = tokio::time::Instant::now();
    if memos.len() >= MAX_NEGATIVE_ENTRIES {
        memos.retain(|_, memo| memo.expires_at > now);
    }
    while memos.len() >= MAX_NEGATIVE_ENTRIES {
        let Some(soonest) = memos
            .iter()
            .min_by_key(|(_, memo)| memo.expires_at)
            .map(|(path, _)| path.clone())
        else {
            break;
        };
        memos.remove(&soonest);
    }
    memos.insert(
        cached.to_owned(),
        NegativeMemo {
            why: why.to_owned(),
            expires_at: now + ttl,
        },
    );
}

/// A published sidecar is the truth about a key; any memory of it failing is
/// stale the moment that happens.
async fn forget_failure(cached: &Path) {
    negative_memos().lock().await.remove(cached);
}

/// What a request should do about one cache key, decided under the registry
/// lock. See [`enlist`] for why that lock is the only place it can be decided.
enum Flight {
    /// The sidecar is already on disk. Nothing to run, nothing to wait for.
    Published,
    /// Someone else is extracting this key; wait on their result.
    Join(Arc<Extraction>),
    /// This request registered the key and owes everyone an extraction.
    Own(Arc<Extraction>),
    /// A recent failure for this key is still inside its window.
    Remembered(String),
}

/// Decide, atomically, whether a cold request joins an extraction or starts
/// one.
///
/// The unlocked cache read that precedes this is only a snapshot, and an
/// extraction that finishes between that read and this call leaves *nothing
/// to join*: the owner renames its sidecar into place and then drops its
/// registry entry, so a request landing in that window sees an absent file
/// and an absent flight, and would start a second run of work already done —
/// which is precisely the extraction a deduplicated request must never
/// perform.
///
/// The registry lock is the only ordering point that closes it. Because the
/// rename happens-before the entry is removed, and the removal happens-before
/// this lock is acquired, a key with no flight registered here is a key whose
/// sidecar is either visible on disk right now or genuinely absent. So the
/// cache is read again, under the lock, before anyone is allowed to own the
/// work. (A failed extraction leaves neither, and the next request correctly
/// retries it.)
async fn valid_sidecar(cached: &Path, max_bytes: u64) -> bool {
    let cached = cached.to_owned();
    tokio::task::spawn_blocking(move || {
        let file = plurx_core::fs_secure::open_read_nofollow_blocking(&cached).ok()?;
        let metadata = file.metadata().ok()?;
        (metadata.is_file() && metadata.len() > 0 && metadata.len() <= max_bytes).then_some(())
    })
    .await
    .ok()
    .flatten()
    .is_some()
}

async fn enlist(cached: &Path, max_bytes: u64) -> Flight {
    let mut active = extractions().lock().await;
    if let Some(flight) = active.get(cached) {
        return Flight::Join(Arc::clone(flight));
    }
    if valid_sidecar(cached, max_bytes).await {
        return Flight::Published;
    }
    // Disk beats the memo: a published sidecar is the truth about a key, and
    // `forget_failure` may not have run yet.
    if let Some(why) = remembered_failure(cached).await {
        return Flight::Remembered(why);
    }
    let flight = Arc::new(Extraction {
        result: tokio::sync::Mutex::new(None),
        ready: tokio::sync::Notify::new(),
    });
    active.insert(cached.to_owned(), Arc::clone(&flight));
    Flight::Own(flight)
}

/// Return a cached WebVTT sidecar, extracting it atomically on a miss.
pub async fn ensure_vtt(dir: &Path, file: &MediaFile, index: i64) -> Result<PathBuf, String> {
    ensure_vtt_with(dir, file, index, |tmp, file, index| async move {
        extract_vtt(&tmp, &file, index).await
    })
    .await
}

/// Return the complete sidecar bytes from the same no-follow handle that was
/// bounded. HTTP consumers must use this instead of validating a pathname and
/// reopening it, which would let a symlink/oversized replacement win between
/// the two operations.
pub async fn ensure_vtt_bytes(dir: &Path, file: &MediaFile, index: i64) -> Result<Vec<u8>, String> {
    let path = ensure_vtt(dir, file, index).await?;
    read_vtt_path(&path, MAX_SIDECAR_BYTES)
        .await?
        .ok_or_else(|| "published subtitle sidecar is no longer valid".to_owned())
}

/// Read a warm sidecar without launching extraction. Used by AVPlayer's
/// short-deadline segmented subtitle route, where a cache miss must return an
/// empty segment immediately and warm in the background.
pub async fn read_cached_vtt(
    dir: &Path,
    file: &MediaFile,
    index: i64,
) -> Result<Option<Vec<u8>>, String> {
    read_vtt_path(&vtt_path(dir, file, index), MAX_SIDECAR_BYTES).await
}

async fn read_vtt_path(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>, String> {
    match plurx_core::fs_secure::read_bounded_regular(path, max_bytes).await {
        Ok(bytes) if !bytes.is_empty() => Ok(Some(bytes)),
        Ok(_) => Ok(None),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(format!("reading subtitle sidecar: {error}")),
    }
}

/// Hold the exact bounded subtitle inode for ffmpeg burn-in. The caller passes
/// this descriptor through `/dev/fd/5`; later pathname replacement cannot
/// change the bytes libass consumes.
pub async fn ensure_vtt_file(
    dir: &Path,
    file: &MediaFile,
    index: i64,
) -> Result<std::fs::File, String> {
    let path = ensure_vtt(dir, file, index).await?;
    tokio::task::spawn_blocking(move || {
        let handle = plurx_core::fs_secure::open_read_nofollow_blocking(&path)
            .map_err(|error| format!("opening subtitle sidecar: {error}"))?;
        let metadata = handle
            .metadata()
            .map_err(|error| format!("reading subtitle sidecar metadata: {error}"))?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_SIDECAR_BYTES {
            return Err("published subtitle sidecar is not a bounded regular file".to_owned());
        }
        Ok(handle)
    })
    .await
    .map_err(|error| format!("subtitle sidecar open worker failed: {error}"))?
}

/// Start materialising a sidecar without holding the caller open.
///
/// Native HLS uses this before returning an empty cold-cache segment. AVPlayer
/// blocks the muxed video behind subtitle segment I/O and gives that I/O about
/// two seconds, while a real extraction may scan a multi-gigabyte source for
/// minutes. One detached warmer per key lets playback begin immediately and
/// lets later segments pick up the finished captions.
pub async fn warm_vtt(dir: &Path, file: &MediaFile, index: i64) {
    let cached = vtt_path(dir, file, index);
    if valid_sidecar(&cached, MAX_SIDECAR_BYTES).await {
        return;
    }
    if !warmups().lock().await.insert(cached.clone()) {
        return;
    }

    let dir = dir.to_owned();
    let file = file.clone();
    tokio::spawn(async move {
        let _ = ensure_vtt(&dir, &file, index).await;
        warmups().lock().await.remove(&cached);
    });
}

/// The production seam: default bounds, injected extractor.
async fn ensure_vtt_with<F, Fut>(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    extract: F,
) -> Result<PathBuf, String>
where
    F: FnOnce(PathBuf, MediaFile, i64) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    ensure_vtt_bounded(dir, file, index, ExtractionLimits::default(), extract).await
}

/// One extraction per cache key. The extraction itself lives in an owned
/// task, rather than in the HTTP request awaiting it: if AVPlayer times out or
/// a client cancels, ffmpeg still publishes the completed sidecar and every
/// concurrent waiter observes that same result.
async fn ensure_vtt_bounded<F, Fut>(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    limits: ExtractionLimits,
    extract: F,
) -> Result<PathBuf, String>
where
    F: FnOnce(PathBuf, MediaFile, i64) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    let cached = vtt_path(dir, file, index);
    ensure_vtt_at(cached, dir, file, index, limits, extract).await
}

/// The same machinery against a caller-chosen cache key.
///
/// Window sidecars are a different key for the same source, and they want
/// every guarantee this path already provides: one flight per key, the
/// negative memo, the bounded read, and the atomic publish whose loser's
/// rename is a no-op. Passing the key in was the whole refactor — none of the
/// rules below know or care which shape of sidecar they are protecting.
async fn ensure_vtt_at<F, Fut>(
    cached: PathBuf,
    dir: &Path,
    file: &MediaFile,
    index: i64,
    limits: ExtractionLimits,
    extract: F,
) -> Result<PathBuf, String>
where
    F: FnOnce(PathBuf, MediaFile, i64) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    if valid_sidecar(&cached, limits.max_sidecar_bytes).await {
        return Ok(cached);
    }

    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| format!("creating subtitle cache: {e}"))?;
    let directory = plurx_core::fs_secure::SecureDirectory::open(dir)
        .await
        .map_err(|error| format!("opening subtitle cache: {error}"))?;
    let (flight, owner) = match enlist(&cached, limits.max_sidecar_bytes).await {
        Flight::Published => return Ok(cached),
        // An in-flight extraction outranks the memo; a remembered failure
        // outranks starting the scan over again.
        Flight::Remembered(why) => return Err(why),
        Flight::Join(flight) => (flight, false),
        Flight::Own(flight) => (flight, true),
    };

    if owner {
        let tmp = dir.join(format!(".tmp-{}.vtt", uuid::Uuid::new_v4()));
        let cached_for_task = cached.clone();
        let flight_for_task = Arc::clone(&flight);
        let file = file.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            tracing::info!(
                file_id = file.id,
                index,
                "extracting embedded text subtitle to the sidecar cache"
            );
            let result = publish_extraction(
                &directory,
                &cached_for_task,
                &tmp,
                &file,
                index,
                limits,
                extract,
            )
            .await;
            match &result {
                Ok(_) => {
                    tracing::info!(
                        file_id = file.id,
                        index,
                        elapsed_ms = started.elapsed().as_millis(),
                        "text subtitle sidecar cached"
                    );
                    forget_failure(&cached_for_task).await;
                }
                Err(why) => {
                    tracing::warn!(
                        file_id = file.id,
                        index,
                        elapsed_ms = started.elapsed().as_millis(),
                        why,
                        "text subtitle extraction failed; suppressing retries for now"
                    );
                    remember_failure(&cached_for_task, why, limits.negative_ttl).await;
                }
            }
            *flight_for_task.result.lock().await = Some(result);
            extractions().lock().await.remove(&cached_for_task);
            flight_for_task.ready.notify_waiters();
        });
    }

    loop {
        // Register before inspecting the result so a publish between the two
        // operations cannot become a lost notification.
        let notified = flight.ready.notified();
        if let Some(result) = flight.result.lock().await.clone() {
            return result;
        }
        notified.await;
    }
}

/// Extract one bounded span of an embedded text track.
///
/// **`-ss` and `-to` go after `-i`, and that is load-bearing.** The obvious
/// form — `-ss` before the input, so it is an index seek — is wrong twice, and
/// both failures are silent:
///
/// - It rewrites cue timestamps toward zero. `slice_webvtt` and the
///   `media_origin_seconds` shift both work in absolute source time, so a
///   zero-based sidecar makes them select nothing and the handler falls back
///   to the empty segment — which is the exact defect windowing exists to
///   remove, wearing a different hat and passing a deadline test.
/// - It does not bound the span. Measured against a fixture with cues at known
///   times, `-ss 1800 -to 2000 -i src` returns *every cue in the file*.
///
/// `-copyts` restores the timestamps and still does not bound, which makes it
/// the more dangerous near-miss: it fixes the half a reviewer checks.
///
/// The form below was measured to be the only one correct on both axes. It is
/// an output seek, so ffmpeg reads the container from its start — see
/// [`windowing_is_worthwhile`] for why that is acceptable and where it stops
/// being so.
async fn extract_vtt_window(
    tmp: &Path,
    file: &MediaFile,
    index: i64,
    anchor_seconds: i64,
    window_seconds: i64,
) -> Result<(), String> {
    let end = anchor_seconds
        .saturating_add(bounded_window_seconds(window_seconds))
        .saturating_add(WINDOW_SLACK_SECONDS);
    let out = tokio::process::Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&file.path)
        .args([
            "-ss",
            &anchor_seconds.to_string(),
            "-to",
            &end.to_string(),
            "-map",
            &format!("0:s:{index}"),
            "-f",
            "webvtt",
        ])
        .arg(tmp)
        .stdin(std::process::Stdio::null())
        // Same reason as the whole-track extractor: the bound is a kill, not
        // merely a stopped wait, or a wedged ffmpeg keeps the stalled mount
        // open after the future is dropped.
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("spawning subtitle window extraction: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        return Err(format!("subtitle window extraction failed: {}", why.trim()));
    }
    // Normalize before the file is published, so what lands in the cache is
    // always absolute-time whatever this ffmpeg build chose to emit. Doing it
    // here rather than at read time means the base is decided once, by the
    // code that knows the anchor, instead of on every segment request.
    let extracted = tokio::fs::read(tmp)
        .await
        .map_err(|e| format!("reading the extracted subtitle window: {e}"))?;
    let normalized = normalize_window_cues(&extracted, anchor_seconds);
    if normalized != extracted {
        tokio::fs::write(tmp, &normalized)
            .await
            .map_err(|e| format!("rewriting the extracted subtitle window: {e}"))?;
    }
    Ok(())
}

/// Read a warm window sidecar without launching anything.
pub async fn read_cached_window(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    anchor_seconds: i64,
    window_seconds: i64,
) -> Result<Option<Vec<u8>>, String> {
    read_vtt_path(
        &vtt_window_path(dir, file, index, anchor_seconds, window_seconds),
        MAX_SIDECAR_BYTES,
    )
    .await
}

/// Start a window extraction if one is worth starting, and do not wait for it.
///
/// Declines for a reason rather than failing: past the midpoint of the file a
/// window costs what the whole track costs, so the honest move is to let
/// `warm_vtt` produce the authoritative sidecar instead of a disposable one
/// for the same bytes. Returns whether a window is now the thing to wait for.
pub async fn warm_vtt_window(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    anchor_seconds: i64,
    window_seconds: i64,
) -> bool {
    let Some(duration_seconds) = file.duration_ms.map(|ms| ms / 1_000) else {
        // Without a duration there is no midpoint to compare against, and
        // guessing one would be guessing about I/O cost. The whole-track warm
        // is already running; let it.
        return false;
    };
    // Clamped here as well as by `window_anchor_seconds`: a negative anchor
    // would become a filename with a minus in it and an `-ss -500`, and the
    // safety should not live only in the caller.
    let anchor_seconds = anchor_seconds.max(0);
    if !windowing_is_worthwhile(anchor_seconds, duration_seconds, window_seconds) {
        return false;
    }
    let cached = vtt_window_path(dir, file, index, anchor_seconds, window_seconds);
    if valid_sidecar(&cached, MAX_SIDECAR_BYTES).await {
        return true;
    }
    if !warmups().lock().await.insert(cached.clone()) {
        // Already in flight for this exact span. A seek storm inside one
        // window must not fan out into one ffmpeg per segment request.
        return true;
    }

    let dir_owned = dir.to_owned();
    let file_owned = file.clone();
    let window = bounded_window_seconds(window_seconds);
    tokio::spawn(async move {
        let _ = ensure_vtt_at(
            cached.clone(),
            &dir_owned,
            &file_owned,
            index,
            ExtractionLimits::default(),
            move |tmp, file, index| async move {
                extract_vtt_window(&tmp, &file, index, anchor_seconds, window).await
            },
        )
        .await;
        warmups().lock().await.remove(&cached);
    });
    true
}

async fn extract_vtt(tmp: &Path, file: &MediaFile, index: i64) -> Result<(), String> {
    let out = tokio::process::Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&file.path)
        .args(["-map", &format!("0:s:{index}"), "-f", "webvtt"])
        .arg(tmp)
        .stdin(std::process::Stdio::null())
        // The timeout in `publish_extraction` bounds this by dropping the
        // future; without `kill_on_drop` that drop would merely stop waiting
        // and leave the wedged ffmpeg holding the stalled mount open. This is
        // what makes the bound an actual kill.
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("spawning subtitle extraction: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        return Err(format!("subtitle extraction failed: {}", why.trim()));
    }
    Ok(())
}

async fn publish_extraction<F, Fut>(
    directory: &plurx_core::fs_secure::SecureDirectory,
    cached: &Path,
    tmp: &Path,
    file: &MediaFile,
    index: i64,
    limits: ExtractionLimits,
    extract: F,
) -> Result<PathBuf, String>
where
    F: FnOnce(PathBuf, MediaFile, i64) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let cached_name = cached
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "subtitle cache path has no safe filename".to_owned())?;
    let tmp_name = tmp
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "subtitle temp path has no safe filename".to_owned())?;
    // A timeout here rather than around the whole task: expiring drops the
    // extractor future (killing its child) while this frame still owns the
    // temp file and can delete it, so a wedge leaves the cache dir as clean as
    // an ordinary failure does.
    let extracted =
        match tokio::time::timeout(limits.timeout, extract(tmp.to_owned(), file.clone(), index))
            .await
        {
            Ok(extracted) => extracted,
            Err(_) => Err(format!(
                "subtitle extraction timed out after {}s",
                limits.timeout.as_secs()
            )),
        };
    if let Err(e) = extracted {
        let _ = directory.unlink_child(tmp_name).await;
        return Err(e);
    }
    // Read through the held directory capability before publication, so an
    // oversized or symlink-swapped temp file is never visible under its cache
    // name: publishing it would commit the daemon to re-reading it for every
    // segment request until the cache is trimmed.
    let bytes = match directory
        .read_bounded_child(tmp_name, limits.max_sidecar_bytes)
        .await
    {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = directory.unlink_child(tmp_name).await;
            return Err(format!("reading extracted subtitle sidecar: {error}"));
        }
    };
    let size = bytes.len() as u64;
    if size == 0 || size > limits.max_sidecar_bytes {
        let _ = directory.unlink_child(tmp_name).await;
        if size == 0 {
            return Err("subtitle extraction produced an empty sidecar".to_owned());
        }
        return Err(format!(
            "subtitle sidecar is {size} bytes, above the {} byte cap",
            limits.max_sidecar_bytes
        ));
    }
    if let Err(error) = directory.atomic_write_child(cached_name, &bytes).await {
        let _ = directory.unlink_child(tmp_name).await;
        return Err(format!("publishing subtitle cache: {error}"));
    }
    let _ = directory.unlink_child(tmp_name).await;
    if let Some(dir) = cached.parent() {
        prune(dir).await;
    }
    Ok(cached.to_owned())
}

/// Drop the oldest cached subtitles once the cache outgrows its cap, and any
/// abandoned temp file a crashed extraction left behind.
async fn prune(dir: &Path) {
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
        return;
    };
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let mut windows: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    while let Ok(Some(entry)) = rd.next_entry().await {
        let path = entry.path();
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".tmp-") {
            if modified.elapsed().is_ok_and(|age| age.as_secs() > 3600) {
                let _ = tokio::fs::remove_file(&path).await;
            }
            continue;
        }
        if is_window_name(&name) {
            windows.push((modified, path));
        } else {
            entries.push((modified, path));
        }
    }
    // Windows are evicted first, and on their own budget.
    //
    // Left in one pool they would be the wrong thing to keep on both counts:
    // they are always the *freshest* files, so an oldest-first trim reaches
    // past them for the whole-track sidecars — which cost minutes of scanning
    // a NAS to produce, where a window costs seconds and is disposable by
    // design. A two-hour file yields a window per grid step across its first
    // half, so a handful of long files could otherwise evict the cache this
    // exists to bridge to.
    if windows.len() > MAX_WINDOW_ENTRIES {
        windows.sort_by_key(|(modified, _)| *modified);
        let doomed = windows.len() - MAX_WINDOW_ENTRIES;
        for (_, path) in windows.into_iter().take(doomed) {
            let _ = tokio::fs::remove_file(path).await;
        }
    }
    if entries.len() <= MAX_ENTRIES {
        return;
    }
    entries.sort_by_key(|(modified, _)| *modified);
    let doomed = entries.len() - TRIM_TO;
    for (_, path) in entries.into_iter().take(doomed) {
        let _ = tokio::fs::remove_file(path).await;
    }
}

/// Whether a cache entry is a window sidecar rather than a whole-track one.
///
/// Keyed on the shape [`vtt_window_name`] writes — `-w{anchor}-{span}.vtt` —
/// which a whole-track name cannot produce: its own trailing field is the
/// source mtime, with no `w` prefix and one fewer separator.
fn is_window_name(name: &str) -> bool {
    name.strip_suffix(".vtt")
        .and_then(|stem| stem.rsplit_once('-'))
        .and_then(|(head, span)| {
            span.parse::<i64>().ok()?;
            head.rsplit_once('-')
        })
        .and_then(|(_, anchor)| anchor.strip_prefix('w'))
        .is_some_and(|anchor| anchor.parse::<i64>().is_ok())
}

#[cfg(test)]
mod tests {
    /// The measurement this rule exists for, kept where it can be checked.
    ///
    /// A subtitle-only extraction reads the container from its start to the
    /// end of the requested span, so cost tracks the window's *position*, not
    /// its length. Measured on a one-hour MKV with a 200-second window:
    /// 7% of the file at the head, 23% at ten minutes, 57% at the midpoint,
    /// 90% at fifty minutes, and 101% — the whole file, the same as extracting
    /// the whole track — near the end.
    ///
    /// Half is therefore where a window stops paying for itself, and past it
    /// the whole-track warm is strictly better: same bytes, authoritative
    /// result instead of a disposable one.
    #[test]
    fn a_window_is_declined_once_it_costs_what_the_whole_track_costs() {
        let hour = 3_600;
        let w = super::WINDOW_SECONDS_DEFAULT;
        assert!(super::windowing_is_worthwhile(0, hour, w));
        assert!(super::windowing_is_worthwhile(600, hour, w));
        assert!(super::windowing_is_worthwhile(1_799, hour, w));
        // The midpoint is where the measured cost reaches the whole track's
        // neighbourhood; at and past it, decline.
        assert!(!super::windowing_is_worthwhile(1_800, hour, w));
        assert!(!super::windowing_is_worthwhile(3_000, hour, w));
        assert!(!super::windowing_is_worthwhile(3_600, hour, w));
        // A duration we do not know is not a duration of zero: there is no
        // midpoint to compare against, so there is no saving to claim.
        assert!(!super::windowing_is_worthwhile(0, 0, w));
        assert!(!super::windowing_is_worthwhile(10, -1, w));
        // A file barely longer than one window has no head to bridge — the
        // window would extract the whole track alongside the warm doing the
        // same scan, and publish the loser under a disposable key.
        assert!(!super::windowing_is_worthwhile(0, w, w));
        assert!(!super::windowing_is_worthwhile(0, w * 2, w));
        assert!(super::windowing_is_worthwhile(0, w * 2 + 1, w));
        // A position before the file is a client bug. It is clamped rather
        // than trusted, in both the grid and the warmer, so neither a negative
        // filename nor a negative `-ss` can be produced.
        assert_eq!(super::window_anchor_seconds(-500, w), 0);
    }

    /// The base of a windowed extraction depends on the ffmpeg build, and the
    /// build is deliberately unpinned.
    ///
    /// Measured: ffmpeg 6.1.1 returns absolute cue times for
    /// `-i src -ss A -to B`; ffmpeg 4.4.2 returns them rebased to zero. Two
    /// reviewers of this change measured it on different builds and reached
    /// opposite conclusions, which is the clearest possible evidence that
    /// depending on either is the bug.
    ///
    /// Detection is exact rather than heuristic: anchors are grid multiples,
    /// so a non-zero anchor is at least one window long, and a rebased
    /// window's cues in `[0, span)` cannot overlap an absolute window's in
    /// `[anchor, anchor + span)`.
    #[test]
    fn a_window_is_normalized_to_absolute_time_whatever_ffmpeg_emitted() {
        let rebased = b"WEBVTT\n\n00:00:10.000 --> 00:00:12.000\nfirst\n\n00:00:50.000 --> 00:00:52.000\nsecond\n";
        let absolute = b"WEBVTT\n\n00:03:30.000 --> 00:03:32.000\nfirst\n\n00:04:10.000 --> 00:04:12.000\nsecond\n";

        // A rebased window at anchor 200 is shifted onto the source clock.
        assert_eq!(
            super::normalize_window_cues(rebased, 200),
            absolute.to_vec(),
            "a rebased window must be shifted by its anchor"
        );
        // An absolute window is already right and must not be shifted twice —
        // the failure that would produce is invisible, because the result is
        // still well-formed WebVTT.
        assert_eq!(
            super::normalize_window_cues(absolute, 200),
            absolute.to_vec(),
            "an absolute window must be left alone"
        );
        // At anchor zero the two bases are the same answer.
        assert_eq!(super::normalize_window_cues(rebased, 0), rebased.to_vec());

        // Cue settings ride after the end timestamp and must survive.
        let styled = b"WEBVTT\n\n00:00:10.000 --> 00:00:12.000 line:90% align:center\nx\n";
        let shifted = super::normalize_window_cues(styled, 200);
        let shifted = String::from_utf8(shifted).expect("utf8");
        assert!(
            shifted.contains("00:03:30.000 --> 00:03:32.000 line:90% align:center"),
            "settings must survive the shift: {shifted}"
        );

        // A window with no cues at all is not evidence of either base, and
        // must not be corrupted on the way through.
        assert_eq!(
            super::normalize_window_cues(b"WEBVTT\n\n", 200),
            b"WEBVTT\n\n".to_vec()
        );
    }

    /// Requests inside one window must share a cache key, or every segment
    /// request starts its own extraction and the dedup registry never sees
    /// that two of them want the same bytes.
    #[test]
    fn demand_positions_inside_one_window_share_an_anchor() {
        assert_eq!(super::window_anchor_seconds(0, 200), 0);
        assert_eq!(super::window_anchor_seconds(199, 200), 0);
        assert_eq!(super::window_anchor_seconds(200, 200), 200);
        assert_eq!(super::window_anchor_seconds(399, 200), 200);
        assert_eq!(super::window_anchor_seconds(1_000, 200), 1_000);
        // The configured length is clamped before it is used as a grid, so a
        // nonsense setting cannot produce a nonsense anchor or a divide by
        // zero.
        assert_eq!(super::bounded_window_seconds(0), super::WINDOW_SECONDS_MIN);
        assert_eq!(
            super::bounded_window_seconds(100_000),
            super::WINDOW_SECONDS_MAX
        );
        assert_eq!(super::window_anchor_seconds(100, 0), 90);
        assert_eq!(super::bounded_window_seconds(200), 200);
    }

    /// The prune must be able to tell the two shapes apart, or it evicts the
    /// expensive sidecars to make room for the cheap ones.
    #[test]
    fn a_window_name_is_distinguishable_from_a_whole_track_name() {
        assert!(super::is_window_name("f7-s0-11-13-w200-200.vtt"));
        assert!(super::is_window_name("f7-s0-11-13-w0-30.vtt"));
        // The whole-track shape ends in the source mtime: no `w`, one fewer
        // separator. Misreading one as a window would put it on the small
        // budget and evict it early.
        assert!(!super::is_window_name("f7-s0-11-13.vtt"));
        assert!(!super::is_window_name("f7-s0-12345-678.vtt"));
        assert!(!super::is_window_name("f7-s0-11-13-w200-200.txt"));
        assert!(!super::is_window_name(".tmp-abc.vtt"));
        assert!(!super::is_window_name("nonsense"));
        // And the real names agree with the predicate.
        let dir = std::path::Path::new("/tmp/subs");
        let file = media_file(PathBuf::from("/tmp/source.mkv"));
        let window = super::vtt_window_path(dir, &file, 0, 200, 200);
        let whole = super::vtt_path(dir, &file, 0);
        assert!(super::is_window_name(
            &window.file_name().unwrap().to_string_lossy()
        ));
        assert!(!super::is_window_name(
            &whole.file_name().unwrap().to_string_lossy()
        ));
    }

    /// Two spans of the same track are two answers, not one answer to
    /// overwrite — the configured length can change under a running server.
    #[test]
    fn a_window_sidecar_is_keyed_by_its_span() {
        let dir = std::path::Path::new("/tmp/subs");
        let file = media_file(PathBuf::from("/tmp/source.mkv"));
        let at = |anchor, window| super::vtt_window_path(dir, &file, 0, anchor, window);
        assert_ne!(at(0, 200), at(200, 200));
        assert_ne!(at(0, 200), at(0, 400));
        assert_eq!(at(200, 200), at(200, 200));
        // And never collides with the whole-track key for the same source.
        assert_ne!(at(0, 200), super::vtt_path(dir, &file, 0));
    }

    use super::*;

    fn media_file(path: PathBuf) -> MediaFile {
        MediaFile {
            id: 77,
            item_id: 1,
            path,
            size: 12_345,
            mtime: 678,
            duration_ms: Some(60_000),
            container: Some("mkv".into()),
            video_codec: Some("h264".into()),
            video_profile: None,
            width: Some(1920),
            height: Some(1080),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            bitrate: Some(8_000_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    #[test]
    fn cache_key_changes_with_source_identity_and_track() {
        let first = vtt_name(42, 2, 1_000, 200);
        assert_eq!(first, "f42-s2-1000-200.vtt");
        assert_ne!(first, vtt_name(42, 3, 1_000, 200));
        assert_ne!(first, vtt_name(42, 2, 1_000, 201));
    }

    #[tokio::test]
    async fn cold_extraction_is_deduplicated_and_survives_waiter_cancellation() {
        let dir = crate::test_tempdir().expect("cache");
        let file = media_file(dir.path().join("source.mkv"));
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();

        let first = {
            let dir = dir.path().to_owned();
            let file = file.clone();
            let runs = Arc::clone(&runs);
            let release = Arc::clone(&release);
            tokio::spawn(async move {
                ensure_vtt_with(&dir, &file, 0, move |tmp, _, _| async move {
                    runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let _ = started_tx.send(());
                    let _permit = release.acquire().await.expect("release");
                    tokio::fs::write(tmp, "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nhello\n")
                        .await
                        .map_err(|e| e.to_string())
                })
                .await
            })
        };
        started_rx.await.expect("extraction started");

        // A second cold request joins the same extraction. Dropping the first
        // simulates AVPlayer timing out while ffmpeg is still reading the MKV.
        let second = {
            let dir = dir.path().to_owned();
            let file = file.clone();
            let runs = Arc::clone(&runs);
            tokio::spawn(async move {
                ensure_vtt_with(&dir, &file, 0, move |_, _, _| async move {
                    runs.fetch_add(100, std::sync::atomic::Ordering::SeqCst);
                    Err("a deduplicated runner must never execute".into())
                })
                .await
            })
        };
        first.abort();
        release.add_permits(1);

        // A LIVENESS bound, not a latency one: the assertion is that an
        // aborted waiter does not strand the extraction, and any finite
        // deadline proves that. Two seconds did not — this suite runs on
        // 2-core CI runners, and a loaded box missed it while the code was
        // perfectly correct. Generous enough to never lie, short enough that
        // a genuine hang still fails the run instead of hanging it.
        let published = tokio::time::timeout(std::time::Duration::from_secs(30), second)
            .await
            .expect("an aborted waiter must not strand the extraction")
            .expect("waiter task")
            .expect("published VTT");
        assert_eq!(
            runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "only the owner runs extraction"
        );
        assert_eq!(published, vtt_path(dir.path(), &file, 0));
        assert!(tokio::fs::metadata(&published).await.is_ok());

        let mut entries = tokio::fs::read_dir(dir.path()).await.expect("read cache");
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            assert!(
                !entry.file_name().to_string_lossy().starts_with(".tmp-"),
                "a completed extraction must not remain unpublished"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_invalid_sidecar_repair_cannot_remove_the_replacement() {
        let dir = crate::test_tempdir().expect("cache");
        let file = media_file(dir.path().join("source.mkv"));
        let cached = vtt_path(dir.path(), &file, 0);
        tokio::fs::write(&cached, vec![b'x'; 65])
            .await
            .expect("oversized stale sidecar");
        let limits = ExtractionLimits {
            max_sidecar_bytes: 64,
            ..ExtractionLimits::default()
        };
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let first = {
            let dir = dir.path().to_owned();
            let file = file.clone();
            let runs = Arc::clone(&runs);
            let release = Arc::clone(&release);
            tokio::spawn(async move {
                ensure_vtt_bounded(&dir, &file, 0, limits, move |tmp, _, _| async move {
                    runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let _ = started_tx.send(());
                    let _permit = release.acquire().await.expect("release repair");
                    tokio::fs::write(tmp, b"WEBVTT\n")
                        .await
                        .map_err(|error| error.to_string())
                })
                .await
            })
        };
        started_rx.await.expect("repair started");
        let second = {
            let dir = dir.path().to_owned();
            let file = file.clone();
            let runs = Arc::clone(&runs);
            tokio::spawn(async move {
                ensure_vtt_bounded(&dir, &file, 0, limits, move |_, _, _| async move {
                    runs.fetch_add(100, std::sync::atomic::Ordering::SeqCst);
                    Err("concurrent repair must join the owner".to_owned())
                })
                .await
            })
        };
        tokio::task::yield_now().await;
        release.add_permits(1);
        assert_eq!(
            first.await.expect("first waiter").expect("first repair"),
            cached
        );
        assert_eq!(
            second.await.expect("second waiter").expect("joined repair"),
            cached
        );
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            tokio::fs::read(&cached).await.expect("published sidecar"),
            b"WEBVTT\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn http_sidecar_reads_reject_a_symlink_replacement() {
        let dir = crate::test_tempdir().expect("cache");
        let outside = tempfile::NamedTempFile::new().expect("outside file");
        std::fs::write(outside.path(), b"outside subtitle bytes").expect("outside bytes");
        let file = media_file(dir.path().join("source.mkv"));
        let cached = vtt_path(dir.path(), &file, 0);
        tokio::fs::write(&cached, b"WEBVTT\n\noriginal\n")
            .await
            .expect("warm sidecar");
        tokio::fs::remove_file(&cached)
            .await
            .expect("remove warm path");
        std::os::unix::fs::symlink(outside.path(), &cached).expect("replace with symlink");

        let observed = read_cached_vtt(dir.path(), &file, 0).await;
        assert!(
            !matches!(observed, Ok(Some(bytes)) if bytes == b"outside subtitle bytes"),
            "the HTTP path must never follow a replacement sidecar symlink"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn burn_in_keeps_the_exact_bounded_sidecar_handle() {
        use std::io::{Read, Seek};

        let dir = crate::test_tempdir().expect("cache");
        let file = media_file(dir.path().join("source.mkv"));
        let cached = vtt_path(dir.path(), &file, 0);
        let original = b"WEBVTT\n\n00:00:00.000 --> 00:00:01.000\noriginal\n";
        tokio::fs::write(&cached, original)
            .await
            .expect("warm sidecar");
        let mut held = ensure_vtt_file(dir.path(), &file, 0)
            .await
            .expect("held subtitle handle");
        let replacement = dir.path().join("replacement.vtt");
        tokio::fs::write(&replacement, b"WEBVTT\n\nreplacement\n")
            .await
            .expect("replacement bytes");
        tokio::fs::rename(&replacement, &cached)
            .await
            .expect("replace sidecar path");

        held.rewind().expect("rewind held sidecar");
        let mut bytes = Vec::new();
        held.read_to_end(&mut bytes).expect("read held sidecar");
        assert_eq!(bytes, original);
        assert_ne!(
            tokio::fs::read(&cached).await.expect("replacement"),
            original
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn extraction_never_publishes_a_symlink_swapped_temp_file() {
        let dir = crate::test_tempdir().expect("cache");
        let outside = tempfile::NamedTempFile::new().expect("outside file");
        std::fs::write(outside.path(), b"WEBVTT\n\noutside\n").expect("outside bytes");
        let file = media_file(dir.path().join("source.mkv"));
        let outside_path = outside.path().to_owned();
        let result = ensure_vtt_bounded(
            dir.path(),
            &file,
            0,
            ExtractionLimits::default(),
            move |tmp, _, _| async move {
                std::os::unix::fs::symlink(outside_path, tmp).map_err(|error| error.to_string())
            },
        )
        .await;

        assert!(result.is_err());
        assert!(tokio::fs::symlink_metadata(vtt_path(dir.path(), &file, 0))
            .await
            .is_err());
        assert_eq!(
            std::fs::read(outside.path()).expect("outside survives"),
            b"WEBVTT\n\noutside\n"
        );
    }

    /// Real time, deliberately: the bounds under test are injected small, so
    /// the suite proves them in milliseconds without a paused clock — which
    /// here would be a trap, since every task in this test parks in file IO
    /// and the runtime would leap straight to the extraction deadline.
    fn wedged_limits() -> ExtractionLimits {
        ExtractionLimits {
            timeout: Duration::from_millis(150),
            negative_ttl: Duration::from_secs(60),
            ..ExtractionLimits::default()
        }
    }

    /// P1-4: a stalled NAS read used to park every waiter forever on
    /// `notified()`, and each following segment request relaunched the same
    /// doomed multi-GB scan. Both waiters must come back with a bounded error,
    /// and the request after them must be answered from the memo.
    #[tokio::test]
    async fn a_wedged_extraction_times_out_and_the_failure_is_memoized() {
        let dir = crate::test_tempdir().expect("cache");
        let file = media_file(dir.path().join("source.mkv"));
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();

        let first = {
            let dir = dir.path().to_owned();
            let file = file.clone();
            let runs = Arc::clone(&runs);
            tokio::spawn(async move {
                ensure_vtt_bounded(&dir, &file, 0, wedged_limits(), move |_, _, _| async move {
                    runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let _ = started_tx.send(());
                    // The wedge: an ffmpeg that never returns and never errors.
                    tokio::time::sleep(Duration::from_secs(3_600)).await;
                    Ok(())
                })
                .await
            })
        };
        started_rx.await.expect("extraction started");

        // Joins the same flight, the way a second segment request would.
        let second = {
            let dir = dir.path().to_owned();
            let file = file.clone();
            let runs = Arc::clone(&runs);
            tokio::spawn(async move {
                ensure_vtt_bounded(&dir, &file, 0, wedged_limits(), move |_, _, _| async move {
                    runs.fetch_add(100, std::sync::atomic::Ordering::SeqCst);
                    Err("a deduplicated runner must never execute".into())
                })
                .await
            })
        };

        for waiter in [first, second] {
            let observed = tokio::time::timeout(Duration::from_secs(10), waiter)
                .await
                .expect("a wedged producer must not park its waiters")
                .expect("waiter task");
            let why = observed.expect_err("a wedged extraction cannot publish");
            assert!(
                why.contains("timed out"),
                "waiters observe the bounded error, got {why}"
            );
        }

        // The memo is written before the flight is dropped, so by the time a
        // waiter has its answer a fresh request is already covered.
        let runs_for_repeat = Arc::clone(&runs);
        let repeat = ensure_vtt_bounded(
            dir.path(),
            &file,
            0,
            wedged_limits(),
            move |_, _, _| async move {
                runs_for_repeat.fetch_add(100, std::sync::atomic::Ordering::SeqCst);
                Err("a memoized failure must not relaunch the scan".into())
            },
        )
        .await;
        assert!(repeat.is_err(), "the memo answers with the same failure");
        assert_eq!(
            runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one scan per memo window, not one per request"
        );
        forget_failure(&vtt_path(dir.path(), &file, 0)).await;
    }

    /// The other side of the TTL comparison: an expired memo must not become a
    /// permanent tomb for a track whose cause has since been fixed, and the
    /// success that follows must clear it outright rather than leave a corpse
    /// in the map.
    #[tokio::test]
    async fn an_expired_memo_relaunches_and_success_clears_it() {
        let dir = crate::test_tempdir().expect("cache");
        let file = media_file(dir.path().join("source.mkv"));
        let key = vtt_path(dir.path(), &file, 0);
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // A zero TTL is a memo that is written and already expired: the
        // deterministic version of "the window has passed", with no wall clock
        // in the test.
        let expired = ExtractionLimits {
            negative_ttl: Duration::ZERO,
            ..ExtractionLimits::default()
        };

        let runs_for_failure = Arc::clone(&runs);
        let failed = ensure_vtt_bounded(dir.path(), &file, 0, expired, move |_, _, _| async move {
            runs_for_failure.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err("no such stream".into())
        })
        .await;
        assert!(failed.is_err(), "the first attempt fails");
        assert!(
            negative_memos().lock().await.contains_key(&key),
            "the failure was memoized, expired or not"
        );

        let runs_for_success = Arc::clone(&runs);
        let published =
            ensure_vtt_bounded(dir.path(), &file, 0, expired, move |tmp, _, _| async move {
                runs_for_success.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::fs::write(tmp, "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nhello\n")
                    .await
                    .map_err(|e| e.to_string())
            })
            .await
            .expect("an expired memo cannot block a working extraction");

        assert_eq!(published, key);
        assert_eq!(
            runs.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the expired memo let the second attempt run"
        );
        assert!(
            !negative_memos().lock().await.contains_key(&key),
            "a published sidecar invalidates the memo"
        );
    }

    #[tokio::test]
    async fn an_empty_sidecar_is_rejected_and_negative_memoized() {
        let dir = crate::test_tempdir().expect("cache");
        let file = media_file(dir.path().join("source.mkv"));
        let cached = vtt_path(dir.path(), &file, 0);
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let limits = ExtractionLimits {
            negative_ttl: Duration::from_secs(60),
            ..ExtractionLimits::default()
        };

        let runs_for_first = Arc::clone(&runs);
        let why = ensure_vtt_bounded(dir.path(), &file, 0, limits, move |tmp, _, _| async move {
            runs_for_first.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::fs::write(tmp, b"")
                .await
                .map_err(|error| error.to_string())
        })
        .await
        .expect_err("an empty sidecar is not publishable");
        assert!(why.contains("empty"), "the error identifies empty output");
        assert!(!cached.exists(), "empty output must not be published");

        let runs_for_repeat = Arc::clone(&runs);
        let repeated =
            ensure_vtt_bounded(dir.path(), &file, 0, limits, move |_, _, _| async move {
                runs_for_repeat.fetch_add(100, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
            .await;
        assert!(repeated.is_err(), "the negative memo suppresses a rescan");
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
        forget_failure(&cached).await;
    }

    /// P2-4: the sidecar is re-read whole for every segment request, so a
    /// pathological track must be refused at publish — and refused the way any
    /// other failure is, leaving neither a cache entry nor a temp file.
    #[tokio::test]
    async fn an_oversized_sidecar_is_rejected_and_leaves_no_file() {
        let dir = crate::test_tempdir().expect("cache");
        let file = media_file(dir.path().join("source.mkv"));
        let limits = ExtractionLimits {
            max_sidecar_bytes: 64,
            negative_ttl: Duration::ZERO,
            ..ExtractionLimits::default()
        };

        let why = ensure_vtt_bounded(dir.path(), &file, 0, limits, |tmp, _, _| async move {
            tokio::fs::write(tmp, "WEBVTT\n".to_string() + &"x".repeat(4_096))
                .await
                .map_err(|e| e.to_string())
        })
        .await
        .expect_err("an oversized sidecar is not publishable");
        assert!(why.contains("cap"), "the error names the cap, got {why}");

        assert!(
            tokio::fs::metadata(vtt_path(dir.path(), &file, 0))
                .await
                .is_err(),
            "a rejected sidecar must never appear under its cache name"
        );
        let mut entries = tokio::fs::read_dir(dir.path()).await.expect("read cache");
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            assert!(
                !entry.file_name().to_string_lossy().starts_with(".tmp-"),
                "and its temp file must be gone too"
            );
        }
    }

    /// The interleaving the test above only hits by luck, stated directly.
    ///
    /// A request reads the cache, misses, and reaches the registry a moment
    /// later — by which time the owner has renamed its sidecar into place and
    /// retired its entry. There is no flight left to join, and the stale miss
    /// says nothing is there. Owning a second extraction at that point is the
    /// bug; [`enlist`] must read the cache again under the lock and hand back
    /// the published answer.
    #[tokio::test]
    async fn a_sidecar_published_after_the_cache_read_is_joined_not_re_extracted() {
        let dir = crate::test_tempdir().expect("cache");
        let file = media_file(dir.path().join("source.mkv"));
        let cached = vtt_path(dir.path(), &file, 0);
        tokio::fs::write(&cached, "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nhello\n")
            .await
            .expect("publish");

        // Exactly the state a request finds in that window: sidecar on disk,
        // registry empty for this key.
        assert!(
            !extractions().lock().await.contains_key(&cached),
            "no flight is registered for a retired extraction"
        );
        assert!(
            matches!(enlist(&cached, MAX_SIDECAR_BYTES).await, Flight::Published),
            "a published sidecar must never be re-owned"
        );
        assert!(
            !extractions().lock().await.contains_key(&cached),
            "and deciding must not leave a flight behind"
        );

        // The other two arms still work: an unpublished key is owned once and
        // joined thereafter.
        let missing = vtt_path(dir.path(), &file, 1);
        assert!(matches!(
            enlist(&missing, MAX_SIDECAR_BYTES).await,
            Flight::Own(_)
        ));
        assert!(matches!(
            enlist(&missing, MAX_SIDECAR_BYTES).await,
            Flight::Join(_)
        ));
        extractions().lock().await.remove(&missing);
    }
}
