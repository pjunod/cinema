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
// A queue wait must finish before the surrounding sidecar producer's 600 s
// timeout, so it can report an active remote owner without racing that timer.
const CLUSTER_WAIT_TIMEOUT: Duration = Duration::from_secs(590);

/// How long a failure is remembered. AVPlayer asks for a VTT segment roughly
/// every segment duration (~6 s), and before this memo each of those requests
/// relaunched a full-source read of a file that had just failed. Two minutes
/// turns that storm into one attempt per window — about a twentieth of the
/// scans — while still being short enough that fixing the cause (remounting
/// the NAS, replacing the file) is picked up inside a viewer's patience
/// rather than needing a restart.
const NEGATIVE_TTL: Duration = Duration::from_secs(120);
/// Internal flight result: all waiters must see the same no-cues answer, but
/// it is a success and must not enter the negative failure memo.
const EMPTY_BURN_RESULT: &str = "subtitle cluster burn track has no cues";

/// Failures remembered at once. A memo is a path, a deadline and a short
/// message, so the cap is about refusing unbounded growth rather than saving
/// bytes: a library full of broken tracks must not turn the memo into a leak.
const MAX_NEGATIVE_ENTRIES: usize = 128;

/// How long a caller **that has a deadline of its own** waits for a sidecar
/// another task is producing.
///
/// Deliberately far below any start budget, because the two outcomes are far
/// apart and there is nothing in between: a published sidecar resolves on the
/// first poll, and a cold one needs a full demux of the source. Extracting one
/// PGS track from file 5208 — a 79.5 GB remux whose subtitle packets are
/// interleaved across 116 minutes — read the entire file to produce 18,866
/// bytes, measured at 402 s on m6, which is just the disk's read speed. No
/// start budget can contain that, so a start must not spend its budget
/// discovering it.
///
/// **Only the session start uses this.** The other callers of this module —
/// offline restore, offline package production, and the subtitle VTT endpoint
/// — have no deadline to protect and map any error to something terminal
/// (410 Gone, a failed download, 500). For them a slow success must stay a
/// success, so they keep [`SIDECAR_JOIN_UNBOUNDED`].
pub(crate) const SIDECAR_JOIN_BUDGET: Duration = Duration::from_secs(5);

/// The budget of a caller with nothing to protect: long enough that it can
/// only be reached after the extraction's own [`EXTRACTION_TIMEOUT`] has
/// already published an answer, so these callers wait exactly as long as they
/// did before the budget existed.
pub(crate) const SIDECAR_JOIN_UNBOUNDED: Duration =
    Duration::from_secs(EXTRACTION_TIMEOUT.as_secs() + 60);

/// Marks a refusal that means "this artifact is being built; ask again".
///
/// It is a distinct class from an extraction *failure* on purpose. A failure
/// is about this track and is remembered by the negative memo; this says
/// nothing is wrong at all, and the only honest response is to wait. The HTTP
/// layer maps it to the `startup_timeout` code the create-retry ladder on
/// every client already knows.
pub(crate) const SIDECAR_PENDING_PREFIX: &str = "subtitle sidecar is still being built: ";

fn sidecar_pending_error(message: impl AsRef<str>) -> String {
    format!("{SIDECAR_PENDING_PREFIX}{}", message.as_ref())
}

pub(crate) fn is_sidecar_pending_error(error: &str) -> bool {
    error.starts_with(SIDECAR_PENDING_PREFIX)
}

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
    /// How long a caller waits for a flight before taking its budget back.
    /// In here with the others so a suite can shrink it. Defaults to
    /// [`SIDECAR_JOIN_UNBOUNDED`]; only the session start narrows it.
    join_budget: Duration,
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            timeout: EXTRACTION_TIMEOUT,
            negative_ttl: NEGATIVE_TTL,
            max_sidecar_bytes: MAX_SIDECAR_BYTES,
            join_budget: SIDECAR_JOIN_UNBOUNDED,
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

/// Whether the whole-track extraction is going to answer this playback.
///
/// The midpoint rule below exists because a window past the midpoint reads
/// the same bytes the whole-track warm is reading, and publishes a disposable
/// result for them. That reasoning holds exactly as long as the whole-track
/// warm is alive and making progress. When it has failed, or has been running
/// longer than the window it is being compared against, "the whole track will
/// answer instead" stops being true — and then declining the window means the
/// second half of the file has no subtitles at all, for good.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WholeTrackProgress {
    /// Ready, absent, or warming. The midpoint rule's reason still holds, so
    /// past-midpoint windows stay declined.
    Healthy,
    /// The last attempt failed and its memo still stands. There is nothing
    /// left for a past-midpoint window to be redundant with.
    Stalled,
}

