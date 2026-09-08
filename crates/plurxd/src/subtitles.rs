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
#[cfg(test)]
pub const WINDOW_SECONDS_DEFAULT: i64 = plurx_core::store::DEFAULT_SUBTITLE_WINDOW_SECS;
/// Below this a window dies inside one AVPlayer retry cycle; above it the
/// extraction stops beating the whole-track warm on a slow source and the
/// bridge loses its point.
pub const WINDOW_SECONDS_MIN: i64 = plurx_core::store::MIN_SUBTITLE_WINDOW_SECS;
pub const WINDOW_SECONDS_MAX: i64 = plurx_core::store::MAX_SUBTITLE_WINDOW_SECS;

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
/// **This is a no-op on every build this server ships against, and that is the
/// point.** Measured on `-i src -ss A -to B`: ffmpeg 7.0.2 and 6.1.1 both
/// return absolute cue times, and 4.4.2 returns them rebased to zero. The
/// Dockerfile installs `jellyfin-ffmpeg7` and fails the build unless it
/// carries `dovi_rpu`, which needs 7.1+, so a running fleet is always on a
/// build where the times are already absolute and this function returns its
/// input untouched.
///
/// It exists because that install is deliberately unpinned — the Dockerfile's
/// own note says "WHICH ffmpeg lands here depends on the day the image was
/// built" — and a base change would otherwise be silent and selective:
/// windows at the head of a file work under either base, so a smoke test
/// passes while every later window serves a cue-less segment logged as a
/// success. One scan of the cue lines buys immunity from that.
///
/// Detection is exact rather than heuristic. A rebased window's cues lie in
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

/// What one playback owns while a subtitle window is being extracted for it.
///
/// [`warmups`] above deduplicates *work* by cache key; this registry bounds
/// *ownership* by playback. A seek storm produces a run of different anchors
/// for one session, and every one of them is a legitimate distinct key, so the
/// key-shaped registries cannot answer the question M7 actually asks: how many
/// ffmpeg children does one viewer have running at once? The answer has to be
/// one, and it has to be physical rather than hopeful — which is why this slot
/// carries a way to stop the real extraction and a way to wait for it to be
/// gone before a replacement starts.
struct SessionWindow {
    /// Distinguishes this flight from a successor claiming the same session, so
    /// a task that finishes after it was displaced cannot remove the slot that
    /// replaced it.
    id: u64,
    /// The control sequence that justified this flight. `None` is a first play:
    /// the client has not settled anywhere yet, so there is no ordering fact to
    /// compare a later request against.
    sequence: Option<u64>,
    anchor_seconds: i64,
    window_seconds: i64,
    /// Dropping or sending this aborts the flight before it publishes.
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    /// Closed once the flight has finished tearing down — the child is dead,
    /// the temp file is unlinked and the registries are clear. Waiting on it
    /// before spawning a successor is the whole difference between "one live
    /// producer" and "one producer we have stopped waiting for".
    settled: Arc<tokio::sync::Semaphore>,
}

/// Live window owners, and the sessions whose owner was just released.
#[derive(Default)]
struct WindowOwners {
    live: HashMap<String, SessionWindow>,
    /// When each recently released session stops being fenced.
    ///
    /// Releasing is an instant, but a segment request that read its authority
    /// just before teardown can still be on its way to claiming a slot. Without
    /// a fence that request installs an owner nothing will ever release: an
    /// ffmpeg with no viewer, and a slot that makes the next playback to reuse
    /// this durable id look superseded by an ordering fact from a session that
    /// no longer exists.
    released: HashMap<String, std::time::Instant>,
    /// Per session: window flights alive right now, and the most that were ever
    /// alive at once.
    ///
    /// A flight is alive from the moment its task starts to the moment its
    /// guard drops — which is *after* the child is dead, the temp file is gone
    /// and the registries are clear. That is the span the settlement wait
    /// exists to keep from overlapping, and it is wider than the span a
    /// producer-side counter can see, so this is where the "one live flight per
    /// playback" claim is observable at all. Test-only: the production path
    /// pays nothing for it.
    #[cfg(test)]
    flights: HashMap<String, (usize, usize)>,
}

/// How long a released session refuses to start new window work.
///
/// Long enough to outlast a segment request that was already in flight when
/// teardown ran, short enough that a genuine resurrection of the same durable
/// id gets its bridge back within one viewer's patience.
const WINDOW_RELEASE_FENCE: Duration = Duration::from_secs(30);

/// Fenced sessions remembered at once. A fence is an id and a deadline, so the
/// cap is about refusing unbounded growth rather than saving bytes.
const MAX_FENCED_SESSIONS: usize = 256;