/// How the whole-track sidecar for one track is doing, for the past-midpoint
/// rule. Observation only, like [`sidecar_state`]: it starts nothing.
///
/// Only a remembered failure counts as stalled. "Warming for longer than I
/// expected" deliberately does not, and the reason is that no clock here has
/// any relationship to how long the extraction takes: a subtitle-only pass
/// over a 40 GB remux on a network mount can legitimately run for minutes,
/// and calling that stalled would start a second full-source scan against the
/// same mount to publish a disposable result the healthy warm is about to
/// supersede — the exact waste the midpoint rule exists to refuse.
///
/// A genuinely wedged extraction is not left forever either: `ensure_vtt_at`
/// bounds it at [`EXTRACTION_TIMEOUT`], and the timeout writes the memo that
/// this function reads.
pub async fn whole_track_progress(dir: &Path, file: &MediaFile, index: i64) -> WholeTrackProgress {
    if remembered_failure(&vtt_path(dir, file, index))
        .await
        .is_some()
    {
        return WholeTrackProgress::Stalled;
    }
    WholeTrackProgress::Healthy
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
///
/// `whole_track` is what makes the midpoint rule conditional rather than
/// absolute — see [`WholeTrackProgress`].
pub fn windowing_is_worthwhile(
    anchor_seconds: i64,
    duration_seconds: i64,
    window_seconds: i64,
    whole_track: WholeTrackProgress,
) -> bool {
    let window = bounded_window_seconds(window_seconds);
    // A file barely longer than one window has no head to bridge: the window
    // would extract effectively the whole track, concurrently with the
    // whole-track warm doing the identical scan, and publish the loser under a
    // disposable key. Require enough runtime that a window is a fraction of it.
    //
    // This first clause is unconditional. It is about the window being
    // pointless in itself, not about the whole track being better.
    if duration_seconds <= window.saturating_mul(2) {
        return false;
    }
    anchor_seconds.max(0).saturating_mul(2) < duration_seconds
        || whole_track == WholeTrackProgress::Stalled
}

/// Whether an extraction for exactly this window span is already running,
/// for anybody.
///
/// The cross-session dedup registries are the honest answer to "is these
/// bytes' producer alive right now": `warmups` holds the key from the moment
/// a flight is claimed, `extractions` from the moment the producer enlists.
/// Observation only — this starts nothing.
pub async fn window_flight_is_live(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    anchor_seconds: i64,
    window_seconds: i64,
) -> bool {
    let cached = vtt_window_path(dir, file, index, anchor_seconds, window_seconds);
    // Two statements, `extractions` first, and neither guard is alive when the
    // other lock is taken. Both matter. `||` keeps its left temporary alive to
    // the end of the statement, so a single expression would hold the first
    // guard across the second `await`; and every other site in this file locks
    // `extractions` before `warmups` (`sidecar_state`, `sidecar_state_for_demand`).
    // A single warmups-first site is a lock-order inversion, and with two async
    // mutexes that have no timeout the result is both global registries wedged
    // for the life of the process.
    let extracting = extractions().lock().await.contains_key(&cached);
    extracting || warmups().lock().await.contains(&cached)
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
///
/// `work` is the caller's class and purpose for the extraction a miss starts.
/// A flight already running keeps the class of the caller that started it.
pub async fn ensure_vtt(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    work: crate::process_control::ChildWork,
) -> Result<PathBuf, String> {
    ensure_vtt_with(dir, file, index, move |tmp, file, index| async move {
        extract_vtt(&tmp, &file, index, work).await
    })
    .await
}

/// The VTT-keyed path consults the stored representation before enlisting an
/// extraction. This must stay outside `ensure_vtt_at`: that helper also owns
/// burn and seek-window keys, for which a WebVTT copy would be wrong.
pub(crate) async fn ensure_vtt_with_store(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    stored: &crate::subtitle_source::StoreAccess,
    work: crate::process_control::ChildWork,
) -> Result<PathBuf, String> {
    if let Some(path) = try_store_vtt(dir, file, index, stored).await? {
        return Ok(path);
    }
    let stored = stored.clone();
    ensure_vtt_with(dir, file, index, move |tmp, file, index| async move {
        match cluster_vtt_into(&tmp, &file, index, &stored).await {
            Ok(true) => Ok(()),
            Ok(false) => extract_vtt(&tmp, &file, index, work).await,
            Err(reason) => Err(reason),
        }
    })
    .await
}

/// The cluster preference runs in the already detached whole-track flight.
/// A session start still joins for five seconds; direct and offline callers
/// keep their existing long join. Failure to find a usable cluster answer
/// releases this same flight to its original inline extractor.
async fn cluster_vtt_into(
    tmp: &Path,
    file: &MediaFile,
    index: i64,
    access: &crate::subtitle_source::StoreAccess,
) -> Result<bool, String> {
    use crate::subtitle_source::{Consumer, Live, Lookup, RepresentationFormat};
    use plurx_core::store::SubtitleSourceStamp;

    if crate::subtitle_source::is_mpegts_container(file) || !access.cluster_enabled().await {
        return Ok(false);
    }
    let (Some(store), Some(jobs)) = (access.store(), access.jobs()) else {
        return Ok(false);
    };
    if !jobs.subtitle_source_queue_enabled().await {
        return Ok(false);
    }
    let copy_local = || async {
        match crate::subtitle_source::lookup(
            access,
            Consumer::Vtt,
            file,
            index,
            Live::Path(&file.path),
        )
        .await
        {
            Lookup::Kept(kept) => crate::subtitle_source::copy_verified(&kept, tmp).await,
            Lookup::Empty(_) => tokio::fs::write(tmp, b"WEBVTT\n\n").await.is_ok(),
            Lookup::Miss(_) => false,
        }
    };
    if crate::subtitle_source::hydrate_from_peers(access, file, index, RepresentationFormat::Webvtt)
        .await
        && copy_local().await
    {
        return Ok(true);
    }
    let publications = store
        .list_subtitle_source_publications(file.id, file.size, file.mtime)
        .await
        .unwrap_or_default();
    let portable_digest = if publications.is_empty() {
        None
    } else {
        publication_source_digest(access, file, None).await
    };
    let settled_here = publications.iter().find(|row| {
        row.ordinal == index
            && row.format == "webvtt"
            && row.origin == "extracted"
            && Some(row.source_attestation.as_str()) == portable_digest.as_deref()
            && (matches!(row.verdict.as_str(), "kept" | "empty" | "malformed")
                || (row.verdict == "transient" && row.attempts >= 3))
    });
    if let Some(row) = settled_here {
        if row.verdict == "empty" {
            return Ok(tokio::fs::write(tmp, b"WEBVTT\n\n").await.is_ok());
        }
        if row.verdict != "kept" {
            return Ok(false);
        }
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if crate::subtitle_source::hydrate_from_peers(
                access,
                file,
                index,
                RepresentationFormat::Webvtt,
            )
            .await
                && copy_local().await
            {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    let stamp = SubtitleSourceStamp {
        file_id: file.id,
        source_size: file.size,
        source_mtime: file.mtime,
        pipeline_version: crate::state::subtitle_source_pipeline_version().await,
    };
    let mut request = match store
        .enqueue_or_promote_subtitle_source(&stamp, "foreground", subtitle_clock_ms())
        .await
    {
        Ok(Some(request)) => {
            crate::telemetry::record_subtitle_source(
                crate::telemetry::SubtitleSourceMetric::RequestForeground,
            );
            request
        }
        _ => return Ok(false),
    };
    if request.state == "ready" {
        let source_digest = publication_source_digest(access, file, None).await;
        if let Some(source_digest) = source_digest.filter(|digest| !digest.is_empty()) {
            if store
                .retire_subtitle_source_ready_for_ordinal(
                    &stamp,
                    index,
                    &source_digest,
                    subtitle_clock_ms(),
                )
                .await
                .unwrap_or(false)
            {
                crate::telemetry::record_subtitle_source(
                    crate::telemetry::SubtitleSourceMetric::Repair,
                );
                request = match store
                    .enqueue_or_promote_subtitle_source(&stamp, "foreground", subtitle_clock_ms())
                    .await
                {
                    Ok(Some(request)) => request,
                    _ => return Ok(false),
                };
            }
        }
    }
    let started = tokio::time::Instant::now();
    let mut self_claimed = false;
    loop {
        if copy_local().await {
            return Ok(true);
        }
        if crate::subtitle_source::hydrate_from_peers(
            access,
            file,
            index,
            RepresentationFormat::Webvtt,
        )
        .await
            && copy_local().await
        {
            return Ok(true);
        }
        let current = match store.analysis_request(&request.request_id).await {
            Ok(Some(current)) => current,
            _ => return Ok(false),
        };
        match current.state.as_str() {
            "cancelled" if current.last_error_code != "artifact_lost" => {
                return Err("subtitle cluster job was cancelled".to_owned());
            }
            "failed" | "cancelled" => return Ok(false),
            "ready" => {
                let rows = store
                    .list_subtitle_source_publications(file.id, file.size, file.mtime)
                    .await
                    .unwrap_or_default();
                let portable_digest = publication_source_digest(access, file, None).await;
                if rows.iter().any(|row| {
                    row.ordinal == index
                        && row.format == "webvtt"
                        && row.origin == "extracted"
                        && row.verdict == "empty"
                        && Some(row.source_attestation.as_str()) == portable_digest.as_deref()
                }) {
                    return Ok(tokio::fs::write(tmp, b"WEBVTT\n\n").await.is_ok());
                }
                return Ok(false);
            }
            "queued" if started.elapsed() >= Duration::from_secs(20) && !self_claimed => {
                self_claimed = true;
                let runtime_cache = access.root().parent().unwrap_or(access.root());
                let claimed = jobs
                    .self_claim_subtitle_source(&request.request_id, runtime_cache)
                    .await;
                if !claimed
                    && store
                        .analysis_request(&request.request_id)
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|latest| latest.state == "queued")
                {
                    return Err(
                        "subtitle cluster job is still queued after the foreground claim wait"
                            .to_owned(),
                    );
                }
            }
            _ => {}
        }
        if started.elapsed() >= CLUSTER_WAIT_TIMEOUT {
            // The queue worker may legitimately own this row longer than the
            // old inline extractor's bound. Expiring our wait must not start
            // a second full-source producer beside that running job.
            return Err(
                "subtitle cluster job is still running after the sidecar wait bound".to_owned(),
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// A replicated empty verdict has no bytes to verify. Attest the current
/// source before accepting it, and bind a burn flight to its held descriptor.
async fn publication_source_digest(
    access: &crate::subtitle_source::StoreAccess,
    file: &MediaFile,
    held_object_version: Option<&str>,
) -> Option<String> {
    let node_id = access.node_id()?;
    let attested = crate::fragment_index_cluster::attest_source(node_id, file, None, &|_| {})
        .await
        .ok()?;
    if held_object_version.is_some_and(|held| held != attested.observation.object_version) {
        return None;
    }
    Some(attested.observation.source_sha256)
}

fn subtitle_clock_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            elapsed.as_millis().min(i64::MAX as u128) as i64
        })
}

async fn try_store_vtt(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    stored: &crate::subtitle_source::StoreAccess,
) -> Result<Option<PathBuf>, String> {
    let cached = vtt_path(dir, file, index);
    if valid_sidecar(&cached, MAX_SIDECAR_BYTES).await {
        return Ok(Some(cached));
    }
    use crate::subtitle_source::{Consumer, Live, Lookup};
    match crate::subtitle_source::lookup(stored, Consumer::Vtt, file, index, Live::Path(&file.path))
        .await
    {
        Lookup::Kept(kept) => {
            if !extractions().lock().await.contains_key(&cached) {
                tokio::fs::create_dir_all(dir)
                    .await
                    .map_err(|error| format!("creating subtitle cache: {error}"))?;
                let tmp = dir.join(format!(".tmp-{}.vtt", uuid::Uuid::new_v4()));
                if crate::subtitle_source::copy_verified(&kept, &tmp).await {
                    let valid = tokio::fs::metadata(&tmp)
                        .await
                        .is_ok_and(|metadata| metadata.len() <= MAX_SIDECAR_BYTES);
                    if valid && tokio::fs::rename(&tmp, &cached).await.is_ok() {
                        forget_failure(&cached).await;
                        return Ok(Some(cached));
                    }
                    let _ = tokio::fs::remove_file(&tmp).await;
                }
            }
        }
        Lookup::Empty(store_dir) => {
            if !extractions().lock().await.contains_key(&cached) {
                tokio::fs::create_dir_all(dir)
                    .await
                    .map_err(|error| format!("creating subtitle cache: {error}"))?;
                let tmp = dir.join(format!(".tmp-{}.vtt", uuid::Uuid::new_v4()));
                if tokio::fs::write(&tmp, b"WEBVTT\n\n").await.is_ok()
                    && tokio::fs::rename(&tmp, &cached).await.is_ok()
                {
                    crate::subtitle_source::record_access(&store_dir).await;
                    forget_failure(&cached).await;
                    return Ok(Some(cached));
                }
                let _ = tokio::fs::remove_file(&tmp).await;
            }
        }
        Lookup::Miss(_) => {}
    }
    Ok(None)
}

/// Return the complete sidecar bytes from the same no-follow handle that was
/// bounded. HTTP consumers must use this instead of validating a pathname and
/// reopening it, which would let a symlink/oversized replacement win between
/// the two operations.
#[cfg(test)]
pub async fn ensure_vtt_bytes(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    work: crate::process_control::ChildWork,
) -> Result<Vec<u8>, String> {
    let path = ensure_vtt(dir, file, index, work).await?;
    read_vtt_path(&path, MAX_SIDECAR_BYTES)
        .await?
        .ok_or_else(|| "published subtitle sidecar is no longer valid".to_owned())
}

pub(crate) async fn ensure_vtt_bytes_with_store(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    stored: &crate::subtitle_source::StoreAccess,
    work: crate::process_control::ChildWork,
) -> Result<Vec<u8>, String> {
    let path = ensure_vtt_with_store(dir, file, index, stored, work).await?;
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
    if file.downloaded_subtitle(index).is_some() {
        return SidecarState::Ready;
    }
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

pub(crate) async fn sidecar_state_with_store(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    stored: &crate::subtitle_source::StoreAccess,
) -> SidecarState {
    if matches!(try_store_vtt(dir, file, index, stored).await, Ok(Some(_))) {
        SidecarState::Ready
    } else {
        sidecar_state(dir, file, index).await
    }
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

pub(crate) async fn sidecar_state_for_demand_with_store(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    anchor_seconds: i64,
    window_seconds: i64,
    stored: &crate::subtitle_source::StoreAccess,
) -> SidecarState {
    if sidecar_state_with_store(dir, file, index, stored).await == SidecarState::Ready {
        SidecarState::Ready
    } else {
        sidecar_state_for_demand(dir, file, index, anchor_seconds, window_seconds).await
    }
}

/// How much longer this track's failure memo stands, when one does.
///
/// The memo is what stops a player re-launching a full-source read every six
/// seconds against a track that has just failed, so it is also the honest
/// `Retry-After`: it says when this server will next be willing to try, which
/// is the only moment a retry could do anything. Observation only.
pub async fn failure_memo_remaining(dir: &Path, file: &MediaFile, index: i64) -> Option<Duration> {
    let cached = vtt_path(dir, file, index);
    let memos = negative_memos().lock().await;
    let memo = memos.get(&cached)?;
    memo.expires_at
        .checked_duration_since(tokio::time::Instant::now())
}

/// Read a warm sidecar without launching extraction. Used by AVPlayer's
/// short-deadline segmented subtitle route, where a cache miss must return an
/// empty segment immediately and warm in the background.
pub async fn read_cached_vtt(
    dir: &Path,
    file: &MediaFile,
    index: i64,
) -> Result<Option<Vec<u8>>, String> {
    if let Some(track) = file.downloaded_subtitle(index) {
        return Ok(Some(track.vtt.as_bytes().to_vec()));
    }
    read_vtt_path(&vtt_path(dir, file, index), MAX_SIDECAR_BYTES).await
}

pub(crate) async fn read_cached_vtt_with_store(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    stored: &crate::subtitle_source::StoreAccess,
) -> Result<Option<Vec<u8>>, String> {
    let _ = try_store_vtt(dir, file, index, stored).await;
    read_cached_vtt(dir, file, index).await
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
    work: crate::process_control::ChildWork,
) -> Result<std::fs::File, String> {
    let path = ensure_vtt(dir, file, index, work).await?;
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

pub(crate) async fn ensure_vtt_file_with_store(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    stored: &crate::subtitle_source::StoreAccess,
    work: crate::process_control::ChildWork,
) -> Result<std::fs::File, String> {
    let path = ensure_vtt_with_store(dir, file, index, stored, work).await?;
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

/// The largest burn sidecar publish accepts, enforced physically: the
/// producer is stopped at it rather than checked afterwards.
const MAX_BURN_BYTES: u64 = 64 * 1024 * 1024;

/// What a burned session needs from its subtitle track.
pub(crate) enum BurnSource {
    /// The bounded subtitle-only Matroska the overlay filter reads.
    File(std::fs::File),
    /// The track has no cues at all, so there is nothing to burn and the
    /// session is built without a subtitle overlay.
    ///
    /// Only the subtitle-source store can say this — it is what a pass that
    /// already read every packet learned — and saying it saves reading the
    /// whole source to publish an empty sidecar, which on file 5208 was 402 s
    /// spent to learn nothing.
    Nothing,
}

/// [`ensure_burn_source`] with the store ignored, for callers that only want
/// the sidecar handle.
#[cfg(test)]
pub(crate) async fn ensure_burn_file(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    expected_object_version: Option<&str>,
    join_budget: Duration,
) -> Result<std::fs::File, String> {
    match ensure_burn_source(
        dir,
        file,
        index,
        expected_object_version,
        join_budget,
        &crate::subtitle_source::StoreAccess::off(),
        crate::process_control::ChildClass::Background,
    )
    .await?
    {
        BurnSource::File(file) => Ok(file),
        // Only the store answers `Nothing`, and it is off here.
        BurnSource::Nothing => Err("the subtitle store answered a burn that ignores it".into()),
    }
}

/// Preserve bitmap display state and ASS styling in a small subtitle-only
/// Matroska. Restarting video must not discard a cue that began before its
/// seek landing. The existing cache owns one bounded extraction per identity,
/// including cancellation, negative memo, atomic publication, and pruning.
/// `join_budget` is the caller's, not this module's: a session start passes
/// [`SIDECAR_JOIN_BUDGET`] because it has 50 s to spend on everything, and a
/// caller with no deadline passes [`SIDECAR_JOIN_UNBOUNDED`].
///
/// When the source is not MPEG-TS, a current track in the subtitle-source
/// store stands in for the source read: `kept` derives the same sidecar from
/// the stored `.sup`, and `empty` answers [`BurnSource::Nothing`] without
/// extracting at all. Every other state — and a derivation that fails — is
/// today's extraction, unchanged, inside the same single flight.
///
/// `class` is the caller's: a session start that joins the flight for
/// [`SIDECAR_JOIN_BUDGET`] passes realtime. A flight already running keeps
/// the class of the caller that started it.
pub(crate) async fn ensure_burn_source(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    expected_object_version: Option<&str>,
    join_budget: Duration,
    stored: &crate::subtitle_source::StoreAccess,
    class: crate::process_control::ChildClass,
) -> Result<BurnSource, String> {
    ensure_burn_source_with(
        dir,
        file,
        index,
        expected_object_version,
        stored,
        ExtractionLimits {
            max_sidecar_bytes: MAX_BURN_BYTES,
            join_budget,
            ..Default::default()
        },
        Derivation::default(),
        class,
    )
    .await
}

/// How long a derivation from a stored `.sup` may run before it is killed
/// and the source is read instead.
///
/// Its own bound, far inside the flight's [`EXTRACTION_TIMEOUT`], because the
/// flight's is the one that reaches the negative memo: a derivation that hung
/// on a stalled cache disk and ran the flight out would be remembered as the
/// track's failure for [`NEGATIVE_TTL`], refusing the very source extraction
/// that was its fallback. A healthy derivation takes 0.04 s for an 18 KB
/// track and 0.25 s for a 10 MB one (design §6.2 fact 11), so thirty seconds
/// cuts off nothing honest.
const DERIVATION_TIMEOUT: Duration = Duration::from_secs(30);

/// The derivation's bound, and a test's way to make it hang.
#[derive(Clone, Copy)]
struct Derivation {
    timeout: Duration,
    /// Test-only: park forever in place of ffmpeg, so the bound is what ends it.
    #[cfg(test)]
    hang: bool,
}

impl Default for Derivation {
    fn default() -> Self {
        Self {
            timeout: DERIVATION_TIMEOUT,
            #[cfg(test)]
            hang: false,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn ensure_burn_source_with(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    expected_object_version: Option<&str>,
    stored: &crate::subtitle_source::StoreAccess,
    limits: ExtractionLimits,
    derivation: Derivation,
    class: crate::process_control::ChildClass,
) -> Result<BurnSource, String> {
    use sha2::{Digest, Sha256};
    let source = Arc::new(
        crate::fragment_index_cluster::open_source_fence(file, expected_object_version).await?,
    );
    if file.downloaded_subtitle(index).is_some() {
        return ensure_vtt_file(
            dir,
            file,
            index,
            crate::process_control::ChildWork::new(class, "downloaded subtitle for a burn"),
        )
        .await
        .map(BurnSource::File);
    }
    let version = hex::encode(Sha256::digest(source.object_version().as_bytes()));
    let cached = dir.join(format!("f{}-s{index}-{version}-burn-v2.mks", file.id));
    // A sidecar already published for this exact object is served before any
    // store work at all — no setting read, no manifest, no `fstat` — and
    // without entering the flight machinery, whose first step is this same
    // check. The store has nothing to save here.
    let path = if valid_sidecar(&cached, limits.max_sidecar_bytes).await {
        cached
    } else {
        let stored_track = match stored_burn_track(stored, file, index, &source).await {
            StoredBurn::Nothing => return Ok(BurnSource::Nothing),
            StoredBurn::Kept(kept) => Some(kept),
            StoredBurn::Extract => None,
        };
        // Whether this call's own extractor ran. A `kept` lookup whose caller
        // joined someone else's flight — or found the sidecar published by
        // the time it enlisted — never opens the stored bytes, and is counted
        // here instead so that no lookup goes uncounted.
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let kept_lookup = stored_track.is_some();
        let extractor_ran = Arc::clone(&ran);
        let extractor_source = Arc::clone(&source);
        let cluster_access = stored.clone();
        let cached_for_empty = cached.clone();
        let answer = ensure_vtt_at(
            cached,
            dir,
            file,
            index,
            limits,
            move |tmp, file, index| async move {
                extractor_ran.store(true, std::sync::atomic::Ordering::Release);
                let source = extractor_source;
                // The fallback lives inside this one flight, and a failed
                // derivation never reaches the negative memo: only this
                // closure's final answer is remembered, and a memo written for
                // the derivation would refuse the source extraction below for
                // its whole TTL. That is also why the derivation has a bound of
                // its own, far inside the flight's.
                let stored_track = match stored_track {
                    Some(kept) => Some(kept),
                    None => {
                        match cluster_wait_burn(&cluster_access, &file, index, &source).await? {
                            StoredBurn::Kept(kept) => Some(kept),
                            StoredBurn::Nothing => return Err(EMPTY_BURN_RESULT.to_owned()),
                            StoredBurn::Extract => None,
                        }
                    }
                };
                if let Some(kept) = stored_track {
                    if kept.format() == crate::subtitle_source::RepresentationFormat::Matroska {
                        if crate::subtitle_source::copy_verified(&kept, &tmp).await
                            && source.unchanged()
                        {
                            #[cfg(test)]
                            note_burn_route(file.id, "store");
                            return Ok(());
                        }
                        let _ = tokio::fs::remove_file(&tmp).await;
                    } else {
                        // Boxed: both routes carry a 64 KiB copy buffer in their
                        // state, and unboxed they are laid out, and in debug builds
                        // moved, side by side on the polling thread's stack.
                        if Box::pin(derive_burn_from_store(
                            &tmp,
                            &kept,
                            MAX_BURN_BYTES,
                            derivation,
                            class,
                        ))
                        .await
                        {
                            #[cfg(test)]
                            note_burn_route(file.id, "store");
                            if !source.unchanged() {
                                return Err("source changed during burn-track derivation".into());
                            }
                            return Ok(());
                        }
                    }
                }
                #[cfg(test)]
                note_burn_route(file.id, "source");
                #[cfg(not(test))]
                let _ = &file;
                extract_burn_from_source(&tmp, &source, index, class).await
            },
        )
        .await;
        if kept_lookup && !ran.load(std::sync::atomic::Ordering::Acquire) {
            crate::subtitle_source::record_kept_joined(crate::subtitle_source::Consumer::Burn);
        }
        match answer {
            Err(reason) if reason == EMPTY_BURN_RESULT => {
                forget_failure(&cached_for_empty).await;
                return Ok(BurnSource::Nothing);
            }
            other => other?,
        }
    };
    if !source.unchanged() {
        return Err("source changed before burn sidecar attachment".into());
    }
    tokio::task::spawn_blocking(move || {
        let file = plurx_core::fs_secure::open_read_nofollow_blocking(&path)
            .map_err(|error| format!("opening burn sidecar: {error}"))?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("reading burn sidecar metadata: {error}"))?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_BURN_BYTES {
            return Err("burn sidecar is not a bounded regular file".into());
        }
        Ok(BurnSource::File(file))
    })
    .await
    .map_err(|error| format!("burn sidecar open worker failed: {error}"))?
}

/// What the subtitle-source store has for one burn.
enum StoredBurn {
    Kept(crate::subtitle_source::KeptTrack),
    Nothing,
    Extract,
}

async fn stored_burn_track(
    stored: &crate::subtitle_source::StoreAccess,
    file: &MediaFile,
    index: i64,
    source: &crate::fragment_index_cluster::SourceFence,
) -> StoredBurn {
    use crate::subtitle_source::{lookup, Consumer, Live, Lookup};
    // Validity at use is a live `fstat` of the file this session holds open,
    // not of its path, so a replacement at the name cannot vouch for this
    // inode. The lookup takes it last, after the switch and the manifest.
    match lookup(
        stored,
        Consumer::Burn,
        file,
        index,
        Live::Handle(&source.handle),
    )
    .await
    {
        Lookup::Kept(kept) => StoredBurn::Kept(kept),
        Lookup::Empty(dir) => {
            crate::subtitle_source::record_access(&dir).await;
            tracing::info!(
                file_id = file.id,
                index,
                "the stored PGS track has no cues; burning nothing instead of reading the source"
            );
            StoredBurn::Nothing
        }
        Lookup::Miss(_) => StoredBurn::Extract,
    }
}

/// Find or build a burn representation inside the existing detached burn
/// flight. The held source remains open while a peer copy is bound to this
/// node, so a replacement at the pathname cannot authorize the wrong inode.
async fn cluster_wait_burn(
    access: &crate::subtitle_source::StoreAccess,
    file: &MediaFile,
    index: i64,
    source: &crate::fragment_index_cluster::SourceFence,
) -> Result<StoredBurn, String> {
    use crate::subtitle_source::RepresentationFormat;
    use plurx_core::store::SubtitleSourceStamp;

    if crate::subtitle_source::is_mpegts_container(file) || !access.cluster_enabled().await {
        return Ok(StoredBurn::Extract);
    }
    let (Some(store), Some(jobs)) = (access.store(), access.jobs()) else {
        return Ok(StoredBurn::Extract);
    };
    if !jobs.subtitle_source_queue_enabled().await {
        return Ok(StoredBurn::Extract);
    }
    let format = match file
        .subtitle_streams
        .iter()
        .find(|stream| stream.index == index)
        .map(|stream| stream.codec.as_str())
    {
        Some("ass" | "ssa") => RepresentationFormat::Matroska,
        Some("hdmv_pgs_subtitle") => RepresentationFormat::Sup,
        _ => return Ok(StoredBurn::Extract),
    };
    let _ = crate::subtitle_source::hydrate_from_peers(access, file, index, format).await;
    match stored_burn_track(access, file, index, source).await {
        StoredBurn::Extract => {}
        answer => return Ok(answer),
    }
    let stamp = SubtitleSourceStamp {
        file_id: file.id,
        source_size: file.size,
        source_mtime: file.mtime,
        pipeline_version: crate::state::subtitle_source_pipeline_version().await,
    };
    let rows = store
        .list_subtitle_source_publications(file.id, file.size, file.mtime)
        .await
        .unwrap_or_default();
    let portable_digest = if rows.is_empty() {
        None
    } else {
        publication_source_digest(access, file, Some(source.object_version())).await
    };
    let format_name = format.publication_name();
    if let Some(row) = rows.iter().find(|row| {
        row.ordinal == index
            && row.format == format_name
            && row.origin == "extracted"
            && Some(row.source_attestation.as_str()) == portable_digest.as_deref()
            && (matches!(row.verdict.as_str(), "kept" | "empty" | "malformed")
                || (row.verdict == "transient" && row.attempts >= 3))
    }) {
        match row.verdict.as_str() {
            "empty" => return Ok(StoredBurn::Nothing),
            "kept" => {
                for _ in 0..10 {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let _ = crate::subtitle_source::hydrate_from_peers(access, file, index, format)
                        .await;
                    match stored_burn_track(access, file, index, source).await {
                        StoredBurn::Extract => {}
                        answer => return Ok(answer),
                    }
                }
                return Ok(StoredBurn::Extract);
            }
            _ => return Ok(StoredBurn::Extract),
        }
    }
    let mut request = match store
        .enqueue_or_promote_subtitle_source(&stamp, "foreground", subtitle_clock_ms())
        .await
    {
        Ok(Some(request)) => {
            crate::telemetry::record_subtitle_source(
                crate::telemetry::SubtitleSourceMetric::RequestForeground,
            );
            request
        }
        _ => return Ok(StoredBurn::Extract),
    };
    if request.state == "ready" {
        let source_digest =
            publication_source_digest(access, file, Some(source.object_version())).await;
        if let Some(source_digest) = source_digest.filter(|digest| !digest.is_empty()) {
            if store
                .retire_subtitle_source_ready_for_ordinal(
                    &stamp,
                    index,
                    &source_digest,
                    subtitle_clock_ms(),
                )
                .await
                .unwrap_or(false)
            {
                crate::telemetry::record_subtitle_source(
                    crate::telemetry::SubtitleSourceMetric::Repair,
                );
                request = match store
                    .enqueue_or_promote_subtitle_source(&stamp, "foreground", subtitle_clock_ms())
                    .await
                {
                    Ok(Some(request)) => request,
                    _ => return Ok(StoredBurn::Extract),
                };
            }
        }
    }
    let started = tokio::time::Instant::now();
    let mut self_claimed = false;
    loop {
        let _ = crate::subtitle_source::hydrate_from_peers(access, file, index, format).await;
        match stored_burn_track(access, file, index, source).await {
            StoredBurn::Extract => {}
            answer => return Ok(answer),
        }
        let current = match store.analysis_request(&request.request_id).await {
            Ok(Some(current)) => current,
            _ => return Ok(StoredBurn::Extract),
        };
        match current.state.as_str() {
            "cancelled" if current.last_error_code != "artifact_lost" => {
                return Err("subtitle cluster job was cancelled".to_owned());
            }
            "failed" | "cancelled" => return Ok(StoredBurn::Extract),
            "ready" => {
                let rows = store
                    .list_subtitle_source_publications(file.id, file.size, file.mtime)
                    .await
                    .unwrap_or_default();
                if rows.iter().any(|row| {
                    row.ordinal == index && row.format == format_name && row.verdict == "empty"
                }) {
                    return Ok(StoredBurn::Nothing);
                }
                return Ok(StoredBurn::Extract);
            }
            "queued" if started.elapsed() >= Duration::from_secs(20) && !self_claimed => {
                self_claimed = true;
                let runtime_cache = access.root().parent().unwrap_or(access.root());
                let claimed = jobs
                    .self_claim_subtitle_source(&request.request_id, runtime_cache)
                    .await;
                if !claimed
                    && store
                        .analysis_request(&request.request_id)
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|latest| latest.state == "queued")
                {
                    return Err(
                        "subtitle cluster job is still queued after the foreground claim wait"
                            .to_owned(),
                    );
                }
            }
            _ => {}
        }
        if started.elapsed() >= CLUSTER_WAIT_TIMEOUT {
            // A running worker retains the sole producer slot until its own
            // lease/worker deadline. Memo this flight error instead of
            // launching an inline extraction in parallel with it.
            return Err(
                "subtitle cluster job is still running after the burn wait bound".to_owned(),
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Derive the burn sidecar from a stored `.sup`. `false` means use the source.
///
/// **`-copyts` and no `-start_at_zero`.** The stored `.sup` was written
/// without `-copyts`, so its cues already sit where the source extraction's
/// `-copyts -start_at_zero` puts them; reading it back with `-copyts` keeps
/// them there. Adding `-start_at_zero` — the source path's argv, and the
/// obvious thing to "harmonise" — moves every cue earlier by the time of the
/// first one. That was measured, and
/// `a_derived_burn_sidecar_keeps_the_source_extraction_cue_times` pins it.
///
/// The stored file is read through the descriptor that was hashed, so the
/// bytes derived are the bytes verified whatever happens to its name. The
/// output is bounded physically, as the source extraction's is, and the run
/// is bounded in time by [`DERIVATION_TIMEOUT`]; expiry drops the child, which
/// kills it.
async fn derive_burn_from_store(
    tmp: &Path,
    kept: &crate::subtitle_source::KeptTrack,
    max_bytes: u64,
    derivation: Derivation,
    class: crate::process_control::ChildClass,
) -> bool {
    use crate::subtitle_source::{record_fallback, Fallback};
    let Some(sup) = crate::subtitle_source::open_verified(kept).await else {
        return false;
    };
    let run = Box::pin(run_burn_derivation(tmp, &sup, max_bytes, class));
    #[cfg(test)]
    let run = async {
        if derivation.hang {
            std::future::pending::<()>().await;
        }
        run.await
    };
    let (why, reason) = match tokio::time::timeout(derivation.timeout, run).await {
        Ok(Ok(())) => return true,
        Ok(Err(why)) => {
            let over_bound = tokio::fs::metadata(tmp)
                .await
                .is_ok_and(|metadata| metadata.len() >= max_bytes);
            (
                why,
                if over_bound {
                    Fallback::OverBound
                } else {
                    Fallback::DeriveFailed
                },
            )
        }
        Err(_) => (
            format!(
                "burn-track derivation did not finish in {}s",
                derivation.timeout.as_secs_f64()
            ),
            Fallback::TimedOut,
        ),
    };
    record_fallback(reason);
    tracing::warn!(
        stored = %kept.path().display(),
        why,
        "deriving the burn sidecar from a stored PGS track failed; reading the source instead"
    );
    // The source extraction creates this path afresh.
    let _ = tokio::fs::remove_file(tmp).await;
    false
}

/// The derivation's argv, apart from its input and output. Named so a test can
/// run exactly this and the `-start_at_zero` variant it must not become.
fn burn_derivation_args(input: &Path) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-y",
        "-copyts",
        "-f",
        "sup",
        "-i",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    args.push(input.as_os_str().to_owned());
    args.extend(
        [
            "-map",
            "0:s:0",
            "-c",
            "copy",
            "-avoid_negative_ts",
            "disabled",
            "-f",
            "matroska",
            "pipe:1",
        ]
        .into_iter()
        .map(Into::into),
    );
    args
}

async fn run_burn_derivation(
    tmp: &Path,
    sup: &std::fs::File,
    max_bytes: u64,
    class: crate::process_control::ChildClass,
) -> Result<(), String> {
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    crate::ffmpeg::inherit_file_descriptors(&mut command, &[(sup, 3)]);
    #[cfg(unix)]
    let input = PathBuf::from("/dev/fd/3");
    #[cfg(windows)]
    let input = crate::ffmpeg::windows_source_path(sup)?;
    command
        .args(burn_derivation_args(&input))
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    crate::ffmpeg::verify_windows_source_path(sup, &input)?;
    let (status, diagnostics) = crate::ffmpeg::BoundedDiagnosticChild::spawn_piped_output(
        &mut command,
        crate::process_control::ChildWork::new(class, "burned-subtitle track derivation"),
    )
    .map_err(|error| format!("starting burn-track derivation: {error}"))?
    .output_to_bounded_file(tmp, max_bytes)
    .await
    .map_err(|error| format!("waiting for burn-track derivation: {error}"))?;
    if !status.success() {
        return Err(format!(
            "burn-track derivation failed: {}",
            diagnostics.trim()
        ));
    }
    // ffmpeg reports a stream it cannot demux and still exits 0 with a valid,
    // cue-less Matroska — measured with `-f sup` on bytes that are not PGS. A
    // clean derivation prints nothing at `-loglevel error`, so anything it
    // printed is a reason to read the source instead.
    if !diagnostics.trim().is_empty() {
        return Err(format!(
            "burn-track derivation reported: {}",
            diagnostics.trim()
        ));
    }
    // An empty output would be refused at publish — and memoized there — so
    // it is a failed derivation here, where the source can still answer.
    let produced = tokio::fs::metadata(tmp)
        .await
        .map_err(|error| format!("reading the derived burn sidecar: {error}"))?
        .len();
    if produced == 0 {
        return Err("burn-track derivation produced nothing".into());
    }
    Ok(())
}

/// Today's burn extraction: one full read of the source through the held
/// descriptor.
async fn extract_burn_from_source(
    tmp: &Path,
    source: &crate::fragment_index_cluster::SourceFence,
    index: i64,
    class: crate::process_control::ChildClass,
) -> Result<(), String> {
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    crate::ffmpeg::inherit_file_descriptors(&mut command, &[(&source.handle, 3)]);
    #[cfg(unix)]
    let input = PathBuf::from("/dev/fd/3");
    #[cfg(windows)]
    let input = crate::ffmpeg::windows_source_path(&source.handle)?;
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-copyts",
            "-start_at_zero",
            "-i",
        ])
        .arg(&input)
        .args([
            "-map",
            &format!("0:s:{index}"),
            "-map",
            "0:t?",
            "-c",
            "copy",
            "-avoid_negative_ts",
            "disabled",
            "-f",
            "matroska",
            "-fs",
            &(MAX_BURN_BYTES + 1).to_string(),
        ])
        .arg("pipe:1")
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    crate::ffmpeg::verify_windows_source_path(&source.handle, &input)?;
    let (status, diagnostics) = crate::ffmpeg::BoundedDiagnosticChild::spawn_piped_output(
        &mut command,
        crate::process_control::ChildWork::new(class, "burned-subtitle track extraction"),
    )
    .map_err(|error| format!("starting burn-track extraction: {error}"))?
    .output_to_bounded_file(tmp, MAX_BURN_BYTES)
    .await
    .map_err(|error| format!("waiting for burn-track extraction: {error}"))?;
    if !status.success() {
        return Err(format!(
            "burn-track extraction failed: {}",
            diagnostics.trim()
        ));
    }
    if !source.unchanged() {
        return Err("source changed during burn-track extraction".into());
    }
    Ok(())
}

/// Which route produced each burn sidecar, by file id, in order. Test-only:
/// the timestamp fixture has to prove the derived path ran, because the
/// source path would pass the same comparison trivially.
#[cfg(test)]
fn burn_routes() -> &'static std::sync::Mutex<HashMap<i64, Vec<&'static str>>> {
    static ROUTES: OnceLock<std::sync::Mutex<HashMap<i64, Vec<&'static str>>>> = OnceLock::new();
    ROUTES.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

#[cfg(test)]
fn note_burn_route(file_id: i64, route: &'static str) {
    burn_routes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(file_id)
        .or_default()
        .push(route);
}

#[cfg(test)]
pub(crate) fn burn_routes_for_test(file_id: i64) -> Vec<&'static str> {
    burn_routes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&file_id)
        .cloned()
        .unwrap_or_default()
}

/// Start materialising a sidecar without holding the caller open.
///
/// Native HLS uses this before returning an empty cold-cache segment. AVPlayer
/// blocks the muxed video behind subtitle segment I/O and gives that I/O about
/// two seconds, while a real extraction may scan a multi-gigabyte source for
/// minutes. One detached warmer per key lets playback begin immediately and
/// lets later segments pick up the finished captions.
pub(crate) async fn warm_vtt_with_store(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    stored: &crate::subtitle_source::StoreAccess,
) {
    if matches!(try_store_vtt(dir, file, index, stored).await, Ok(Some(_))) {
        return;
    }
    let stored = stored.clone();
    warm_vtt_with(dir, file, index, move |tmp, file, index| async move {
        match cluster_vtt_into(&tmp, &file, index, &stored).await {
            Ok(true) => Ok(()),
            Ok(false) => extract_vtt(&tmp, &file, index, SUBTITLE_WARM_UP).await,
            Err(reason) => Err(reason),
        }
    })
    .await;
}

/// A warm-up nobody is waiting on: native HLS answers the segment empty and
/// later segments pick the captions up.
const SUBTITLE_WARM_UP: crate::process_control::ChildWork =
    crate::process_control::ChildWork::background("subtitle track warm-up");

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
                Err(why) if why == EMPTY_BURN_RESULT => {}
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

    join_flight(&flight, limits.join_budget).await
}

/// Wait for whoever owns this key to publish an answer, or give the caller
/// back its budget.
///
/// The wait used to be unbounded, and the owning task is deliberately detached
/// so that a cancelled caller does not abandon a running ffmpeg — which meant
/// a caller inherited the extraction's whole runtime whether or not it had
/// that long to give. Timing out here abandons only the *waiting*: the
/// extraction keeps going, publishes as it always did, and the next caller
/// finds it ready.
async fn join_flight(flight: &Extraction, budget: Duration) -> Result<PathBuf, String> {
    match tokio::time::timeout(budget, join_flight_unbounded(flight)).await {
        Ok(result) => result,
        Err(_) => Err(sidecar_pending_error(
            "the source is still being read for this track",
        )),
    }
}

async fn join_flight_unbounded(flight: &Extraction) -> Result<PathBuf, String> {
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
            result = join_flight_unbounded(&flight) => WindowExtractionOutcome::Complete(result),
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

/// Extract a playback text window. Indexed Matroska seeks skip preceding
/// clusters. Copy timestamps without output -ss keeps one absolute timeline
/// on both old and new FFmpeg; Rust filters preroll without rebasing cues.
pub(crate) async fn extract_vtt_window(
    tmp: &Path,
    file: &MediaFile,
    index: i64,
    anchor_seconds: i64,
    window_seconds: i64,
) -> Result<(), String> {
    if let Some(track) = file.downloaded_subtitle(index) {
        // The consumer slices absolute cue times to its segment. Keeping the
        // complete small track here also preserves cues spanning a window.
        return tokio::fs::write(tmp, &track.vtt)
            .await
            .map_err(|e| e.to_string());
    }
    let end = anchor_seconds
        .saturating_add(bounded_window_seconds(window_seconds))
        .saturating_add(WINDOW_SLACK_SECONDS);
    let source = crate::fragment_index_cluster::open_source_fence(file, None).await?;
    #[cfg(unix)]
    let input = PathBuf::from("/dev/fd/3");
    #[cfg(windows)]
    let input = crate::ffmpeg::windows_source_path(&source.handle)?;
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    crate::ffmpeg::inherit_file_descriptors(&mut command, &[(&source.handle, 3)]);
    command.args(["-hide_banner", "-loglevel", "error"]);
    let indexed = crate::subtitle_ranges::indexed_text(file, index);
    if indexed {
        command.args([
            "-copyts",
            "-start_at_zero",
            "-ss",
            &anchor_seconds.saturating_sub(60).max(0).to_string(),
        ]);
    }
    command.arg("-i").arg(&input);
    // Output -ss rebases subtitles on FFmpeg 5 but not 8. With copyts and
    // no output seek both emit the full-extraction timeline, even when the
    // first cue is sparse or the container starts at a nonzero timestamp.
    if !indexed {
        command.args(["-ss", &anchor_seconds.to_string()]);
    }
    command
        .args([
            "-to",
            &end.to_string(),
            "-map",
            &format!("0:s:{index}"),
            "-f",
            "webvtt",
        ])
        .arg("pipe:1")
        .stdin(std::process::Stdio::null())
        // Same reason as the whole-track extractor: the bound is a kill, not
        // merely a stopped wait, or a wedged ffmpeg keeps the stalled mount
        // open after the future is dropped.
        .kill_on_drop(true);
    #[cfg(windows)]
    crate::ffmpeg::verify_windows_source_path(&source.handle, &input)?;
    let (status, diagnostics) = crate::ffmpeg::BoundedDiagnosticChild::spawn_piped_output(
        &mut command,
        crate::process_control::ChildWork::realtime("subtitle window for the playhead"),
    )
    .map_err(|error| format!("spawning subtitle window extraction: {error}"))?
    .output_to_bounded_file(tmp, MAX_SIDECAR_BYTES)
    .await
    .map_err(|error| format!("reading subtitle window extraction: {error}"))?;
    if !status.success() {
        return Err(format!(
            "subtitle window extraction failed: {}",
            diagnostics.trim()
        ));
    }
    if !source.unchanged() || source.reopen(file).await.is_err() {
        return Err("subtitle window source changed during extraction".to_owned());
    }
    if !diagnostics.trim().is_empty() {
        return Err("subtitle window decoder reported an error".to_owned());
    }
    let extracted = plurx_core::fs_secure::read_bounded_regular(tmp, MAX_SIDECAR_BYTES)
        .await
        .map_err(|e| format!("reading the extracted subtitle window: {e}"))?;
    let normalized = if indexed {
        // Preroll is already in absolute source time. Never infer its base
        // from the first cue: sparse windows make that ambiguous.
        crate::subtitle_ranges::filter_absolute_window(&extracted, anchor_seconds, end)?
    } else {
        normalize_window_cues(&extracted, anchor_seconds)
    };
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
#[allow(clippy::too_many_arguments)]
pub async fn warm_vtt_window(
    session: &str,
    sequence: Option<u64>,
    dir: &Path,
    file: &MediaFile,
    index: i64,
    anchor_seconds: i64,
    window_seconds: i64,
    access: &crate::subtitle_source::StoreAccess,
) -> bool {
    let access = access.clone();
    let dir_owned = dir.to_owned();
    warm_vtt_window_with(
        session,
        sequence,
        dir,
        file,
        index,
        anchor_seconds,
        window_seconds,
        move |tmp, file, index, anchor_seconds, window_seconds| async move {
            crate::subtitle_ranges::prepare(
                &access,
                &dir_owned,
                &tmp,
                &file,
                index,
                anchor_seconds,
                window_seconds,
            )
            .await
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
    if !crate::subtitle_ranges::indexed_text(file, index)
        && !windowing_is_worthwhile(
            anchor_seconds,
            duration_seconds,
            window_seconds,
            whole_track_progress(dir, file, index).await,
        )
    {
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

/// Memo a whole-track extraction failure, as a real one would.
///
/// The HTTP boundary fixture substitutes the *producer*, not the registries,
/// so a test that wants the "this track has already failed" state has to
/// reach the same memo the production failure path writes — otherwise it is
/// asserting on a state the server can never actually be in.
#[cfg(test)]
pub(crate) async fn remember_whole_track_failure_for_test(
    dir: &Path,
    file: &MediaFile,
    index: i64,
    why: &str,
    ttl: Duration,
) {
    remember_failure(&vtt_path(dir, file, index), why, ttl).await;
}

/// Drop the memo a test wrote, so it cannot outlive its own fixture in the
/// process-global registry.
#[cfg(test)]
pub(crate) async fn forget_whole_track_failure_for_test(dir: &Path, file: &MediaFile, index: i64) {
    forget_failure(&vtt_path(dir, file, index)).await;
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

async fn extract_vtt(
    tmp: &Path,
    file: &MediaFile,
    index: i64,
    work: crate::process_control::ChildWork,
) -> Result<(), String> {
    if let Some(track) = file.downloaded_subtitle(index) {
        return tokio::fs::write(tmp, &track.vtt)
            .await
            .map_err(|e| e.to_string());
    }
    let source = crate::fragment_index_cluster::open_source_fence(file, None).await?;
    #[cfg(unix)]
    let input = PathBuf::from("/dev/fd/3");
    #[cfg(windows)]
    let input = crate::ffmpeg::windows_source_path(&source.handle)?;
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    crate::ffmpeg::inherit_file_descriptors(&mut command, &[(&source.handle, 3)]);
    command
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&input)
        .args(["-map", &format!("0:s:{index}"), "-f", "webvtt"])
        .arg(tmp)
        .stdin(std::process::Stdio::null())
        // The timeout in `publish_extraction` bounds this by dropping the
        // future; without `kill_on_drop` that drop would merely stop waiting
        // and leave the wedged ffmpeg holding the stalled mount open. This is
        // what makes the bound an actual kill.
        .kill_on_drop(true);
    #[cfg(windows)]
    crate::ffmpeg::verify_windows_source_path(&source.handle, &input)?;
    let out = crate::process_control::output_job_owned(&mut command, work)
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

/// The class a test's own extraction runs at.
#[cfg(test)]
pub(crate) const TEST_TRACK: crate::process_control::ChildWork =
    crate::process_control::ChildWork::background("subtitle extraction test");

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
        const HEALTHY: super::WholeTrackProgress = super::WholeTrackProgress::Healthy;
        let hour = 3_600;
        let w = super::WINDOW_SECONDS_DEFAULT;
        assert!(super::windowing_is_worthwhile(0, hour, w, HEALTHY));
        assert!(super::windowing_is_worthwhile(600, hour, w, HEALTHY));
        assert!(super::windowing_is_worthwhile(1_799, hour, w, HEALTHY));
        // The midpoint is where the measured cost reaches the whole track's
        // neighbourhood; at and past it, decline.
        assert!(!super::windowing_is_worthwhile(1_800, hour, w, HEALTHY));
        assert!(!super::windowing_is_worthwhile(3_000, hour, w, HEALTHY));
        assert!(!super::windowing_is_worthwhile(3_600, hour, w, HEALTHY));
        // A duration we do not know is not a duration of zero: there is no
        // midpoint to compare against, so there is no saving to claim.
        assert!(!super::windowing_is_worthwhile(0, 0, w, HEALTHY));
        assert!(!super::windowing_is_worthwhile(10, -1, w, HEALTHY));
        // A file barely longer than one window has no head to bridge — the
        // window would extract the whole track alongside the warm doing the
        // same scan, and publish the loser under a disposable key.
        assert!(!super::windowing_is_worthwhile(0, w, w, HEALTHY));
        assert!(!super::windowing_is_worthwhile(0, w * 2, w, HEALTHY));
        assert!(super::windowing_is_worthwhile(0, w * 2 + 1, w, HEALTHY));
        // A position before the file is a client bug. It is clamped rather
        // than trusted, in both the grid and the warmer, so neither a negative
        // filename nor a negative `-ss` can be produced.
        assert_eq!(super::window_anchor_seconds(-500, w), 0);

        // The midpoint rule's reason is that the whole-track warm reads the
        // same bytes and publishes the authoritative answer. When that warm
        // has failed, or has been grinding for longer than the window it is
        // being compared against, there is nothing left to be redundant with
        // — and declining means the back half of the film never gets
        // subtitles at all.
        let stalled = super::WholeTrackProgress::Stalled;
        assert!(super::windowing_is_worthwhile(1_800, hour, w, stalled));
        assert!(super::windowing_is_worthwhile(3_000, hour, w, stalled));
        // The other clause is not about the whole track and stays absolute: a
        // file barely longer than one window has no head to bridge however
        // the whole-track warm is doing.
        assert!(!super::windowing_is_worthwhile(0, w * 2, w, stalled));
        assert!(!super::windowing_is_worthwhile(0, 0, w, stalled));
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

    #[tokio::test]
    async fn downloaded_captions_rebuild_cache_without_embedded_streams_or_provider() {
        let dir = crate::test_tempdir().expect("downloaded caption fixture");
        let mut file = media_file(dir.path().join("not-an-embedded-subtitle.mkv"));
        let vtt = "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nFirst\n\n00:02:00.000 --> 00:02:05.000\nLater\n";
        file.subtitle_streams = vec![plurx_core::domain::SubtitleStream {
            index: 0,
            codec: "webvtt".into(),
            ..Default::default()
        }];
        file.downloaded_subtitles = vec![plurx_core::domain::DownloadedSubtitle {
            source_size: file.size,
            source_mtime: file.mtime,
            provider_file_id: 1,
            language: "en".into(),
            title: "Example".into(),
            hearing_impaired: false,
            forced: false,
            vtt: vtt.into(),
        }];
        assert_eq!(
            read_cached_vtt(dir.path(), &file, 0)
                .await
                .expect("downloaded caption fixture")
                .expect("downloaded caption fixture"),
            vtt.as_bytes()
        );
        assert!(matches!(
            sidecar_state(dir.path(), &file, 0).await,
            SidecarState::Ready
        ));
        assert_eq!(
            ensure_vtt_bytes(dir.path(), &file, 0, crate::subtitles::TEST_TRACK)
                .await
                .expect("downloaded caption fixture"),
            vtt.as_bytes()
        );
        tokio::fs::remove_file(vtt_path(dir.path(), &file, 0))
            .await
            .expect("downloaded caption fixture");
        assert_eq!(
            ensure_vtt_bytes(dir.path(), &file, 0, crate::subtitles::TEST_TRACK)
                .await
                .expect("downloaded caption fixture"),
            vtt.as_bytes()
        );
        let window = dir.path().join("window.vtt");
        extract_vtt_window(&window, &file, 0, 200, 200)
            .await
            .expect("downloaded caption fixture");
        assert_eq!(
            tokio::fs::read(window)
                .await
                .expect("downloaded caption fixture"),
            vtt.as_bytes(),
            "absolute cues are never shifted to a later seek window"
        );
    }

    fn media_file(path: PathBuf) -> MediaFile {
        MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 77,
            item_id: 1,
            path,
            size: 12_345,
            mtime: 678,
            duration_ms: Some(60_000),
            container: Some("mkv".into()),
            video_codec: Some("h264".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: None,
            width: Some(1920),
            height: Some(1080),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: Some(8_000_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    /// The midpoint rule declines a window because the whole-track warm reads
    /// the same bytes and publishes the authoritative answer instead. That
    /// reason is about the warm being alive: once it has failed there is
    /// nothing left to be redundant with, and declining meant the back half
    /// of the film had no subtitles for good.
    ///
    /// Driven through `warm_vtt_window_with`, which is where the rule is
    /// actually consulted — the HTTP boundary only forwards the anchor.
    #[tokio::test]
    async fn a_failed_whole_track_lets_a_window_past_the_midpoint_start() {
        let dir = tempfile::tempdir().expect("cache dir");
        let mut file = media_file(dir.path().join("past-midpoint.mkv"));
        // An hour, so the midpoint is well clear of the window span below.
        file.duration_ms = Some(3_600_000);
        let session = &format!("past-midpoint-{}", uuid::Uuid::new_v4());
        let anchor = 3_000; // past 1,800 s
        let window = 200;

        let never = |_tmp: PathBuf, _f: MediaFile, _i: i64, _a: i64, _w: i64| async move {
            std::future::pending::<()>().await;
            Ok(())
        };

        assert!(
            !warm_vtt_window_with(session, None, dir.path(), &file, 0, anchor, window, never).await,
            "a healthy whole-track warm keeps the midpoint rule"
        );
        assert_eq!(
            peak_window_flights_for_test(session),
            0,
            "and starts nothing at all"
        );

        remember_whole_track_failure_for_test(
            dir.path(),
            &file,
            0,
            "the source could not be read",
            Duration::from_secs(90),
        )
        .await;

        assert!(
            warm_vtt_window_with(session, None, dir.path(), &file, 0, anchor, window, never).await,
            "a dead whole-track warm is not a reason to leave the second half blank"
        );
        // The flight is owned the moment `warm_vtt_window_with` returns true;
        // the producer itself starts one poll later, which is what this yield
        // is for. The count is the contract — one per playback, never two.
        tokio::task::yield_now().await;
        assert_eq!(
            peak_window_flights_for_test(session),
            1,
            "and it is still one flight per playback"
        );

        release_session_window(session).await;
        forget_failure(&vtt_path(dir.path(), &file, 0)).await;
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
        let mut held = ensure_vtt_file(dir.path(), &file, 0, crate::subtitles::TEST_TRACK)
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

    /// A cold sidecar on a large source is not a failure and must not be paid
    /// for by whoever asked first.
    ///
    /// The extraction reads the whole container — 79.5 GB for file 5208, 402 s
    /// measured, to produce 18,866 bytes — so a caller that waits for it
    /// inherits a runtime no start budget can contain. The wait is bounded and
    /// the *extraction* is not: it must still finish, publish, and be there
    /// for the next caller.
    #[tokio::test]
    async fn a_cold_sidecar_gives_the_caller_its_budget_back_and_keeps_extracting() {
        let dir = crate::test_tempdir().expect("cache");
        let file = media_file(dir.path().join("source.mkv"));
        let limits = || ExtractionLimits {
            join_budget: Duration::from_millis(50),
            ..ExtractionLimits::default()
        };
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let finished = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let finished_for_run = Arc::clone(&finished);
        let slow = ensure_vtt_bounded(
            dir.path(),
            &file,
            0,
            limits(),
            move |tmp, _, _| async move {
                // Longer than the join budget, shorter than the extraction's own.
                let _ = release_rx.await;
                tokio::fs::write(&tmp, b"WEBVTT\n\ncold\n")
                    .await
                    .map_err(|error| error.to_string())?;
                finished_for_run.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        )
        .await;

        let why = slow.expect_err("a caller must not inherit a full-source read");
        assert!(
            is_sidecar_pending_error(&why),
            "the refusal names a pending artifact rather than a failure, got {why}"
        );

        // Nothing was cancelled: releasing the extraction still publishes.
        release_tx.send(()).expect("release the extraction");
        for _ in 0..200 {
            if finished.load(std::sync::atomic::Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            finished.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "bounding the wait must not abandon the extraction"
        );

        // And the next caller finds it warm, which is the whole point of
        // letting the detached extraction run on. This one waits on the
        // unbounded budget on purpose: it is asserting that the work was not
        // abandoned, not racing the publish that proves it.
        let warm = ensure_vtt_bounded(
            dir.path(),
            &file,
            0,
            ExtractionLimits::default(),
            move |_, _, _| async move {
                Err("a published sidecar must not be re-extracted".into())
            },
        )
        .await
        .expect("the published sidecar is served to the next caller");
        assert_eq!(
            tokio::fs::read(&warm).await.expect("published sidecar"),
            b"WEBVTT\n\ncold\n"
        );
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

/// The burn consumer of the subtitle-source store.
///
/// Nothing in this build writes the store, so every store here is written by
/// hand — but the `.sup` in it is produced exactly as the ride-along will
/// produce it, because the claim under test is that the two paths put the
/// cues at the same times.
#[cfg(test)]
mod stored_source_tests {
    use super::*;
    use crate::subtitle_source::testing::{kept, settled, stamp_of, write_manifest};
    use crate::subtitle_source::{
        self as store, RepresentationEntry, RepresentationFormat, RepresentationOrigin,
        StoreAccess, TrackEntry, TrackKind, Verdict,
    };
    use sha2::Digest;

    fn file_at(id: i64, path: PathBuf) -> MediaFile {
        let metadata = std::fs::metadata(&path).expect("source metadata");
        MediaFile {
            downloaded_subtitles: Vec::new(),
            id,
            item_id: 1,
            size: metadata.len() as i64,
            mtime: crate::fragment_index_cluster::source_stamp(&metadata).mtime,
            path,
            duration_ms: Some(90_000),
            container: Some("mkv".into()),
            video_codec: Some("mpeg4".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: None,
            width: Some(320),
            height: Some(180),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: None,
            audio_streams: vec![],
            subtitle_streams: vec![plurx_core::domain::SubtitleStream {
                index: 1,
                codec: "hdmv_pgs_subtitle".into(),
                ..Default::default()
            }],
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    fn run(command: &mut std::process::Command, what: &str) -> std::process::Output {
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("{what}: {error}"));
        assert!(
            output.status.success(),
            "{what} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    /// A PGS track whose first cue is at 60 s, muxed under a video whose
    /// container starts at `start` seconds. `-copyts` so the times survive the
    /// mux; the design's §6.2 fact 10 was only true of fixtures that kept them.
    fn source_with_first_cue_at_sixty(dir: &Path, start: &str) -> PathBuf {
        let authored = dir.join("authored.sup");
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/mkpgs");
        run(
            std::process::Command::new(script)
                .args(["1920", "1080"])
                .arg(&authored),
            "author the PGS track",
        );
        let source = dir.join(format!("source-{start}.mkv"));
        run(
            std::process::Command::new(ffmpeg_bin())
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-nostdin",
                    "-y",
                    "-copyts",
                    "-f",
                    "lavfi",
                    "-itsoffset",
                    start,
                    "-i",
                    "color=c=black:s=320x180:r=10:d=90",
                    // The authored track's first cue is at 1 s.
                    "-itsoffset",
                    "59",
                    "-i",
                ])
                .arg(&authored)
                .args([
                    "-map", "0:v:0", "-map", "1:s:0", "-c:v", "mpeg4", "-c:s", "copy",
                ])
                .arg(&source),
            "mux the fixture",
        );
        source
    }

    /// The `.sup` exactly as the producer will write it: `-c:s copy -f sup`,
    /// and no `-copyts`.
    fn ride_along_sup(dir: &Path, source: &Path) -> Vec<u8> {
        let sup = dir.join("ride-along.sup");
        run(
            std::process::Command::new(ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
                .arg(source)
                .args(["-map", "0:s:0", "-c:s", "copy", "-f", "sup"])
                .arg(&sup),
            "the ride-along extraction",
        );
        std::fs::read(sup).expect("ride-along bytes")
    }

    fn probe(path: &Path, entries: &[&str]) -> String {
        let output = run(
            std::process::Command::new(crate::ffmpeg::ffprobe_bin())
                .args(["-v", "error"])
                .args(entries)
                .args(["-of", "csv=p=0"])
                .arg(path),
            "ffprobe",
        );
        String::from_utf8(output.stdout).expect("utf8")
    }

    fn container_start(path: &Path) -> f64 {
        probe(path, &["-show_entries", "format=start_time"])
            .trim()
            .parse()
            .expect("start time")
    }

    /// Every subtitle packet's PTS, in order, as ffprobe prints it.
    fn cue_times(path: &Path) -> Vec<String> {
        probe(
            path,
            &["-select_streams", "s:0", "-show_entries", "packet=pts_time"],
        )
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
    }

    fn burned(answer: BurnSource, to: &Path) -> Vec<String> {
        let BurnSource::File(mut handle) = answer else {
            panic!("expected a sidecar, the store answered nothing to burn");
        };
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut handle, &mut bytes).expect("sidecar bytes");
        std::fs::write(to, bytes).expect("sidecar copy");
        cue_times(to)
    }

    fn on(root: &Path) -> StoreAccess {
        StoreAccess::new(root.to_owned(), true)
    }

    fn text_store(base: &Path, file_id: i64, verdict: Verdict) -> (MediaFile, StoreAccess) {
        let source = base.join("source.mkv");
        std::fs::write(&source, b"source bytes").expect("source");
        let file = file_at(file_id, source.clone());
        let root = base.join("runtime").join(store::STORE_DIR);
        let dir = store::file_dir(&root, file_id);
        std::fs::create_dir_all(&dir).expect("store dir");
        let bytes = b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhello\n";
        let sha256 = hex::encode(sha2::Sha256::digest(bytes));
        let name = store::representation_file_name(0, RepresentationFormat::Webvtt, &sha256)
            .expect("name");
        if verdict == Verdict::Kept {
            std::fs::write(dir.join(&name), bytes).expect("VTT");
        }
        write_manifest(
            &root,
            file_id,
            stamp_of(&source),
            vec![TrackEntry {
                ordinal: 0,
                kind: TrackKind::Text,
                representations: vec![RepresentationEntry {
                    format: RepresentationFormat::Webvtt,
                    origin: RepresentationOrigin::Extracted,
                    verdict,
                    attempts: 1,
                    file: (verdict == Verdict::Kept).then_some(name),
                    sha256: (verdict == Verdict::Kept).then_some(sha256),
                    bytes: if verdict == Verdict::Kept {
                        bytes.len() as u64
                    } else {
                        0
                    },
                }],
                verdict,
                attempts: 1,
                file: None,
                sha256: None,
            }],
        );
        (file, on(&root))
    }

    #[tokio::test]
    async fn store_answer_does_not_start_vtt_flight() {
        let base = crate::test_tempdir().expect("fixture");
        let (file, access) = text_store(base.path(), 93_001, Verdict::Kept);
        let cache = base.path().join("subs");
        let cached = vtt_path(&cache, &file, 0);
        remember_failure(&cached, "a flight would hit this memo", NEGATIVE_TTL).await;
        let path = ensure_vtt_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
            .await
            .expect("stored VTT");
        assert_eq!(
            std::fs::read(path).expect("sidecar"),
            b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhello\n"
        );
        assert_eq!(remembered_failure(&cached).await, None);
        assert!(!extractions().lock().await.contains_key(&cached));
    }

    #[tokio::test]
    async fn webvtt_store_lookup_never_targets_burn_or_window_key() {
        let base = crate::test_tempdir().expect("fixture");
        let (file, access) = text_store(base.path(), 93_002, Verdict::Kept);
        let cache = base.path().join("subs");
        ensure_vtt_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
            .await
            .expect("stored VTT");
        assert!(!vtt_window_path(&cache, &file, 0, 0, 200).exists());
        assert!(!cache.join(format!("f{}-s0-burn-v2.mks", file.id)).exists());
        assert_eq!(std::fs::read_dir(cache).expect("cache").count(), 1);
    }

    #[tokio::test]
    async fn empty_stored_text_publishes_webvtt_without_flight() {
        let base = crate::test_tempdir().expect("fixture");
        let (file, access) = text_store(base.path(), 93_003, Verdict::Empty);
        let cache = base.path().join("subs");
        let cached = vtt_path(&cache, &file, 0);
        remember_failure(&cached, "a flight would hit this memo", NEGATIVE_TTL).await;
        let path = ensure_vtt_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
            .await
            .expect("empty VTT");
        assert_eq!(std::fs::read(path).expect("sidecar"), b"WEBVTT\n\n");
        assert_eq!(remembered_failure(&cached).await, None);
        assert!(!extractions().lock().await.contains_key(&cached));
    }

    #[tokio::test]
    async fn styled_ass_stored_matroska_burn_matches_source_cues_and_style() {
        crate::transcode::require_ffmpeg();
        let base = crate::test_tempdir().expect("fixture");
        let authored = base.path().join("styled.ass");
        std::fs::write(&authored, "[Script Info]\nScriptType: v4.00+\nPlayResX: 320\nPlayResY: 180\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,24,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.50,0:00:03.50,Default,,0,0,0,,{\\pos(120,90)}Positioned caption\n").expect("ASS");
        let source = base.path().join("styled.mkv");
        run(
            std::process::Command::new(ffmpeg_bin())
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-nostdin",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=c=black:s=320x180:r=10:d=5",
                    "-i",
                ])
                .arg(&authored)
                .args([
                    "-map", "0:v:0", "-map", "1:s:0", "-c:v", "mpeg4", "-c:s", "ass",
                ])
                .arg(&source),
            "mux styled ASS fixture",
        );
        let file = file_at(93_004, source.clone());
        let direct_cache = base.path().join("direct");
        let direct = ensure_burn_source(
            &direct_cache,
            &file,
            0,
            None,
            SIDECAR_JOIN_UNBOUNDED,
            &StoreAccess::off(),
            crate::process_control::ChildClass::Background,
        )
        .await
        .expect("direct burn");
        let BurnSource::File(mut direct) = direct else {
            panic!("direct burn file")
        };
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut direct, &mut bytes).expect("direct bytes");
        assert!(bytes
            .windows(b"Positioned caption".len())
            .any(|part| part == b"Positioned caption"));
        let sha256 = hex::encode(sha2::Sha256::digest(&bytes));
        let name = store::representation_file_name(0, RepresentationFormat::Matroska, &sha256)
            .expect("name");
        let root = base.path().join("runtime").join(store::STORE_DIR);
        let local = store::file_dir(&root, file.id);
        std::fs::create_dir_all(&local).expect("local store");
        std::fs::write(local.join(&name), &bytes).expect("stored Matroska");
        write_manifest(
            &root,
            file.id,
            stamp_of(&source),
            vec![TrackEntry {
                ordinal: 0,
                kind: TrackKind::TextStyled,
                representations: vec![RepresentationEntry {
                    format: RepresentationFormat::Matroska,
                    origin: RepresentationOrigin::Extracted,
                    verdict: Verdict::Kept,
                    attempts: 1,
                    file: Some(name),
                    sha256: Some(sha256),
                    bytes: bytes.len() as u64,
                }],
                verdict: Verdict::Kept,
                attempts: 1,
                file: None,
                sha256: None,
            }],
        );
        let stored_cache = base.path().join("stored");
        let stored = ensure_burn_source(
            &stored_cache,
            &file,
            0,
            None,
            SIDECAR_JOIN_UNBOUNDED,
            &on(&root),
            crate::process_control::ChildClass::Background,
        )
        .await
        .expect("stored burn");
        let BurnSource::File(mut stored) = stored else {
            panic!("stored burn file")
        };
        let mut from_store = Vec::new();
        std::io::Read::read_to_end(&mut stored, &mut from_store).expect("stored bytes");
        assert_eq!(from_store, bytes, "styled Matroska is copied intact");
    }

    /// The design's required fixture (§6.2 fact 10, §6.7 item 1).
    ///
    /// The derived `.mks` must put every cue exactly where today's source
    /// extraction does, on a zero and a non-zero container start — and the
    /// `-start_at_zero` variant, the source path's own argv and the obvious
    /// thing to "harmonise" the derivation to, must not, so this cannot pass
    /// on a fixture that happens to start its first cue at zero.
    #[tokio::test]
    async fn a_derived_burn_sidecar_keeps_the_source_extraction_cue_times() {
        crate::transcode::require_ffmpeg();
        let _counters = crate::subtitle_source::testing::counter_lock().lock().await;
        for (file_id, start) in [(91_001, "0"), (91_002, "7.5")] {
            let base = crate::test_tempdir().expect("fixture");
            let source = source_with_first_cue_at_sixty(base.path(), start);
            let offset = container_start(&source);
            if start == "0" {
                assert_eq!(offset, 0.0, "the zero-start fixture starts at zero");
            } else {
                assert!(offset > 7.0, "the fixture starts late: {offset}");
            }
            let file = file_at(file_id, source.clone());

            let today = burned(
                ensure_burn_source(
                    &base.path().join("today"),
                    &file,
                    0,
                    None,
                    SIDECAR_JOIN_UNBOUNDED,
                    &StoreAccess::off(),
                    crate::process_control::ChildClass::Background,
                )
                .await
                .expect("today's extraction"),
                &base.path().join("today.mks"),
            );
            assert_eq!(
                today.len(),
                4,
                "two cues, each shown and cleared: {today:?}"
            );
            let first: f64 = today[0].parse().expect("first cue");
            assert!(
                (first - (60.0 - offset)).abs() < 0.001,
                "today's first cue sits at 60 s less the container start: {today:?}"
            );

            let root = base.path().join("runtime").join(store::STORE_DIR);
            let ride = ride_along_sup(base.path(), &source);
            let dir = store::file_dir(&root, file_id);
            write_manifest(
                &root,
                file_id,
                stamp_of(&source),
                vec![kept(&dir, 0, &ride)],
            );
            let derived = burned(
                ensure_burn_source(
                    &base.path().join("derived"),
                    &file,
                    0,
                    None,
                    SIDECAR_JOIN_UNBOUNDED,
                    &on(&root),
                    crate::process_control::ChildClass::Background,
                )
                .await
                .expect("derived sidecar"),
                &base.path().join("derived.mks"),
            );
            assert_eq!(
                burn_routes_for_test(file_id),
                vec!["source", "store"],
                "the second sidecar came from the store, not the source"
            );
            assert_eq!(derived, today, "container start {start}");

            // The variant this must not become.
            let stored = base.path().join("stored.sup");
            std::fs::write(&stored, &ride).expect("stored copy");
            let mut args = burn_derivation_args(&stored);
            let copyts = args
                .iter()
                .position(|arg| arg == "-copyts")
                .expect("the derivation keeps -copyts");
            assert!(
                !args.iter().any(|arg| arg == "-start_at_zero"),
                "the derivation must not carry -start_at_zero"
            );
            args.insert(copyts + 1, "-start_at_zero".into());
            let variant = base.path().join("start-at-zero.mks");
            *args.last_mut().expect("output") = variant.clone().into_os_string();
            run(
                std::process::Command::new(ffmpeg_bin()).args(&args),
                "the -start_at_zero variant",
            );
            let shifted = cue_times(&variant);
            assert_ne!(
                shifted, today,
                "with -start_at_zero every cue moves earlier by the first cue's time"
            );
            assert_eq!(shifted.first().map(String::as_str), Some("0.000000"));
        }
    }

    /// `empty` answers "nothing to burn" without reading the source, and the
    /// off switch makes the same store invisible.
    #[tokio::test]
    async fn an_empty_stored_track_burns_nothing_without_reading_the_source() {
        crate::transcode::require_ffmpeg();
        let _counters = crate::subtitle_source::testing::counter_lock().lock().await;
        let base = crate::test_tempdir().expect("fixture");
        let source = source_with_first_cue_at_sixty(base.path(), "0");
        let file_id = 91_003;
        let file = file_at(file_id, source.clone());
        let root = base.path().join("runtime").join(store::STORE_DIR);
        let dir = write_manifest(
            &root,
            file_id,
            stamp_of(&source),
            vec![settled(0, Verdict::Empty)],
        );
        let cache = base.path().join("subs");

        let answer = ensure_burn_source(
            &cache,
            &file,
            0,
            None,
            SIDECAR_JOIN_UNBOUNDED,
            &on(&root),
            crate::process_control::ChildClass::Background,
        )
        .await
        .expect("an answer");
        assert!(matches!(answer, BurnSource::Nothing));
        assert!(
            burn_routes_for_test(file_id).is_empty(),
            "nothing was extracted or derived"
        );
        assert!(
            !cache.exists() || std::fs::read_dir(&cache).expect("cache").next().is_none(),
            "and nothing was published"
        );
        assert!(dir.join(".access").exists(), "an answer marks its access");

        let answer = ensure_burn_source(
            &cache,
            &file,
            0,
            None,
            SIDECAR_JOIN_UNBOUNDED,
            &StoreAccess::new(root, false),
            crate::process_control::ChildClass::Background,
        )
        .await
        .expect("off reads the source");
        assert!(matches!(answer, BurnSource::File(_)));
        assert_eq!(burn_routes_for_test(file_id), vec!["source"]);
    }

    /// Off, and an MPEG-TS source, both ignore a valid kept track and a valid
    /// empty one alike.
    #[tokio::test]
    async fn off_and_mpegts_never_use_the_store() {
        crate::transcode::require_ffmpeg();
        let _counters = crate::subtitle_source::testing::counter_lock().lock().await;
        let base = crate::test_tempdir().expect("fixture");
        let source = source_with_first_cue_at_sixty(base.path(), "0");
        let root = base.path().join("runtime").join(store::STORE_DIR);
        let ride = ride_along_sup(base.path(), &source);

        // Off, with a kept track.
        let file_id = 91_004;
        let file = file_at(file_id, source.clone());
        let dir = store::file_dir(&root, file_id);
        write_manifest(
            &root,
            file_id,
            stamp_of(&source),
            vec![kept(&dir, 0, &ride)],
        );
        let answer = ensure_burn_source(
            &base.path().join("off"),
            &file,
            0,
            None,
            SIDECAR_JOIN_UNBOUNDED,
            &StoreAccess::new(root.clone(), false),
            crate::process_control::ChildClass::Background,
        )
        .await
        .expect("off");
        assert!(matches!(answer, BurnSource::File(_)));
        assert_eq!(burn_routes_for_test(file_id), vec!["source"]);

        // MPEG-TS, with a kept track and then an empty one.
        let file_id = 91_005;
        let mut file = file_at(file_id, source.clone());
        file.container = Some("m2ts".into());
        let dir = store::file_dir(&root, file_id);
        write_manifest(
            &root,
            file_id,
            stamp_of(&source),
            vec![kept(&dir, 0, &ride)],
        );
        let before = store::misses_for_test(store::Consumer::Burn, store::MissReason::Mpegts);
        let answer = ensure_burn_source(
            &base.path().join("mpegts-kept"),
            &file,
            0,
            None,
            SIDECAR_JOIN_UNBOUNDED,
            &on(&root),
            crate::process_control::ChildClass::Background,
        )
        .await
        .expect("mpegts");
        assert!(matches!(answer, BurnSource::File(_)));
        assert_eq!(burn_routes_for_test(file_id), vec!["source"]);
        assert!(store::misses_for_test(store::Consumer::Burn, store::MissReason::Mpegts) > before);

        write_manifest(
            &root,
            file_id,
            stamp_of(&source),
            vec![settled(0, Verdict::Empty)],
        );
        let answer = ensure_burn_source(
            &base.path().join("mpegts-empty"),
            &file,
            0,
            None,
            SIDECAR_JOIN_UNBOUNDED,
            &on(&root),
            crate::process_control::ChildClass::Background,
        )
        .await
        .expect("mpegts empty");
        assert!(
            matches!(answer, BurnSource::File(_)),
            "an MPEG-TS source never takes the store's word that there is nothing"
        );
    }

    /// A derivation that fails falls back to the source inside the same
    /// call, and the failure is never remembered: a memo written for it would
    /// refuse the very extraction that answered.
    #[tokio::test]
    async fn a_failed_derivation_reads_the_source_in_the_same_flight_and_leaves_no_memo() {
        crate::transcode::require_ffmpeg();
        let _counters = crate::subtitle_source::testing::counter_lock().lock().await;
        let base = crate::test_tempdir().expect("fixture");
        let source = source_with_first_cue_at_sixty(base.path(), "0");
        let file_id = 91_006;
        let file = file_at(file_id, source.clone());
        let root = base.path().join("runtime").join(store::STORE_DIR);
        let dir = store::file_dir(&root, file_id);
        // Hash-valid, so it is opened and handed to ffmpeg, and not PGS, so
        // the derivation fails. (ffmpeg exits 0 on it with a cue-less
        // Matroska; the derivation counts what it printed as a failure.)
        write_manifest(
            &root,
            file_id,
            stamp_of(&source),
            vec![kept(&dir, 0, b"these bytes are not a PGS stream")],
        );
        let cache = base.path().join("subs");
        let before = store::fallbacks_for_test(store::Fallback::DeriveFailed);

        let today_cues = {
            let answer = ensure_burn_source(
                &cache,
                &file,
                0,
                None,
                SIDECAR_JOIN_UNBOUNDED,
                &on(&root),
                crate::process_control::ChildClass::Background,
            )
            .await
            .expect("the source answers in the same call");
            burned(answer, &base.path().join("fallback.mks"))
        };
        assert_eq!(
            burn_routes_for_test(file_id),
            vec!["source"],
            "the store was tried, failed, and the source ran in the same flight"
        );
        assert!(store::fallbacks_for_test(store::Fallback::DeriveFailed) > before);
        assert_eq!(
            today_cues.len(),
            4,
            "the source's cues, not an empty sidecar"
        );

        let version = {
            use sha2::{Digest, Sha256};
            let fence = crate::fragment_index_cluster::open_source_fence(&file, None)
                .await
                .expect("fence");
            hex::encode(Sha256::digest(fence.object_version().as_bytes()))
        };
        let cached = cache.join(format!("f{file_id}-s0-{version}-burn-v2.mks"));
        // The route assertion above is what catches a failed derivation
        // being made terminal; a memo check here could not fail, because the
        // successful source extraction clears the memo before it returns.
        // `a_double_failure_remembers_the_source_error_and_runs_neither_again`
        // is where the memo's contents are pinned.
        assert!(cached.exists(), "the source extraction was published");
        let mut entries = std::fs::read_dir(&cache).expect("cache");
        assert!(
            !entries.any(|entry| entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".tmp-")),
            "and no temp file"
        );
    }

    /// The published burn sidecar's path for `file`, as the flight keys it.
    async fn cached_burn(cache: &Path, file: &MediaFile) -> PathBuf {
        use sha2::{Digest, Sha256};
        let fence = crate::fragment_index_cluster::open_source_fence(file, None)
            .await
            .expect("fence");
        cache.join(format!(
            "f{}-s0-{}-burn-v2.mks",
            file.id,
            hex::encode(Sha256::digest(fence.object_version().as_bytes()))
        ))
    }

    /// A derivation that never returns is killed at its own bound and the
    /// source answers in the same call. Without that bound it would run the
    /// flight's timeout out — shrunk to five seconds here — and *that*
    /// timeout is memoised, refusing the track for the memo's whole TTL: the
    /// one route by which a derivation failure could reach the memo.
    #[tokio::test]
    async fn a_hung_derivation_is_bounded_and_falls_back_to_the_source() {
        crate::transcode::require_ffmpeg();
        let _counters = crate::subtitle_source::testing::counter_lock().lock().await;
        let base = crate::test_tempdir().expect("fixture");
        let source = source_with_first_cue_at_sixty(base.path(), "0");
        let file_id = 91_007;
        let file = file_at(file_id, source.clone());
        let root = base.path().join("runtime").join(store::STORE_DIR);
        let dir = store::file_dir(&root, file_id);
        let ride = ride_along_sup(base.path(), &source);
        write_manifest(
            &root,
            file_id,
            stamp_of(&source),
            vec![kept(&dir, 0, &ride)],
        );
        let cache = base.path().join("subs");
        let before = store::fallbacks_for_test(store::Fallback::TimedOut);

        let answer = Box::pin(ensure_burn_source_with(
            &cache,
            &file,
            0,
            None,
            &on(&root),
            ExtractionLimits {
                max_sidecar_bytes: MAX_BURN_BYTES,
                join_budget: SIDECAR_JOIN_UNBOUNDED,
                timeout: Duration::from_secs(5),
                ..Default::default()
            },
            Derivation {
                timeout: Duration::from_millis(200),
                hang: true,
            },
            crate::process_control::ChildClass::Background,
        ))
        .await
        .expect("the source answers once the derivation is cut off");
        assert!(matches!(answer, BurnSource::File(_)));
        assert_eq!(
            burn_routes_for_test(file_id),
            vec!["source"],
            "the hung derivation gave way to the source in the same flight"
        );
        assert!(store::fallbacks_for_test(store::Fallback::TimedOut) > before);
        let cached = cached_burn(&cache, &file).await;
        assert!(cached.exists(), "the source extraction was published");
    }

    /// A derivation that fails and a source extraction that then fails too:
    /// the memo holds the *source's* error — the flight's final answer — and
    /// the next call is refused from it without running either.
    #[tokio::test]
    async fn a_double_failure_remembers_the_source_error_and_runs_neither_again() {
        crate::transcode::require_ffmpeg();
        let _counters = crate::subtitle_source::testing::counter_lock().lock().await;
        let base = crate::test_tempdir().expect("fixture");
        // A source with no subtitle stream at all, so `-map 0:s:0` fails.
        let source = base.path().join("no-subtitles.mkv");
        run(
            std::process::Command::new(ffmpeg_bin())
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-nostdin",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=c=black:s=320x180:r=1:d=5",
                    "-c:v",
                    "mpeg4",
                ])
                .arg(&source),
            "mux a source without subtitles",
        );
        let file_id = 91_008;
        let file = file_at(file_id, source.clone());
        let root = base.path().join("runtime").join(store::STORE_DIR);
        let dir = store::file_dir(&root, file_id);
        write_manifest(
            &root,
            file_id,
            stamp_of(&source),
            vec![kept(&dir, 0, b"these bytes are not a PGS stream")],
        );
        let cache = base.path().join("subs");
        let limits = ExtractionLimits {
            max_sidecar_bytes: MAX_BURN_BYTES,
            join_budget: SIDECAR_JOIN_UNBOUNDED,
            negative_ttl: Duration::from_secs(60),
            ..Default::default()
        };
        let derivations_before = store::fallbacks_for_test(store::Fallback::DeriveFailed);

        let why = match Box::pin(ensure_burn_source_with(
            &cache,
            &file,
            0,
            None,
            &on(&root),
            limits,
            Derivation::default(),
            crate::process_control::ChildClass::Background,
        ))
        .await
        {
            Err(why) => why,
            Ok(_) => panic!("neither route can produce a sidecar"),
        };
        assert!(
            why.starts_with("burn-track extraction failed"),
            "the caller sees the source's error, not the derivation's: {why}"
        );
        assert_eq!(burn_routes_for_test(file_id), vec!["source"]);
        let derivations = store::fallbacks_for_test(store::Fallback::DeriveFailed);
        assert_eq!(derivations, derivations_before + 1, "one derivation ran");
        let cached = cached_burn(&cache, &file).await;
        let remembered = remembered_failure(&cached).await.expect("a memo");
        assert!(
            remembered.starts_with("burn-track extraction failed"),
            "the memo is the source's error: {remembered}"
        );

        let again = Box::pin(ensure_burn_source_with(
            &cache,
            &file,
            0,
            None,
            &on(&root),
            limits,
            Derivation::default(),
            crate::process_control::ChildClass::Background,
        ))
        .await;
        assert!(matches!(again, Err(ref why) if why == &remembered));
        assert_eq!(
            burn_routes_for_test(file_id),
            vec!["source"],
            "the memo refused the call before either route ran"
        );
        assert_eq!(
            store::fallbacks_for_test(store::Fallback::DeriveFailed),
            derivations,
            "and no second derivation"
        );
        forget_failure(&cached).await;
    }

    /// A `kept` lookup is counted once per call even when the caller joins a
    /// flight someone else owns and never opens the stored bytes itself.
    #[tokio::test]
    async fn a_kept_lookup_that_joins_another_flight_is_still_counted() {
        crate::transcode::require_ffmpeg();
        let _counters = crate::subtitle_source::testing::counter_lock().lock().await;
        let base = crate::test_tempdir().expect("fixture");
        let source = source_with_first_cue_at_sixty(base.path(), "0");
        let file_id = 91_009;
        let file = file_at(file_id, source.clone());
        let root = base.path().join("runtime").join(store::STORE_DIR);
        let dir = store::file_dir(&root, file_id);
        let ride = ride_along_sup(base.path(), &source);
        write_manifest(
            &root,
            file_id,
            stamp_of(&source),
            vec![kept(&dir, 0, &ride)],
        );
        let cache = base.path().join("subs");
        let access = on(&root);
        let before = store::hits_for_test(store::Consumer::Burn);

        let (first, second) = tokio::join!(
            Box::pin(ensure_burn_source(
                &cache,
                &file,
                0,
                None,
                SIDECAR_JOIN_UNBOUNDED,
                &access,
                crate::process_control::ChildClass::Background
            )),
            Box::pin(ensure_burn_source(
                &cache,
                &file,
                0,
                None,
                SIDECAR_JOIN_UNBOUNDED,
                &access,
                crate::process_control::ChildClass::Background
            )),
        );
        assert!(matches!(first, Ok(BurnSource::File(_))));
        assert!(matches!(second, Ok(BurnSource::File(_))));
        assert_eq!(
            burn_routes_for_test(file_id),
            vec!["store"],
            "one derivation served both callers"
        );
        assert_eq!(
            store::hits_for_test(store::Consumer::Burn),
            before + 2,
            "both lookups counted, the joiner's included"
        );
    }
}

#[cfg(test)]
mod cluster_consumer_tests {
    use super::*;
    use plurx_core::domain::{ItemKind, NewItem, NewLibrary, ProbeResult, SubtitleStream};
    use plurx_core::store::{
        keys, ClusterFragmentIndexStore, LibraryStore, MediaStore, SettingsStore, SqliteStore,
        SubtitleSourcePublication, SubtitleSourceStamp,
    };

    /// Plan P-02 §3.2.2, review of #518 finding 1: a whole-track extraction
    /// runs at the class its caller passes. The `/subs` request a viewer's
    /// player waits on is realtime; the native-HLS warm-up nobody waits on is
    /// background. Before the fix every whole-track extraction was background.
    #[tokio::test]
    async fn a_whole_track_extraction_runs_at_the_class_its_caller_passes() {
        use crate::process_control::{priority::spawns_of, ChildClass};
        use crate::subtitle_ride_along::testing::{fixture, Sub};

        let fixture = fixture(97_002, &[Sub::Srt]);
        let store = SqliteStore::open_in_memory().expect("SQLite");
        let file = catalogued_text_source(&store, &fixture.source).await;
        let off = crate::subtitle_source::StoreAccess::off();

        let viewer = crate::http::stream::SUBTITLE_TRACK_FOR_A_VIEWER;
        assert_eq!(viewer.class, ChildClass::Realtime);
        let before = spawns_of(viewer);
        let bytes =
            ensure_vtt_bytes_with_store(&fixture.dir.path().join("viewer"), &file, 0, &off, viewer)
                .await
                .expect("the viewer's track");
        assert!(std::str::from_utf8(&bytes).expect("VTT").contains("hello"));
        assert!(
            spawns_of(viewer) > before,
            "the viewer's extraction was not started at the realtime class"
        );

        assert_eq!(SUBTITLE_WARM_UP.class, ChildClass::Background);
        let before = spawns_of(SUBTITLE_WARM_UP);
        let warm = fixture.dir.path().join("warm");
        warm_vtt_with_store(&warm, &file, 0, &off).await;
        let published = vtt_path(&warm, &file, 0);
        tokio::time::timeout(Duration::from_secs(30), async {
            while !valid_sidecar(&published, MAX_SIDECAR_BYTES).await {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the detached warm-up published its sidecar");
        assert!(
            spawns_of(SUBTITLE_WARM_UP) > before,
            "the warm-up's extraction was not started at the background class"
        );
    }

    async fn catalogued_text_source(store: &SqliteStore, source: &Path) -> MediaFile {
        let library = store
            .create_library(&NewLibrary {
                name: "Cluster subtitle consumer".to_owned(),
                kind: plurx_core::domain::LibraryKind::Movies,
                paths: vec![source.parent().expect("parent").to_owned()],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Subtitle fixture".to_owned(),
                year: Some(2026),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let metadata = std::fs::metadata(source).expect("source metadata");
        let stamp = crate::fragment_index_cluster::source_stamp(&metadata);
        let probe = ProbeResult {
            container: Some("mkv".to_owned()),
            subtitle_streams: vec![SubtitleStream {
                index: 0,
                codec: "subrip".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let id = store
            .upsert_file(
                item,
                source.to_str().expect("source path"),
                metadata.len() as i64,
                stamp.mtime,
                &probe,
            )
            .await
            .expect("catalogue source");
        MediaFile {
            id,
            item_id: item,
            path: source.to_owned(),
            size: metadata.len() as i64,
            mtime: stamp.mtime,
            duration_ms: Some(16_000),
            container: probe.container,
            video_codec: Some("h264".to_owned()),
            video_codec_tag: None,
            field_order: None,
            video_profile: None,
            width: Some(64),
            height: Some(64),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            dolby_vision: Default::default(),
            bitrate: None,
            audio_streams: Vec::new(),
            subtitle_streams: probe.subtitle_streams,
            downloaded_subtitles: Vec::new(),
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
        }
    }

    async fn access(store: Arc<SqliteStore>, base: &Path) -> crate::subtitle_source::StoreAccess {
        for key in [
            keys::VOD_INDEX_CLUSTER_CACHE,
            keys::SUBTITLE_CLUSTER_SOURCES,
            keys::SUBTITLE_STORED_SOURCES,
        ] {
            store.put_setting(key, "1").await.expect("enable fixture");
        }
        let jobs = crate::state::JobManager::test_new(store.clone(), base.join("artwork"));
        crate::subtitle_source::StoreAccess::from_setting(store, &base.join("runtime"))
            .on_node(Some("test-node"))
            .with_jobs(jobs)
    }

    async fn source_and_access(
        base: &Path,
    ) -> (
        Arc<SqliteStore>,
        MediaFile,
        crate::subtitle_source::StoreAccess,
    ) {
        let source = base.join("source.mkv");
        std::fs::write(&source, b"not needed by queue-state tests").expect("source");
        let store = Arc::new(SqliteStore::open_in_memory().expect("SQLite"));
        let file = catalogued_text_source(&store, &source).await;
        let access = access(Arc::clone(&store), base).await;
        (store, file, access)
    }

    async fn stamp(file: &MediaFile) -> SubtitleSourceStamp {
        SubtitleSourceStamp {
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            pipeline_version: crate::state::subtitle_source_pipeline_version().await,
        }
    }

    async fn source_digest(file: &MediaFile) -> String {
        crate::fragment_index_cluster::attest_source("test-node", file, None, &|_| {})
            .await
            .expect("attest fixture")
            .observation
            .source_sha256
    }

    #[tokio::test]
    async fn cancelled_job_memos_for_negative_ttl_without_reenqueue() {
        let base = crate::test_tempdir().expect("fixture");
        let (store, file, access) = source_and_access(base.path()).await;
        let request = store
            .enqueue_or_promote_subtitle_source(
                &stamp(&file).await,
                "foreground",
                subtitle_clock_ms(),
            )
            .await
            .expect("enqueue")
            .expect("request");
        store
            .cancel_analysis_request_admin(&request.request_id, subtitle_clock_ms())
            .await
            .expect("cancel request");
        let cache = base.path().join("subs");
        let first = ensure_vtt_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
            .await
            .expect_err("operator cancellation is terminal for this flight");
        assert!(first.contains("cancelled"));
        let cached = vtt_path(&cache, &file, 0);
        assert_eq!(
            remembered_failure(&cached).await.as_deref(),
            Some(first.as_str())
        );
        let second = ensure_vtt_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
            .await
            .expect_err("negative memo refuses immediate retry");
        assert_eq!(second, first);
        let rows = store.analysis_requests(10).await.expect("requests");
        assert_eq!(rows.len(), 1, "memo cannot fork a second producer");
        assert_eq!(rows[0].state, "cancelled");
        assert!(!rows[0].force_rebuild);
    }

    #[tokio::test(start_paused = true)]
    async fn unreachable_holders_release_inline_after_claim_wait_without_second_producer() {
        let base = crate::test_tempdir().expect("fixture");
        let (store, file, access) = source_and_access(base.path()).await;
        store
            .upsert_subtitle_source_publication(&SubtitleSourcePublication {
                file_id: file.id,
                source_size: file.size,
                source_mtime: file.mtime,
                source_attestation: source_digest(&file).await,
                node_id: "unreachable-node".to_owned(),
                ordinal: 0,
                kind: "text".to_owned(),
                format: "webvtt".to_owned(),
                verdict: "kept".to_owned(),
                attempts: 1,
                origin: "extracted".to_owned(),
                sha256: "b".repeat(64),
                bytes: 25,
                published_at_ms: subtitle_clock_ms(),
            })
            .await
            .expect("holder publication");
        let tmp = base.path().join("candidate.vtt");
        assert!(!cluster_vtt_into(&tmp, &file, 0, &access)
            .await
            .expect("release to inline"));
        assert!(store
            .analysis_requests(10)
            .await
            .expect("requests")
            .is_empty());
        assert!(
            !tmp.exists(),
            "cluster preference did not fabricate a sidecar"
        );
    }

    #[tokio::test]
    async fn terminal_failed_row_releases_inline_path() {
        let base = crate::test_tempdir().expect("fixture");
        let (store, file, access) = source_and_access(base.path()).await;
        let now = subtitle_clock_ms();
        let request = store
            .enqueue_or_promote_subtitle_source(&stamp(&file).await, "foreground", now)
            .await
            .expect("enqueue")
            .expect("request");
        let claimed = store
            .claim_analysis_request_foreground(&request.request_id, "test-node", now, now + 120_000)
            .await
            .expect("claim")
            .expect("running request");
        assert!(store
            .fail_analysis_request(
                &claimed.request_id,
                "test-node",
                claimed.fence,
                "stored_probe_invalid",
                now
            )
            .await
            .expect("fail request"));
        let tmp = base.path().join("candidate.vtt");
        assert!(!cluster_vtt_into(&tmp, &file, 0, &access)
            .await
            .expect("terminal failure releases inline"));
        let rows = store.analysis_requests(10).await.expect("requests");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, "failed");
        assert!(!rows[0].force_rebuild);
    }

    #[tokio::test]
    async fn source_empty_publication_completes_vtt_without_inline_extraction() {
        let base = crate::test_tempdir().expect("fixture");
        let (store, file, access) = source_and_access(base.path()).await;
        store
            .upsert_subtitle_source_publication(&SubtitleSourcePublication {
                file_id: file.id,
                source_size: file.size,
                source_mtime: file.mtime,
                source_attestation: source_digest(&file).await,
                node_id: "producer".to_owned(),
                ordinal: 0,
                kind: "text".to_owned(),
                format: "webvtt".to_owned(),
                verdict: "empty".to_owned(),
                attempts: 1,
                origin: "extracted".to_owned(),
                sha256: String::new(),
                bytes: 0,
                published_at_ms: subtitle_clock_ms(),
            })
            .await
            .expect("empty publication");
        let cache = base.path().join("subs");
        let path = ensure_vtt_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
            .await
            .expect("empty representation");
        assert_eq!(std::fs::read(path).expect("sidecar"), b"WEBVTT\n\n");
        assert!(store
            .analysis_requests(10)
            .await
            .expect("requests")
            .is_empty());
    }

    #[tokio::test]
    async fn direct_text_and_offline_cold_queued_requests_complete_without_500() {
        use crate::subtitle_ride_along::testing::{fixture, Sub};

        let fixture = fixture(97_000, &[Sub::Srt]);
        let store = Arc::new(SqliteStore::open_in_memory().expect("SQLite"));
        let file = catalogued_text_source(&store, &fixture.source).await;
        let access = access(Arc::clone(&store), fixture.dir.path()).await;
        let jobs = Arc::clone(access.jobs().expect("queue worker"));
        let cache = fixture.dir.path().join("subs");

        let direct = {
            let access = access.clone();
            let file = file.clone();
            let cache = cache.clone();
            tokio::spawn(async move {
                ensure_vtt_bytes_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
                    .await
            })
        };
        let offline = {
            let access = access.clone();
            let file = file.clone();
            let cache = cache.clone();
            tokio::spawn(async move {
                ensure_vtt_file_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
                    .await
            })
        };
        let request = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(request) = store
                    .analysis_requests(10)
                    .await
                    .expect("queued requests")
                    .into_iter()
                    .next()
                {
                    break request;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("consumer queued foreground work");
        assert_eq!(request.priority, "foreground");
        assert!(!request.force_rebuild);
        assert!(
            jobs.self_claim_subtitle_source(
                &request.request_id,
                &fixture.dir.path().join("runtime")
            )
            .await
        );
        let direct = direct
            .await
            .expect("direct task")
            .expect("direct VTT bytes");
        let mut offline = offline
            .await
            .expect("offline task")
            .expect("offline VTT file");
        let mut offline_bytes = Vec::new();
        std::io::Read::read_to_end(&mut offline, &mut offline_bytes).expect("offline VTT bytes");
        assert_eq!(direct, offline_bytes);
        assert!(std::str::from_utf8(&direct).expect("VTT").contains("hello"));
        let rows = store.analysis_requests(10).await.expect("requests");
        assert_eq!(rows.len(), 1, "both callers joined one producer");
        assert_eq!(rows[0].state, "ready");
    }

    #[tokio::test]
    async fn cold_hls_rendition_warm_enqueues_and_completes_queued_source() {
        use crate::subtitle_ride_along::testing::{fixture, Sub};

        let fixture = fixture(97_001, &[Sub::Srt]);
        let store = Arc::new(SqliteStore::open_in_memory().expect("SQLite"));
        let file = catalogued_text_source(&store, &fixture.source).await;
        let access = access(Arc::clone(&store), fixture.dir.path()).await;
        let jobs = Arc::clone(access.jobs().expect("queue worker"));
        let cache = fixture.dir.path().join("hls-subs");

        // The HLS segment returns while its detached warmer owns the flight.
        warm_vtt_with_store(&cache, &file, 0, &access).await;
        let request = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(request) = store
                    .analysis_requests(10)
                    .await
                    .expect("queued requests")
                    .into_iter()
                    .next()
                {
                    break request;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("detached HLS warm enqueued its source");
        assert_eq!(request.priority, "foreground");
        assert!(!request.force_rebuild);
        assert!(
            jobs.self_claim_subtitle_source(
                &request.request_id,
                &fixture.dir.path().join("runtime")
            )
            .await
        );
        let sidecar = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let sidecar = vtt_path(&cache, &file, 0);
                if valid_sidecar(&sidecar, MAX_SIDECAR_BYTES).await {
                    break sidecar;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("queue result reached the detached HLS warmer");
        assert!(std::fs::read_to_string(sidecar)
            .expect("VTT")
            .contains("hello"));
        assert_eq!(
            store.analysis_requests(10).await.expect("requests").len(),
            1
        );
    }

    #[tokio::test(start_paused = true)]
    async fn remote_running_beyond_wait_bound_never_starts_inline_producer() {
        let base = crate::test_tempdir().expect("fixture");
        let (store, file, access) = source_and_access(base.path()).await;
        let now = subtitle_clock_ms();
        let request = store
            .enqueue_or_promote_subtitle_source(&stamp(&file).await, "foreground", now)
            .await
            .expect("enqueue")
            .expect("request");
        store
            .claim_analysis_request_foreground(
                &request.request_id,
                "remote-worker",
                now,
                now + 1_800_000,
            )
            .await
            .expect("claim")
            .expect("running row");

        let cache = base.path().join("vtt-cache");
        let error = tokio::time::timeout(
            Duration::from_secs(670),
            ensure_vtt_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK),
        )
        .await
        .expect("bounded VTT wait")
        .expect_err("active worker cannot release inline");
        assert!(error.contains("still running"), "{error}");
        assert_eq!(
            remembered_failure(&vtt_path(&cache, &file, 0)).await,
            Some(error)
        );

        let mut pgs = file.clone();
        pgs.subtitle_streams[0].codec = "hdmv_pgs_subtitle".to_owned();
        let source = crate::fragment_index_cluster::open_source_fence(&pgs, None)
            .await
            .expect("held source");
        let error = tokio::time::timeout(
            Duration::from_secs(610),
            cluster_wait_burn(&access, &pgs, 0, &source),
        )
        .await
        .expect("bounded burn wait")
        .err()
        .expect("active worker cannot release burn inline");
        assert!(error.contains("still running"), "{error}");
        let rows = store.analysis_requests(10).await.expect("requests");
        assert_eq!(rows.len(), 1, "neither consumer forked a producer");
        assert_eq!(rows[0].state, "running");
        assert!(!rows[0].force_rebuild);
    }
}