/// The owner registry is a *synchronous* mutex on purpose.
///
/// Every critical section here is a map lookup and a map write with no I/O in
/// it, and the one operation that must wait — settlement — deliberately waits
/// outside the lock. Making it synchronous also makes the destructor below
/// possible, and a `std::sync::MutexGuard` held across an `await` is a compile
/// error rather than a deadlock discovered in production.
type SessionWindows = std::sync::Mutex<WindowOwners>;

fn session_windows() -> &'static SessionWindows {
    static SESSION_WINDOWS: OnceLock<SessionWindows> = OnceLock::new();
    SESSION_WINDOWS.get_or_init(|| std::sync::Mutex::new(WindowOwners::default()))
}

fn window_owners() -> std::sync::MutexGuard<'static, WindowOwners> {
    session_windows()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn next_window_flight_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl WindowOwners {
    /// Whether this session is inside its post-release fence.
    fn fenced(&self, session: &str, now: std::time::Instant) -> bool {
        self.released
            .get(session)
            .is_some_and(|expires_at| *expires_at > now)
    }

    /// Fence a session, evicting expired entries first and then the fence
    /// closest to expiry — the one whose loss costs the least.
    fn fence(&mut self, session: &str, now: std::time::Instant) {
        self.released.retain(|_, expires_at| *expires_at > now);
        while self.released.len() >= MAX_FENCED_SESSIONS {
            let Some(soonest) = self
                .released
                .iter()
                .min_by_key(|(_, expires_at)| **expires_at)
                .map(|(session, _)| session.clone())
            else {
                break;
            };
            self.released.remove(&soonest);
        }
        self.released
            .insert(session.to_owned(), now + WINDOW_RELEASE_FENCE);
    }
}

/// Owns one flight's claim on a session slot for as long as that flight exists.
///
/// Every way out has to reach the same two facts: this session no longer holds
/// this flight, and whoever is waiting for it to settle is released. A tail
/// statement reaches them on the paths its author thought of; a destructor
/// reaches them when the task panics, when the runtime drops it at shutdown,
/// and when a cancelled request abandons a slot it had already claimed. The
/// cost of missing one of those is a `release_session_window` that waits
/// forever, holding a session's terminal cleanup behind it.
struct WindowFlightGuard {
    session: String,
    id: u64,
    settled: Arc<tokio::sync::Semaphore>,
}

impl Drop for WindowFlightGuard {
    fn drop(&mut self) {
        {
            let mut owners = window_owners();
            if owners
                .live
                .get(&self.session)
                .is_some_and(|live| live.id == self.id)
            {
                owners.live.remove(&self.session);
            }
            #[cfg(test)]
            if let Some(counts) = owners.flights.get_mut(&self.session) {
                counts.0 = counts.0.saturating_sub(1);
            }
        }
        // Last, so anything woken by it finds the registry already clear.
        self.settled.close();
    }
}

/// The error a joiner sees when the flight it was waiting on was superseded.
///
/// A window is best effort by construction — the caller already has a correct
/// empty answer to serve — so this reads as "no window", and deliberately
/// leaves no negative memo behind: nothing was wrong with the extraction except
/// where the viewer went.
const WINDOW_SUPERSEDED: &str = "subtitle window extraction was superseded";

/// Whether a window request carries a newer ordering fact than the flight a
/// playback already owns.
///
/// A request with an authority outranks one started without any, because a
/// settled destination is a fact and a first play is the absence of one. Equal
/// sequences never displace: the client has not moved, so two anchors under one
/// sequence are two views of the same destination and the flight already
/// running is as good as its replacement.
fn later_window_authority(requested: Option<u64>, live: Option<u64>) -> bool {
    match (requested, live) {
        (Some(requested), Some(live)) => requested > live,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// Wait for a displaced flight to finish tearing down.
async fn await_window_settlement(settled: &tokio::sync::Semaphore) {
    // `close()` is the completion signal, so the acquire failing is the case
    // this function exists to observe.
    let _ = settled.acquire().await;
}

/// Release the window owner a playback holds, and fence the session against
/// acquiring another.
///
/// Cancellation-safe in the sense session teardown needs: the fence is set, the
/// slot removed and the cancel signalled before the first await, so a caller
/// that is itself dropped mid-release still leaves the flight stopping rather
/// than orphaned and still owning a session id a later playback may reuse.
///
/// The fence is why this is not merely a sweep. A segment request that read its
/// authority a moment before teardown is still entitled to claim a slot, and
/// without the fence it would claim one after the only thing that would ever
/// release it had already run.
pub async fn release_session_window(session: &str) {
    let live = {
        let mut owners = window_owners();
        owners.fence(session, std::time::Instant::now());
        owners.live.remove(session)
    };
    stop_window_flight(live).await;
}

/// Which flight this session holds right now, if any.
///
/// The caller that wants to stop a *particular* flight has to name it, because
/// between reading and stopping it the session may legitimately have started
/// another. Synchronous on purpose: it is meant to be read inside the same
/// guard that decides the flight is obsolete.
pub fn session_window_flight(session: &str) -> Option<u64> {
    window_owners().live.get(session).map(|live| live.id)
}

/// Stop one named flight without ending the session that owns it.
///
/// Reattachment moves a viewer to a different rendition under the same session
/// id. The flight they leave behind is a real ffmpeg extracting a span for the
/// recipe they just left, so it is worth stopping — but
/// [`release_session_window`] is the wrong instrument. Releasing also fences
/// the id for [`WINDOW_RELEASE_FENCE`], and this id belongs to a viewer who is
/// still watching: the fence would refuse the first window of the attachment
/// that replaced it, turning a recipe change into half a minute without
/// subtitles.
///
/// `flight` is the identity read when the flight was judged obsolete. A
/// successor claiming the same session between that read and this call carries
/// a different id and is left alone, which is the same guarantee
/// [`WindowFlightGuard`] relies on for its own removal.
pub async fn abandon_session_window(session: &str, flight: u64) {
    let live = {
        let mut owners = window_owners();
        match owners.live.get(session) {
            Some(live) if live.id == flight => owners.live.remove(session),
            _ => None,
        }
    };
    stop_window_flight(live).await;
}

/// Cancel and wait out a flight taken out of the registry.
///
/// Cancellation-safe in the sense session teardown needs: the slot is already
/// gone and the cancel is signalled before the first await, so a caller that is
/// itself dropped mid-stop still leaves the flight stopping rather than
/// orphaned and still owning a session id a later playback may reuse.
async fn stop_window_flight(live: Option<SessionWindow>) {
    let Some(mut live) = live else {
        return;
    };
    drop(live.cancel.take());
    await_window_settlement(&live.settled).await;
}

/// How a window extraction ended.
///
/// The whole-track path has no use for this distinction — it cannot be
/// cancelled — but the window path turns entirely on it: a failure is
/// remembered so a broken track is not rescanned every six seconds, while an
/// abortion must leave no trace at all, because the next request for the same
/// span is not a retry of something that went wrong.
enum WindowExtractionOutcome {
    Complete(Result<PathBuf, String>),
    Aborted,
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

/// What the sidecar cache can say about one track right now, without doing
/// anything about it.
///
/// Every arm is a fact the control plane can report to a client so it stops
/// guessing when to re-fetch an empty subtitle segment. Deliberately
/// side-effect free: it launches no extraction, enlists no warmer, and writes
/// no memo. A readiness probe that started work would make every control
/// exchange a reason to spawn ffmpeg, which is the opposite of what bounded
/// materialization is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidecarState {
    /// Bytes are on disk and servable now.
    Ready,
    /// An extraction or warmer is in flight for this key.
    Warming,
    /// The last attempt failed and its memo has not expired. A client should
    /// stop retrying rather than poll a key this server has given up on.
    Failed,
    /// Nothing is cached and nothing is running. From the client's side this
    /// is indistinguishable from `Warming` — both mean "not yet" — but they
    /// are different facts about the server and only one of them is progress.
    Absent,
}

/// Probe the cache for one track. See [`SidecarState`] for why this starts
/// nothing.
pub async fn sidecar_state(dir: &Path, file: &MediaFile, index: i64) -> SidecarState {
    let cached = vtt_path(dir, file, index);
    if matches!(read_vtt_path(&cached, MAX_SIDECAR_BYTES).await, Ok(Some(_))) {
        return SidecarState::Ready;
    }
    // Order matters below `Ready`: a published sidecar outranks any memory of
    // failing, which is why `forget_failure` exists, and an in-flight attempt
    // outranks a stale memo because it is the thing that will settle the key.
    if extractions().lock().await.contains_key(&cached) || warmups().lock().await.contains(&cached)
    {
        return SidecarState::Warming;
    }
    if remembered_failure(&cached).await.is_some() {
        return SidecarState::Failed;
    }
    SidecarState::Absent
}

/// Probe the whole-track cache and the exact forward window serving one
/// demand anchor. Like [`sidecar_state`], this is observation only: a control
/// exchange must never become a reason to start ffmpeg.
pub async fn sidecar_state_for_demand(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    anchor_seconds: i64,
    window_seconds: i64,
) -> SidecarState {
    let whole_state = sidecar_state(dir, file, index).await;
    if whole_state == SidecarState::Ready {
        return SidecarState::Ready;
    }

    let anchor = window_anchor_seconds(anchor_seconds, window_seconds);
    let window = vtt_window_path(dir, file, index, anchor, window_seconds);
    if matches!(read_vtt_path(&window, MAX_SIDECAR_BYTES).await, Ok(Some(_))) {
        return SidecarState::Ready;
    }

    // Only the whole track and this demand's exact grid window count as
    // progress. A neighbouring window is real work for another position, not
    // evidence that this one is becoming servable.
    if whole_state == SidecarState::Warming {
        return SidecarState::Warming;
    }
    if extractions().lock().await.contains_key(&window) || warmups().lock().await.contains(&window)
    {
        return SidecarState::Warming;
    }
    if whole_state == SidecarState::Failed {
        return SidecarState::Failed;
    }
    SidecarState::Absent
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
    warm_vtt_with(dir, file, index, |tmp, file, index| async move {
        extract_vtt(&tmp, &file, index).await
    })
    .await;
}

/// The whole-track warmer seam used by deterministic HTTP boundary tests.
/// The injected producer still runs behind the production warmup and
/// extraction registries, so cancellation and single-flight semantics remain
/// part of the exercised path.
pub(crate) async fn warm_vtt_with<F, Fut>(dir: &Path, file: &MediaFile, index: i64, extract: F)
where
    F: FnOnce(PathBuf, MediaFile, i64) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
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
        let _ = ensure_vtt_with(&dir, &file, index, extract).await;
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

    join_flight(&flight).await
}

/// Wait for whoever owns this key to publish an answer.
async fn join_flight(flight: &Extraction) -> Result<PathBuf, String> {
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

/// Open the cache directory a window is about to publish into.
///
/// `Ok(None)` means the sidecar is already there and nothing needs starting.
async fn prepare_window_cache(
    cached: &Path,
    dir: &Path,
    limits: ExtractionLimits,
) -> Result<Option<plurx_core::fs_secure::SecureDirectory>, String> {
    if valid_sidecar(cached, limits.max_sidecar_bytes).await {
        return Ok(None);
    }
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|error| format!("creating subtitle cache: {error}"))?;
    plurx_core::fs_secure::SecureDirectory::open(dir)
        .await
        .map(Some)
        .map_err(|error| format!("opening subtitle cache: {error}"))
}

/// The window path's owned extraction.
///
/// [`ensure_vtt_at`] deliberately detaches the real work: a whole-track sidecar
/// is what every other consumer needs, so a client that walks away must not
/// stop it. A window is the opposite. It exists for one playback's current
/// destination, and when that destination moves the extraction is waste that
/// holds a stalled mount open. So this runs the extraction inside the caller's
/// task, where dropping it reaches the ffmpeg child, and reports abortion as a
/// different outcome from failure.
async fn ensure_window_owned<F, Fut>(
    cached: PathBuf,
    dir: &Path,
    file: &MediaFile,
    index: i64,
    limits: ExtractionLimits,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
    extract: F,
) -> WindowExtractionOutcome
where
    F: FnOnce(PathBuf, MediaFile, i64) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    // The cache directory is server-local state rather than the media mount, so
    // these are cheap — but they are still I/O, and a supersession arriving
    // during them should not have to wait for them. Cancelling here is also
    // strictly before `enlist`, so an abort at this point leaves no flight
    // registered for anyone to join.
    let prepared = tokio::select! {
        biased;
        _ = &mut cancel => return WindowExtractionOutcome::Aborted,
        prepared = prepare_window_cache(&cached, dir, limits) => prepared,
    };
    let directory = match prepared {
        Ok(Some(directory)) => directory,
        Ok(None) => return WindowExtractionOutcome::Complete(Ok(cached)),
        Err(why) => return WindowExtractionOutcome::Complete(Err(why)),
    };
    let (flight, owner) = match enlist(&cached, limits.max_sidecar_bytes).await {
        Flight::Published => return WindowExtractionOutcome::Complete(Ok(cached)),
        Flight::Remembered(why) => return WindowExtractionOutcome::Complete(Err(why)),
        Flight::Join(flight) => (flight, false),
        Flight::Own(flight) => (flight, true),
    };

    if !owner {
        // A flight this playback does not own — another session's warm, or a
        // request awaiting the same key. Supersession may stop this playback
        // waiting for it; it may never kill it.
        return tokio::select! {
            biased;
            _ = cancel => WindowExtractionOutcome::Aborted,
            result = join_flight(&flight) => WindowExtractionOutcome::Complete(result),
        };
    }

    let tmp = dir.join(format!(".tmp-{}.vtt", uuid::Uuid::new_v4()));
    let started = std::time::Instant::now();
    tracing::info!(
        file_id = file.id,
        index,
        "extracting an embedded text subtitle window to the sidecar cache"
    );
    let outcome = publish_extraction_with_cancel(
        &directory,
        &cached,
        &tmp,
        file,
        index,
        limits,
        Some(cancel),
        extract,
    )
    .await;
    match &outcome {
        WindowExtractionOutcome::Complete(Ok(_)) => {
            tracing::info!(
                file_id = file.id,
                index,
                elapsed_ms = started.elapsed().as_millis(),
                "windowed text subtitle sidecar cached"
            );
            forget_failure(&cached).await;
        }
        WindowExtractionOutcome::Complete(Err(why)) => {
            tracing::warn!(
                file_id = file.id,
                index,
                elapsed_ms = started.elapsed().as_millis(),
                why,
                "windowed text subtitle extraction failed; suppressing retries for now"
            );
            remember_failure(&cached, why, limits.negative_ttl).await;
        }
        WindowExtractionOutcome::Aborted => {
            tracing::debug!(
                file_id = file.id,
                index,
                elapsed_ms = started.elapsed().as_millis(),
                "abandoning a windowed subtitle extraction the viewer left behind"
            );
        }
    }
    // Joiners are released either way. An abortion answers them with the
    // superseded marker rather than leaving them parked on a flight that will
    // never publish, and writes no memo, so the next request for this span is a
    // first attempt rather than a suppressed retry.
    let published = match &outcome {
        WindowExtractionOutcome::Complete(result) => result.clone(),
        WindowExtractionOutcome::Aborted => Err(WINDOW_SUPERSEDED.to_owned()),
    };
    *flight.result.lock().await = Some(published);
    extractions().lock().await.remove(&cached);
    flight.ready.notify_waiters();
    outcome
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
///
/// `sequence` is the control sequence the playback has settled on, when it has
/// settled on one. It is the only thing that lets a later anchor displace an
/// earlier flight: without an ordering fact, a second anchor is just a second
/// request, and the work already running is as likely to be the right work.
pub async fn warm_vtt_window(
    session: &str,
    sequence: Option<u64>,
    dir: &Path,
    file: &MediaFile,
    index: i64,
    anchor_seconds: i64,
    window_seconds: i64,
) -> bool {
    warm_vtt_window_with(
        session,
        sequence,
        dir,
        file,
        index,
        anchor_seconds,
        window_seconds,
        |tmp, file, index, anchor_seconds, window_seconds| async move {
            extract_vtt_window(&tmp, &file, index, anchor_seconds, window_seconds).await
        },
    )
    .await
}

/// The window-warmer seam used by the HTTP boundary fixture.
///
/// Keeping the injected producer behind the same warmup, extraction and
/// ownership registries is deliberate: the fixture must prove the production
/// single-flight behavior, not a test double's imitation of it.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn warm_vtt_window_with<F, Fut>(
    session: &str,
    sequence: Option<u64>,
    dir: &Path,
    file: &MediaFile,
    index: i64,
    anchor_seconds: i64,
    window_seconds: i64,
    extract: F,
) -> bool
where
    F: FnOnce(PathBuf, MediaFile, i64, i64, i64) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
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

    // Ownership is decided before work is deduplicated. "May this playback
    // start anything" is a question about ordering; "has somebody already
    // started this exact span" is a question about waste. Answering them in
    // this order is what stops a storm from leaving a trail of live children.
    let mut displaced: Option<SessionWindow> = None;
    let (cancel_rx, guard) = loop {
        if let Some(mut live) = displaced.take() {
            drop(live.cancel.take());
            await_window_settlement(&live.settled).await;
        }
        let mut owners = window_owners();
        if owners.fenced(session, std::time::Instant::now()) {
            // This playback has ended. A request that read its authority just
            // before teardown is not entitled to leave an extraction behind it.
            return false;
        }
        match owners.live.get(session) {
            Some(live)
                if live.anchor_seconds == anchor_seconds
                    && live.window_seconds == window_seconds =>
            {
                // The same destination this playback is already extracting.
                // Join it; never spawn a second extractor for it.
                return true;
            }
            Some(live) if !later_window_authority(sequence, live.sequence) => {
                // A different anchor with nothing newer behind it is not
                // evidence the live flight is wrong, and killing it would turn
                // an ordinary segment walk into a restart loop.
                return false;
            }
            Some(_) => {
                // Newer ordering fact, different destination. Take the slot
                // out of the registry now so nothing else adopts it, then wait
                // for the real producer to settle outside this lock.
                displaced = owners.live.remove(session);
                continue;
            }
            None => {}
        }
        let id = next_window_flight_id();
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let settled = Arc::new(tokio::sync::Semaphore::new(0));
        owners.live.insert(
            session.to_owned(),
            SessionWindow {
                id,
                sequence,
                anchor_seconds,
                window_seconds,
                cancel: Some(cancel_tx),
                settled: Arc::clone(&settled),
            },
        );
        // Constructed under the same lock that installed the slot, so from
        // here on there is no path — return, panic or task drop — that leaves
        // the slot claimed or a waiter parked.
        break (
            cancel_rx,
            WindowFlightGuard {
                session: session.to_owned(),
                id,
                settled,
            },
        );
    };

    // Cross-session dedup underneath the per-playback slot: a different
    // playback already extracting this exact span is work to join rather than
    // repeat, and it is not a flight this one may cancel. Dropping the guard
    // releases the slot this request claimed a moment ago.
    if !warmups().lock().await.insert(cached.clone()) {
        drop(guard);
        return true;
    }

    let dir_owned = dir.to_owned();
    let file_owned = file.clone();
    let window = bounded_window_seconds(window_seconds);
    let counted_session = session.to_owned();
    tokio::spawn(async move {
        // Moved in so the slot outlives exactly this task and no longer.
        let _guard = guard;
        #[cfg(test)]
        {
            let mut owners = window_owners();
            let counts = owners.flights.entry(counted_session).or_insert((0, 0));
            counts.0 += 1;
            counts.1 = counts.1.max(counts.0);
        }
        #[cfg(not(test))]
        let _ = counted_session;
        let _ = ensure_window_owned(
            cached.clone(),
            &dir_owned,
            &file_owned,
            index,
            ExtractionLimits::default(),
            cancel_rx,
            move |tmp, file, index| async move {
                extract(tmp, file, index, anchor_seconds, window).await
            },
        )
        .await;
        warmups().lock().await.remove(&cached);
    });
    true
}

/// The window flight a playback owns right now: its anchor, its span and the
/// control sequence that justified it.
///
/// The acceptance needs to say *which* target the surviving producer carries,
/// not merely that one survived, and that fact lives in this registry rather
/// than in anything the HTTP boundary returns.
#[cfg(test)]
pub(crate) fn owned_window_for_test(session: &str) -> Option<(i64, i64, Option<u64>)> {
    window_owners()
        .live
        .get(session)
        .map(|live| (live.anchor_seconds, live.window_seconds, live.sequence))
}

/// Lift the post-release fence so a test can reuse a session id it just ended.
#[cfg(test)]
pub(crate) fn clear_release_fence_for_test(session: &str) {
    window_owners().released.remove(session);
}

/// The most window flights this playback ever had alive at one time.
///
/// The whole point of waiting for a displaced flight to settle rather than
/// dropping its handle is that this can never exceed one.
#[cfg(test)]
pub(crate) fn peak_window_flights_for_test(session: &str) -> usize {
    window_owners()
        .flights
        .get(session)
        .map(|counts| counts.1)
        .unwrap_or(0)
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
    match publish_extraction_with_cancel(directory, cached, tmp, file, index, limits, None, extract)
        .await
    {
        WindowExtractionOutcome::Complete(result) => result,
        // Nothing was passed that could abort this call. Answering with the
        // superseded marker keeps an impossible case from becoming a panic in
        // production, and a caller that somehow reached it reads "no sidecar",
        // which is the truth.
        WindowExtractionOutcome::Aborted => Err(WINDOW_SUPERSEDED.to_owned()),
    }
}

/// Publish an extraction, optionally letting a supersession stop it.
///
/// The cancellation point is deliberately only around the extractor. Once
/// bytes exist, validation and the atomic publish run to completion: a window
/// that has already paid for the scan is worth publishing even for a
/// destination the viewer has left — some later request may want it — and a
/// cancel point in the middle of `atomic_write_child` would be a way to strand
/// a temp file rather than a way to save work.
#[allow(clippy::too_many_arguments)]
async fn publish_extraction_with_cancel<F, Fut>(
    directory: &plurx_core::fs_secure::SecureDirectory,
    cached: &Path,
    tmp: &Path,
    file: &MediaFile,
    index: i64,
    limits: ExtractionLimits,
    cancel: Option<tokio::sync::oneshot::Receiver<()>>,
    extract: F,
) -> WindowExtractionOutcome
where
    F: FnOnce(PathBuf, MediaFile, i64) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    macro_rules! bail {
        ($why:expr) => {
            return WindowExtractionOutcome::Complete(Err($why))
        };
    }
    let Some(cached_name) = cached.file_name().and_then(|name| name.to_str()) else {
        bail!("subtitle cache path has no safe filename".to_owned())
    };
    let Some(tmp_name) = tmp.file_name().and_then(|name| name.to_str()) else {
        bail!("subtitle temp path has no safe filename".to_owned())
    };
    // A timeout here rather than around the whole task: expiring drops the
    // extractor future (killing its child) while this frame still owns the
    // temp file and can delete it, so a wedge leaves the cache dir as clean as
    // an ordinary failure does.
    let extraction =
        tokio::time::timeout(limits.timeout, extract(tmp.to_owned(), file.clone(), index));
    let extracted = match cancel {
        Some(cancel) => {
            // The cleanup deliberately happens *after* the select expression
            // rather than inside its arm: `select!` drops the losing future
            // when the whole expression completes, so unlinking from inside the
            // arm would remove the temp file while ffmpeg still held it open.
            // Ending the select first drops `extraction`, which is what
            // `kill_on_drop` turns into an actual kill.
            let raced = tokio::select! {
                biased;
                _ = cancel => None,
                extracted = extraction => Some(extracted),
            };
            match raced {
                Some(extracted) => extracted,
                None => {
                    // The child is dead and this frame still owns the temp
                    // file, so an abandoned window leaves the cache directory
                    // exactly as clean as an ordinary failure leaves it — but
                    // with no memo, because nothing was wrong with the
                    // extraction except its destination.
                    let _ = directory.unlink_child(tmp_name).await;
                    return WindowExtractionOutcome::Aborted;
                }
            }
        }
        None => extraction.await,
    };
    let extracted = match extracted {
        Ok(extracted) => extracted,
        Err(_) => Err(format!(
            "subtitle extraction timed out after {}s",
            limits.timeout.as_secs()
        )),
    };
    if let Err(e) = extracted {
        let _ = directory.unlink_child(tmp_name).await;
        bail!(e);
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
            bail!(format!("reading extracted subtitle sidecar: {error}"));
        }
    };
    let size = bytes.len() as u64;
    if size == 0 || size > limits.max_sidecar_bytes {
        let _ = directory.unlink_child(tmp_name).await;
        if size == 0 {
            bail!("subtitle extraction produced an empty sidecar".to_owned());
        }
        bail!(format!(
            "subtitle sidecar is {size} bytes, above the {} byte cap",
            limits.max_sidecar_bytes
        ));
    }
    // A whole-track sidecar is authoritative. If it won while this window was
    // extracting, do not resurrect disposable bytes after the whole-track
    // publisher pruned them.
    if let Some(whole_name) = whole_name_for_window(cached_name) {
        if let Some(parent) = cached.parent() {
            let whole = parent.join(&whole_name);
            if valid_sidecar(&whole, MAX_SIDECAR_BYTES).await {
                let _ = directory.unlink_child(tmp_name).await;
                return WindowExtractionOutcome::Complete(Ok(whole));
            }
        }
    }
    if let Err(error) = directory.atomic_write_child(cached_name, &bytes).await {
        let _ = directory.unlink_child(tmp_name).await;
        bail!(format!("publishing subtitle cache: {error}"));
    }
    let _ = directory.unlink_child(tmp_name).await;
    if let Some(dir) = cached.parent() {
        if !is_window_name(cached_name) {
            prune_matching_windows(directory, dir, cached_name).await;
        }
        prune(dir).await;
    }
    WindowExtractionOutcome::Complete(Ok(cached.to_owned()))
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

fn whole_name_for_window(name: &str) -> Option<String> {
    if !is_window_name(name) {
        return None;
    }
    let stem = name.strip_suffix(".vtt")?;
    let (head, _) = stem.rsplit_once('-')?;
    let (whole, anchor) = head.rsplit_once('-')?;
    anchor.strip_prefix('w')?.parse::<i64>().ok()?;
    Some(format!("{whole}.vtt"))
}

async fn prune_matching_windows(
    directory: &plurx_core::fs_secure::SecureDirectory,
    dir: &Path,
    whole_name: &str,
) {
    let Some(stem) = whole_name.strip_suffix(".vtt") else {
        return;
    };
    let prefix = format!("{stem}-w");
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && is_window_name(&name) {
            let _ = directory.unlink_child(&name).await;
        }
    }
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
    /// Measured on `-i src -ss A -to B`: ffmpeg 7.0.2 and 6.1.1 return
    /// absolute cue times, 4.4.2 returns them rebased to zero. The fleet runs
    /// 7.1+ — the Dockerfile fails the build without `dovi_rpu` — so the
    /// shipped path is the absolute one and normalization is a no-op there.
    /// Two reviewers measuring on different builds reached opposite
    /// conclusions, which is why the code no longer depends on either.
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
            &window
                .file_name()
                .expect("window path has a file name")
                .to_string_lossy()
        ));
        assert!(!super::is_window_name(
            &whole
                .file_name()
                .expect("whole-track path has a file name")
                .to_string_lossy()
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
    async fn subtitle_readiness_names_only_the_window_that_can_serve_it() {
        let dir = crate::test_tempdir().expect("cache");
        let file = media_file(dir.path().join("source.mkv"));
        let whole = vtt_path(dir.path(), &file, 0);
        let wanted = vtt_window_path(dir.path(), &file, 0, 0, 200);
        let unrelated = vtt_window_path(dir.path(), &file, 0, 200, 200);

        assert_eq!(
            sidecar_state_for_demand(dir.path(), &file, 0, 12, 200).await,
            SidecarState::Absent
        );

        tokio::fs::write(&unrelated, b"WEBVTT\n\nunrelated\n")
            .await
            .expect("unrelated window");
        assert_eq!(
            sidecar_state_for_demand(dir.path(), &file, 0, 12, 200).await,
            SidecarState::Absent,
            "a neighbouring window cannot make this demand ready"
        );

        warmups().lock().await.insert(whole.clone());
        assert_eq!(
            sidecar_state_for_demand(dir.path(), &file, 0, 12, 200).await,
            SidecarState::Warming
        );
        warmups().lock().await.remove(&whole);

        extractions().lock().await.insert(
            wanted.clone(),
            Arc::new(Extraction {
                result: tokio::sync::Mutex::new(None),
                ready: tokio::sync::Notify::new(),
            }),
        );
        assert_eq!(
            sidecar_state_for_demand(dir.path(), &file, 0, 12, 200).await,
            SidecarState::Warming
        );
        extractions().lock().await.remove(&wanted);

        remember_failure(&whole, "failed", Duration::from_secs(60)).await;
        assert_eq!(
            sidecar_state_for_demand(dir.path(), &file, 0, 12, 200).await,
            SidecarState::Failed
        );
        forget_failure(&whole).await;

        tokio::fs::write(&wanted, b"WEBVTT\n\n00:00:12.000 --> 00:00:13.000\nready\n")
            .await
            .expect("wanted window");
        assert_eq!(
            sidecar_state_for_demand(dir.path(), &file, 0, 12, 200).await,
            SidecarState::Ready
        );
        tokio::fs::remove_file(&wanted)
            .await
            .expect("remove window");

        tokio::fs::write(&whole, b"WEBVTT\n\nwhole\n")
            .await
            .expect("whole track");
        assert_eq!(
            sidecar_state_for_demand(dir.path(), &file, 0, 12, 200).await,
            SidecarState::Ready
        );
    }

    #[tokio::test]
    async fn whole_track_publication_prunes_only_its_fingerprint_windows() {
        let dir = crate::test_tempdir().expect("cache");
        let file = media_file(dir.path().join("source.mkv"));
        let mut other = file.clone();
        other.mtime += 1;
        let first = vtt_window_path(dir.path(), &file, 0, 0, 200);
        let second = vtt_window_path(dir.path(), &file, 0, 200, 200);
        let other_window = vtt_window_path(dir.path(), &other, 0, 0, 200);
        for path in [&first, &second, &other_window] {
            tokio::fs::write(path, b"WEBVTT\n\nwindow\n")
                .await
                .expect("window sidecar");
        }

        ensure_vtt_with(dir.path(), &file, 0, |tmp, _, _| async move {
            tokio::fs::write(tmp, b"WEBVTT\n\nwhole\n")
                .await
                .map_err(|error| error.to_string())
        })
        .await
        .expect("publish whole track");

        assert!(tokio::fs::metadata(&first).await.is_err());
        assert!(tokio::fs::metadata(&second).await.is_err());
        assert!(
            tokio::fs::metadata(&other_window).await.is_ok(),
            "another source fingerprint keeps its windows"
        );
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
