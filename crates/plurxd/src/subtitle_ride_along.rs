//! The subtitle-source store's producer: the fragment-index pass keeps every
//! eligible subtitle track it is already reading.
//!
//! Design: `docs/clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md` §6.3. The
//! index pass is one `ffmpeg` child that demuxes the whole file from byte zero
//! and discards the subtitle packets with `-sn`. This module adds one more
//! output to that child — a `tee` of `sup`, `webvtt`, and `matroska` slaves
//! with packet-count companions and a mandatory `null` sentinel — and decides,
//! track by track, whether what the pass wrote may be served in place of a
//! whole-source extraction. [`crate::subtitle_source`] is everything that reads
//! what this module publishes.
//!
//! The rules it keeps, each with the experiment behind it in §6.2:
//!
//! - **The index never changes.** The extra output is appended after `pipe:1`
//!   in [`crate::fragindex`]'s pass, never inside
//!   `plurx_core::transcode::copy_index_pipe_args*`, which
//!   `fragment_index_cluster::pipeline_digest_for_transform` hashes into every
//!   cluster cache key. Every slave is `onfail=ignore` and the sentinel keeps
//!   the tee alive when all of them fail, so the worst the ride-along can do
//!   is keep nothing. The index's exit code, row check and retry charging are
//!   exactly what they were.
//! - **Ordinals come from the held-fd probe of the file being read**, never
//!   from scan-time facts, and every map is hard (`0:s:N`, never `?`).
//! - **A verdict is taken only after the child is reaped**, only when the
//!   index was built, and a track is `kept` only when an independent count —
//!   the `framecrc` byte total — agrees with a walk of the `.sup` and the
//!   parser the overlay uses accepts it.
//! - **A fresh private stage per attempt**, on the store's filesystem so
//!   publishing is a rename, removed by a drop guard on every exit route.
//! - **The Developer switch controls the producer.** The startup self-test,
//!   filesystem and free-space readings are advisory in Developer settings;
//!   they never silently override the operator's saved choice.

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use plurx_core::store::{AnalysisRequest, Store, SubtitleSourcePublication};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::subtitle_source::{
    self as store, Consumer, Manifest, RepresentationEntry, RepresentationFormat,
    RepresentationOrigin, SourceStamp, TrackEntry, TrackKind, Verdict, MANIFEST_NAME,
    MANIFEST_VERSION, MAX_TRACK_BYTES,
};

/// The codec name the probe reports for a PGS track.
pub(crate) const PGS_CODEC: &str = "hdmv_pgs_subtitle";
/// The kinds emitted by the held-fd probe. Unknown and unsupported bitmap
/// codecs never enter a ride-along plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProbedKind {
    Pgs,
    Text,
    TextStyled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProbedTrack {
    pub(crate) ordinal: i64,
    pub(crate) kind: ProbedKind,
    pub(crate) stream_index: u64,
}

pub(crate) fn eligible_tracks_from_probe(raw: &str) -> Vec<ProbedTrack> {
    let Ok(document) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(streams) = document
        .get("streams")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    let mut indexed = Vec::with_capacity(streams.len());
    for stream in streams {
        let Some(index) = stream.get("index").and_then(serde_json::Value::as_u64) else {
            return Vec::new();
        };
        indexed.push((index, stream));
    }
    indexed.sort_by_key(|(index, _)| *index);
    let mut ordinal = 0_i64;
    let mut eligible = Vec::new();
    for (stream_index, stream) in indexed {
        if stream.get("codec_type").and_then(serde_json::Value::as_str) != Some("subtitle") {
            continue;
        }
        let kind = match stream.get("codec_name").and_then(serde_json::Value::as_str) {
            Some(PGS_CODEC) => Some(ProbedKind::Pgs),
            Some("subrip" | "mov_text" | "webvtt" | "text") => Some(ProbedKind::Text),
            Some("ass" | "ssa") => Some(ProbedKind::TextStyled),
            _ => None,
        };
        if let Some(kind) = kind {
            eligible.push(ProbedTrack {
                ordinal,
                kind,
                stream_index,
            });
        }
        ordinal += 1;
    }
    eligible
}
/// Stage directories live beside the store's `f<id>` directories, under a
/// name the store's sweep never mistakes for one.
const STAGE_PREFIX: &str = ".stage-";
const SELF_TEST_PREFIX: &str = ".selftest-";
/// A stage older than this that no process owns is a crash's leftover.
pub(crate) const STALE_STAGE_AGE: Duration = Duration::from_secs(60 * 60);
/// A `.sup` segment on disk: `PG` (2) + PTS (4) + DTS (4) + type (1) +
/// length (2), then the payload.
const SEGMENT_HEADER_BYTES: u64 = 13;
/// Of that header, what the packet ffmpeg carries already holds: type and
/// length. The `sup` muxer adds only the first ten bytes.
const PACKET_SEGMENT_HEADER_BYTES: u64 = 3;
/// A stderr line longer than this is kept only up to it. The lines this scan
/// reads are a slave's spec plus one error string.
const MAX_STDERR_LINE_BYTES: usize = 16 * 1024;
/// The startup self-test's whole budget. It reads a four-second synthetic
/// source three times; this is a bound, not an expectation.
const SELF_TEST_BUDGET: Duration = Duration::from_secs(60);
/// One `ffmpeg` run inside the self-test.
const SELF_TEST_STEP_BUDGET: Duration = Duration::from_secs(20);
/// The self-test's index output is a four-second 64x64 fragmented MP4.
const SELF_TEST_MAX_STDOUT_BYTES: usize = 8 * 1024 * 1024;

/// The authored PGS track the repository's fuzz corpus carries, the same one
/// `scripts/mkpgs` writes: two display sets, sixteen segments.
const SELF_TEST_PGS: &[u8] = include_bytes!("../../../fuzz/corpus/inspect_sup/mkpgs-1920x1080.sup");

/// OS errors that make a slave failure `transient`: the disk, not the track.
/// `libavformat/tee.c` prints them through `av_err2str`, which is `strerror`.
const OS_ERRORS: [&str; 4] = [
    "No space left on device",
    "Input/output error",
    "Permission denied",
    "No such file or directory",
];

// ---------------------------------------------------------------------------
// Ordinals.

/// The PGS tracks of a probed file, as subtitle ordinals: the `N` in
/// `-map 0:s:N`, which counts every subtitle stream in stream-index order —
/// a text track before a PGS track shifts the PGS track's ordinal.
///
/// Read from the held-fd probe of the very file the pass is about to read,
/// so a track the scanner saw on an older version of the file can never be
/// asked for. A document this cannot read yields no ordinals, and no
/// ride-along: guessing an ordinal is how one track's cues get filed under
/// another's number.
#[cfg(test)]
pub(crate) fn pgs_ordinals_from_probe(raw: &str) -> Vec<i64> {
    eligible_tracks_from_probe(raw)
        .into_iter()
        .filter(|track| track.kind == ProbedKind::Pgs)
        .map(|track| track.ordinal)
        .collect()
}

// ---------------------------------------------------------------------------
// The gate: switch, self-test, local filesystem.

/// The startup self-test's state, for the gate and the Developer page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SelfTest {
    NotRun,
    Running,
    Passed {
        elapsed_ms: u64,
        /// `ffmpeg -version`'s version token, which the self-test found equal
        /// to `ffprobe`'s.
        version: String,
    },
    Failed {
        reason: String,
    },
}

static SELF_TEST: Mutex<SelfTest> = Mutex::new(SelfTest::NotRun);

#[cfg(test)]
thread_local! {
    /// A test's own answer for every gate condition but the switch, on its
    /// own thread only: a test that needs a riding pass through the real
    /// entry points cannot race the process-wide self-test state other tests
    /// read.
    static GATE_OVERRIDE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Open the gate's self-test, filesystem and free-space conditions on this
/// thread until the guard drops. Test-only; the switch is still read.
#[cfg(test)]
pub(crate) fn force_gate_open_on_this_thread() -> impl Drop {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            GATE_OVERRIDE.with(|cell| cell.set(false));
        }
    }
    GATE_OVERRIDE.with(|cell| cell.set(true));
    Reset
}

#[cfg(test)]
fn gate_overridden() -> bool {
    GATE_OVERRIDE.with(std::cell::Cell::get)
}

#[cfg(not(test))]
fn gate_overridden() -> bool {
    false
}

pub(crate) fn self_test_state() -> SelfTest {
    SELF_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn set_self_test_state(state: SelfTest) {
    *SELF_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = state;
}

/// Permission for one pass to ride along. Only [`RideAlongGate::open`] makes
/// one outside tests, and only when every condition holds.
#[derive(Clone)]
pub(crate) struct RideAlongGate {
    root: PathBuf,
    catalog: Option<Arc<dyn Store>>,
}

impl std::fmt::Debug for RideAlongGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RideAlongGate")
            .field("root", &self.root)
            .field("has_catalog", &self.catalog.is_some())
            .finish()
    }
}

impl RideAlongGate {
    /// Read the operator's switch and create the stage root. The startup
    /// self-test, filesystem and free-space probes remain advisory in the
    /// Developer screen; none silently vetoes an explicit enable.
    ///
    /// `None` is the behaviour that shipped before the producer existed. It
    /// is asked once per pass, so turning the switch off stops the next pass
    /// from riding without a restart.
    pub(crate) async fn open(
        store: Arc<dyn plurx_core::store::Store>,
        runtime_cache: &Path,
    ) -> Option<Self> {
        let switch_on = crate::subtitle_source::enabled(store.as_ref()).await;
        let root = store::store_root(runtime_cache);
        if !switch_on {
            return None;
        }
        if !gate_overridden() {
            if let Err(error) = tokio::fs::create_dir_all(&root).await {
                tracing::warn!(%error, path = %root.display(), "creating subtitle-source stage root");
                return None;
            }
        }
        Some(Self {
            root,
            catalog: Some(store),
        })
    }

    /// A gate for a test that exercises the pass itself.
    #[cfg(test)]
    pub(crate) fn for_test(root: PathBuf) -> Self {
        Self {
            root,
            catalog: None,
        }
    }
}

/// The gate's decision, with the reason when it is closed. Pure, so every
/// branch is testable without a store or a global.
pub(crate) fn gate_from(
    switch_on: bool,
    self_test: &SelfTest,
    filesystem: Result<String, String>,
    space: Result<String, String>,
    root: PathBuf,
) -> Result<RideAlongGate, String> {
    if !switch_on {
        return Err("subtitles.stored_sources is off".to_owned());
    }
    match self_test {
        SelfTest::Passed { .. } => {}
        SelfTest::NotRun | SelfTest::Running => {
            return Err("the startup self-test has not passed yet".to_owned());
        }
        SelfTest::Failed { reason } => {
            return Err(format!("the startup self-test failed: {reason}"));
        }
    }
    filesystem.map_err(|reason| format!("the cache is not usable for the ride-along: {reason}"))?;
    space.map_err(|reason| format!("the cache is too full for the ride-along: {reason}"))?;
    Ok(RideAlongGate {
        root,
        catalog: None,
    })
}

/// Advisory conditions for relying on stored tracks. Developer settings
/// displays these readings without overriding the saved switch. Nothing is
/// created while taking the reading.
pub(crate) fn gate_verdict(switch_on: bool, runtime_cache: &Path) -> Result<(), String> {
    let root = store::store_root(runtime_cache);
    let checked = if root.is_dir() {
        root.clone()
    } else {
        runtime_cache.to_owned()
    };
    gate_from(
        switch_on,
        &self_test_state(),
        local_filesystem(&checked),
        free_space(&checked),
        root,
    )
    .map(|_| ())
}

/// Whether `path` is on a local filesystem, by `statfs` type. `Ok` names the
/// filesystem; `Err` warns that a ride-along may stall on it.
///
/// A blocked file output stalls the demuxer every output shares, so a stage
/// on a network or FUSE filesystem could hold the index pass itself hostage.
/// The same rule is why two nodes never write one store.
#[cfg(target_os = "linux")]
pub(crate) fn local_filesystem(path: &Path) -> Result<String, String> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("{} contains a NUL byte", path.display()))?;
    let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `c_path` is NUL-terminated and outlives the call; `info` is a
    // valid out-pointer for exactly one `statfs`.
    let status = unsafe { libc::statfs(c_path.as_ptr(), info.as_mut_ptr()) };
    if status != 0 {
        return Err(format!(
            "statfs {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `statfs` returned 0, so it filled the structure.
    let info = unsafe { info.assume_init() };
    // `f_type` is a signed word whose width varies by architecture; the
    // magic numbers are 32-bit.
    #[allow(clippy::unnecessary_cast)]
    let magic = (info.f_type as i64 as u64 & 0xFFFF_FFFF) as u32;
    classify_filesystem_magic(magic)
}

/// Off the one platform the check is written for, the answer is "not
/// local": the ride-along is an optimisation, and a platform it cannot vouch
/// for loses nothing but the optimisation.
#[cfg(not(target_os = "linux"))]
pub(crate) fn local_filesystem(_path: &Path) -> Result<String, String> {
    Err("the local-filesystem check is implemented for Linux only".to_owned())
}

/// The free space the cache keeps before a pass may ride: the larger of one
/// gibibyte and two percent of the filesystem. Stage writes share the disk
/// with the index blob a pass is about to publish, so near full the ride
/// must give way before it can turn a pass the index would have finished into
/// an `ENOSPC` on every retry.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn free_space_margin(total_bytes: u64) -> u64 {
    (1_u64 << 30).max(total_bytes / 50)
}

/// Whether the cache's filesystem has [`free_space_margin`] available to an
/// unprivileged writer, by `statvfs`. `Ok` says how much; `Err` says why not.
#[cfg(target_os = "linux")]
pub(crate) fn free_space(path: &Path) -> Result<String, String> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("{} contains a NUL byte", path.display()))?;
    let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c_path` is NUL-terminated and outlives the call; `info` is a
    // valid out-pointer for exactly one `statvfs`.
    let status = unsafe { libc::statvfs(c_path.as_ptr(), info.as_mut_ptr()) };
    if status != 0 {
        return Err(format!(
            "statvfs {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `statvfs` returned 0, so it filled the structure.
    let info = unsafe { info.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    let (fragment, available, blocks) = (
        info.f_frsize as u64,
        info.f_bavail as u64,
        info.f_blocks as u64,
    );
    free_space_verdict(
        available.saturating_mul(fragment),
        blocks.saturating_mul(fragment),
    )
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn free_space(_path: &Path) -> Result<String, String> {
    Err("the free-space check is implemented for Linux only".to_owned())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn free_space_verdict(available: u64, total: u64) -> Result<String, String> {
    let margin = free_space_margin(total);
    let gib = |bytes: u64| bytes as f64 / f64::from(1_u32 << 30);
    if available >= margin {
        Ok(format!(
            "{:.1} GiB free of {:.1} GiB, above the {:.1} GiB margin",
            gib(available),
            gib(total),
            gib(margin)
        ))
    } else {
        Err(format!(
            "{:.1} GiB free of {:.1} GiB, below the {:.1} GiB margin",
            gib(available),
            gib(total),
            gib(margin)
        ))
    }
}

/// Network and userspace filesystems, by `statfs` magic. The design names
/// NFS, CIFS/SMB and FUSE; 9p, Ceph and AFS are here for the same reason.
#[cfg(target_os = "linux")]
const REMOTE_FILESYSTEMS: [(u32, &str); 8] = [
    (0x6969, "NFS"),
    (0x517B, "SMB"),
    (0xFF53_4D42, "CIFS"),
    (0xFE53_4D42, "SMB2"),
    (0x6573_5546, "FUSE"),
    (0x0102_1997, "9p"),
    (0x00C3_6400, "Ceph"),
    (0x5346_414F, "AFS"),
];

#[cfg(target_os = "linux")]
fn classify_filesystem_magic(magic: u32) -> Result<String, String> {
    match REMOTE_FILESYSTEMS.iter().find(|(known, _)| *known == magic) {
        Some((_, name)) => Err(format!("{name} (0x{magic:x}) is not a local filesystem")),
        None => Ok(format!("statfs type 0x{magic:x}")),
    }
}

// ---------------------------------------------------------------------------
// The plan: which tracks this pass extracts, into which stage.

/// A private directory for one attempt, removed on every exit route —
/// success, failure, a dropped build future (`foreground_preempted`, lease
/// loss) and a panic alike.
#[derive(Debug)]
pub(crate) struct StageDir {
    path: PathBuf,
}

impl StageDir {
    /// Make a new, empty, private directory under `root`. `create` rather than
    /// `create_all` for the leaf: a stage is never reused, so a leftover file
    /// can never sit where a slave will write (§6.2 fact 4).
    fn create(root: &Path, prefix: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(root)?;
        let path = root.join(format!("{prefix}{}", uuid::Uuid::new_v4().simple()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self { path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StageDir {
    fn drop(&mut self) {
        // A few tries: a child killed a moment ago may still be closing a
        // slave, and a directory that is not empty yet refuses the removal.
        for _ in 0..3 {
            match std::fs::remove_dir_all(&self.path) {
                Ok(()) => return,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) => std::thread::yield_now(),
            }
        }
        tracing::debug!(stage = %self.path.display(), "a ride-along stage outlived its guard; the startup sweep removes it");
    }
}

/// Files with a ride-along in flight on this node. The latch rides on
/// whichever identity's pass runs first; a second pass for the same file
/// while the first is running simply does not ride.
/// Keyed by store root and file id: one process has one root, and a test
/// with its own cache cannot collide with another's claim on the same id.
///
/// The value says what the ride is doing, for the product to show while it
/// runs: which tracks, into which stage, since when. It is filled in once the
/// stage exists.
static IN_FLIGHT: Mutex<BTreeMap<(PathBuf, i64), Option<RideInFlight>>> =
    Mutex::new(BTreeMap::new());

#[derive(Clone, Debug)]
struct RideInFlight {
    tracks: usize,
    stage: PathBuf,
    started_at_ms: i64,
}

/// One ride-along running on this node now, as the Maintenance card and the
/// analysis row show it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActiveRide {
    pub file_id: i64,
    /// The file's item and title, filled in by the settings read (the
    /// producer knows only the file id); `0` and empty when the catalog no
    /// longer names it.
    #[serde(default)]
    pub item_id: i64,
    #[serde(default)]
    pub title: String,
    /// Eligible subtitle tracks this pass is keeping.
    pub tracks: usize,
    /// Bytes written into the stage so far — the `.sup` and `.crc` files.
    pub bytes_written: u64,
    pub started_at_ms: i64,
    /// How long the pass has been riding, by this node's clock, so a viewer
    /// with another clock still reads the right duration.
    #[serde(default)]
    pub running_ms: i64,
}

/// Every ride-along in flight on this node, with what each has written so
/// far. Measured now: a stage is a handful of files.
pub(crate) fn active_rides() -> Vec<ActiveRide> {
    let rides: Vec<(i64, RideInFlight)> = IN_FLIGHT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter_map(|((_, file_id), ride)| ride.clone().map(|ride| (*file_id, ride)))
        .collect();
    let now = unix_ms();
    rides
        .into_iter()
        .map(|(file_id, ride)| ActiveRide {
            file_id,
            item_id: 0,
            title: String::new(),
            tracks: ride.tracks,
            bytes_written: stage_bytes(&ride.stage),
            started_at_ms: ride.started_at_ms,
            running_ms: now.saturating_sub(ride.started_at_ms).max(0),
        })
        .collect()
}

/// The bytes a stage's files hold now.
pub(crate) fn stage_bytes(stage: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(stage) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|metadata| metadata.len())
        .sum()
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

#[derive(Debug)]
struct FileClaim((PathBuf, i64));

impl FileClaim {
    fn take(root: &Path, file_id: i64) -> Option<Self> {
        let key = (root.to_owned(), file_id);
        // The guard is released before a claim exists: a claim's drop takes
        // the same lock.
        let inserted = {
            let mut in_flight = IN_FLIGHT
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if in_flight.contains_key(&key) {
                false
            } else {
                in_flight.insert(key.clone(), None);
                true
            }
        };
        inserted.then(|| Self(key))
    }

    /// Say what the claimed ride is doing, once its stage exists.
    fn describe(&self, tracks: usize, stage: &Path) {
        if let Some(entry) = IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&self.0)
        {
            *entry = Some(RideInFlight {
                tracks,
                stage: stage.to_owned(),
                started_at_ms: unix_ms(),
            });
        }
    }
}

impl Drop for FileClaim {
    fn drop(&mut self) {
        IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// Files whose last riding pass did not build its index, with the source
/// identity that pass read.
///
/// The only way the ride-along's own argv can fail the index is a hard
/// `-map 0:s:N` this `ffmpeg` cannot find — an `ffprobe` of another build
/// counting subtitle streams differently, say. A failed riding pass leaves no
/// manifest, so without this every retry would rebuild the same argv, fail
/// the same way and charge the same retry, and the title's index would never
/// arrive: the ride-along's worst case must be "no subtitle artifacts", never
/// "no index". So the next pass on the file is a plain index pass. Any
/// non-`Built` outcome counts — a riding pass cannot tell whose fault a
/// failure was, and the cost of being wrong is one pass that does not ride.
///
/// In memory, deliberately. The startup self-test refuses a mismatched
/// `ffprobe`/`ffmpeg` pair outright, so a failure that survives it is rare,
/// and a restart — a new build, a new `PLURX_FFMPEG` — is exactly when the
/// question deserves asking again. A persistent record would outlive the
/// thing it describes.
/// Keyed by store root and file id, as [`IN_FLIGHT`] is.
static FAILED_RIDES: Mutex<BTreeMap<(PathBuf, i64), SourceStamp>> = Mutex::new(BTreeMap::new());

fn failed_rides() -> std::sync::MutexGuard<'static, BTreeMap<(PathBuf, i64), SourceStamp>> {
    FAILED_RIDES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Remember that a riding pass over this plan's source did not build its
/// index.
pub(crate) fn record_failed_ride(plan: &RideAlongPlan) {
    tracing::warn!(
        file_id = plan.file_id,
        tracks = ?plan.tracks,
        "a riding fragment-index pass did not build its index; this file's next pass will not ride"
    );
    failed_rides().insert((plan.root.clone(), plan.file_id), plan.source);
}

/// Files that ride no more in this process because a riding pass failed.
pub(crate) fn failed_ride_count() -> usize {
    failed_rides().len()
}

/// One pass's ride-along: the ordinals it extracts, in `-map` order, and the
/// stage it writes them into.
#[derive(Debug)]
pub(crate) struct RideAlongPlan {
    file_id: i64,
    root: PathBuf,
    source: SourceStamp,
    /// Every eligible ordinal the probe found, in order — the manifest's
    /// `ordinals`.
    probed: Vec<i64>,
    /// The ordinals this pass extracts; track `i` of the tee is `tracks[i]`.
    tracks: Vec<i64>,
    kinds: BTreeMap<i64, ProbedKind>,
    stream_indexes: BTreeMap<i64, u64>,
    /// Settled entries from a manifest for this same source, carried over.
    carried: Vec<TrackEntry>,
    /// Passes already spent on each extracted ordinal for this source.
    prior_attempts: BTreeMap<i64, u32>,
    stage: StageDir,
    /// Declared after the stage, so the stage is gone before another pass
    /// for this file may ride.
    _claim: Option<FileClaim>,
}

/// The latch, checked after the held-fd probe: a plan for the tracks still
/// needing work, or `None` when there are none.
///
/// *Current* means the manifest's source matches a live `fstat` of the file
/// the pass holds — size and mtime, and `(dev, ino)` where the platform has
/// them, the burn path's rule, so a file replaced in place with a new inode
/// rides again rather than leaving the burn path to miss on it for good —
/// and every probed ordinal is settled. A file with no eligible subtitle track has nothing
/// to latch: no plan, no argv, ever.
#[cfg(test)]
pub(crate) async fn plan(
    gate: &RideAlongGate,
    file_id: i64,
    source: &std::fs::File,
    probed: &[i64],
) -> Option<RideAlongPlan> {
    let typed: Vec<_> = probed
        .iter()
        .map(|ordinal| ProbedTrack {
            ordinal: *ordinal,
            kind: ProbedKind::Pgs,
            stream_index: *ordinal as u64,
        })
        .collect();
    plan_tracks(gate, file_id, source, &typed).await
}

/// Plan from the live probe's typed eligible ordinals. Hydrated entries do
/// not establish extraction coverage, so they cannot close this plan.
pub(crate) async fn plan_tracks(
    gate: &RideAlongGate,
    file_id: i64,
    source: &std::fs::File,
    probed: &[ProbedTrack],
) -> Option<RideAlongPlan> {
    if probed.is_empty() {
        return None;
    }
    let live = crate::fragment_index_cluster::source_stamp(&source.metadata().ok()?);
    {
        let key = (gate.root.clone(), file_id);
        let mut failed = failed_rides();
        match failed.get(&key) {
            Some(stamp) if *stamp == live => {
                tracing::debug!(
                    file_id,
                    "the last riding pass on this source failed; not riding"
                );
                return None;
            }
            // Another source identity is another question.
            Some(_) => {
                failed.remove(&key);
            }
            None => {}
        }
    }
    let dir = store::file_dir(&gate.root, file_id);
    let previous = store::read_manifest(&dir).await.filter(|manifest| {
        manifest.version == MANIFEST_VERSION
            && manifest.file_id == file_id
            && manifest.source_matches(&live, Consumer::Burn)
    });
    let cluster_covered = cluster_extracted_coverage(gate, file_id, source, probed).await;
    let mut tracks = Vec::new();
    let mut carried = Vec::new();
    let mut prior_attempts = BTreeMap::new();
    for ProbedTrack { ordinal, .. } in probed {
        if cluster_covered.contains(ordinal) {
            continue;
        }
        match previous
            .as_ref()
            .and_then(|manifest| manifest.track(*ordinal))
        {
            // A `kept` entry is settled only while its file is there: after a
            // power loss or a deletion the manifest can name a track the disk
            // no longer has, and trusting it would miss for good.
            Some(entry)
                if entry.settled()
                    && entry.representations.iter().all(|representation| {
                        representation.origin == RepresentationOrigin::Extracted
                    })
                    && kept_file_present(&dir, entry).await =>
            {
                carried.push(entry.clone())
            }
            Some(entry) => {
                prior_attempts.insert(*ordinal, entry.attempts);
                tracks.push(*ordinal);
            }
            None => {
                prior_attempts.insert(*ordinal, 0);
                tracks.push(*ordinal);
            }
        }
    }
    if tracks.is_empty() {
        return None;
    }
    let claim = FileClaim::take(&gate.root, file_id)?;
    let stage = match StageDir::create(&gate.root, STAGE_PREFIX) {
        Ok(stage) => stage,
        Err(error) => {
            tracing::warn!(file_id, %error, "creating a subtitle ride-along stage; this pass does not ride");
            return None;
        }
    };
    // The tee spec carries the stage path as text; a path that is not UTF-8
    // cannot be written into it faithfully.
    stage.path().to_str()?;
    claim.describe(tracks.len(), stage.path());
    Some(RideAlongPlan {
        file_id,
        root: gate.root.clone(),
        source: live,
        probed: probed.iter().map(|track| track.ordinal).collect(),
        tracks,
        kinds: probed
            .iter()
            .map(|track| (track.ordinal, track.kind))
            .collect(),
        stream_indexes: probed
            .iter()
            .map(|track| (track.ordinal, track.stream_index))
            .collect(),
        carried,
        prior_attempts,
        stage,
        _claim: Some(claim),
    })
}

/// A second node's settled extraction covers an ordinal only when its
/// portable source digest matches this held file. The index pass otherwise
/// keeps its normal local ride-along and never trusts size/mtime alone.
async fn cluster_extracted_coverage(
    gate: &RideAlongGate,
    file_id: i64,
    source: &std::fs::File,
    probed: &[ProbedTrack],
) -> HashSet<i64> {
    let Some(catalog) = gate.catalog.as_ref() else {
        return HashSet::new();
    };
    let cluster_on = catalog
        .get_setting(plurx_core::store::keys::SUBTITLE_CLUSTER_SOURCES)
        .await
        .ok()
        .is_some_and(|value| plurx_core::store::stored_switch(value.as_deref(), false));
    if !cluster_on {
        return HashSet::new();
    }
    let Some(file) = catalog.get_file(file_id).await.ok().flatten() else {
        return HashSet::new();
    };
    let Ok(rows) = catalog
        .list_subtitle_source_publications(file_id, file.size, file.mtime)
        .await
    else {
        return HashSet::new();
    };
    if rows.is_empty() {
        return HashSet::new();
    }
    let Ok(attested) = crate::fragment_index_cluster::attest_source("", &file, None, &|_| {}).await
    else {
        return HashSet::new();
    };
    if !crate::fragment_index_cluster::source_still_matches(source, &attested.observation)
        .unwrap_or(false)
    {
        return HashSet::new();
    }
    let current: Vec<_> = rows
        .into_iter()
        .filter(|row| row.source_attestation == attested.observation.source_sha256)
        .collect();
    probed
        .iter()
        .filter(|track| store::extracted_ordinal_covered(&current, track.ordinal))
        .map(|track| track.ordinal)
        .collect()
}

/// A `kept` entry's stored file exists as a regular file; any other verdict
/// has no file to check.
async fn kept_file_present(dir: &Path, entry: &TrackEntry) -> bool {
    if entry.representations.is_empty() {
        return entry.verdict != Verdict::Kept
            || match entry.file.as_deref() {
                Some(name) => tokio::fs::metadata(dir.join(name))
                    .await
                    .is_ok_and(|metadata| metadata.is_file()),
                None => false,
            };
    }
    for representation in &entry.representations {
        if representation.verdict == Verdict::Kept {
            let Some(name) = representation.file.as_deref() else {
                return false;
            };
            if !tokio::fs::metadata(dir.join(name))
                .await
                .is_ok_and(|metadata| metadata.is_file())
            {
                return false;
            }
        }
    }
    true
}

impl RideAlongPlan {
    /// A plan for the self-test and the tests: every given ordinal is
    /// extracted, nothing is carried, and no claim is taken.
    pub(crate) fn standalone(
        root: &Path,
        file_id: i64,
        source: SourceStamp,
        tracks: Vec<i64>,
    ) -> std::io::Result<Self> {
        Ok(Self {
            file_id,
            root: root.to_owned(),
            source,
            probed: tracks.clone(),
            prior_attempts: tracks.iter().map(|ordinal| (*ordinal, 0)).collect(),
            kinds: tracks
                .iter()
                .map(|ordinal| (*ordinal, ProbedKind::Pgs))
                .collect(),
            stream_indexes: tracks
                .iter()
                .map(|ordinal| (*ordinal, *ordinal as u64))
                .collect(),
            tracks,
            carried: Vec::new(),
            stage: StageDir::create(root, STAGE_PREFIX)?,
            _claim: None,
        })
    }

    pub(crate) fn tracks(&self) -> &[i64] {
        &self.tracks
    }

    pub(crate) fn stage(&self) -> &Path {
        self.stage.path()
    }

    fn sup_path(&self, ordinal: i64) -> PathBuf {
        self.stage.path().join(format!("s{ordinal}.sup"))
    }

    fn crc_path(&self, ordinal: i64) -> PathBuf {
        self.stage.path().join(format!("s{ordinal}.crc"))
    }

    fn vtt_path(&self, ordinal: i64) -> PathBuf {
        self.stage.path().join(format!("s{ordinal}.vtt"))
    }

    fn mks_path(&self, ordinal: i64) -> PathBuf {
        self.stage.path().join(format!("s{ordinal}.mks"))
    }

    fn kind(&self, ordinal: i64) -> ProbedKind {
        self.kinds.get(&ordinal).copied().unwrap_or(ProbedKind::Pgs)
    }

    fn outputs(&self) -> Vec<(i64, ProbedKind, RepresentationFormat)> {
        let mut outputs = Vec::with_capacity(self.tracks.len() * 2);
        for &ordinal in &self.tracks {
            let kind = self.kind(ordinal);
            match kind {
                ProbedKind::Pgs => outputs.push((ordinal, kind, RepresentationFormat::Sup)),
                ProbedKind::Text => outputs.push((ordinal, kind, RepresentationFormat::Webvtt)),
                ProbedKind::TextStyled => {
                    outputs.push((ordinal, kind, RepresentationFormat::Webvtt));
                    outputs.push((ordinal, kind, RepresentationFormat::Matroska));
                }
            }
        }
        outputs
    }

    fn representation_path(&self, ordinal: i64, format: RepresentationFormat) -> PathBuf {
        match format {
            RepresentationFormat::Sup => self.sup_path(ordinal),
            RepresentationFormat::Webvtt => self.vtt_path(ordinal),
            RepresentationFormat::Matroska => self.mks_path(ordinal),
        }
    }

    /// The slaves in tee order: track `i`'s `sup` is `#2i`, its `framecrc`
    /// companion `#2i+1`. The sentinel, last, is not listed.
    fn slave_paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        for (ordinal, _, format) in self.outputs() {
            paths.push(
                self.representation_path(ordinal, format)
                    .to_string_lossy()
                    .into_owned(),
            );
            if format != RepresentationFormat::Matroska {
                paths.push(self.crc_path(ordinal).to_string_lossy().into_owned());
            }
        }
        paths
    }

    /// The argv appended to the index pass, after `pipe:1`.
    ///
    /// `-nostdin` and `-y` are global options and change nothing about the
    /// index output (the self-test proves that on every start). The maps are
    /// hard: an ordinal the file does not have kills the pass rather than
    /// silently shifting another track into its place.
    pub(crate) fn args(&self) -> Vec<String> {
        let mut args = vec!["-nostdin".to_owned(), "-y".to_owned()];
        for (ordinal, _, _) in self.outputs() {
            args.push("-map".to_owned());
            args.push(format!("0:s:{ordinal}"));
        }
        for (position, (_, _, format)) in self.outputs().iter().enumerate() {
            args.push(format!("-c:s:{position}"));
            args.push(
                if *format == RepresentationFormat::Webvtt {
                    "webvtt"
                } else {
                    "copy"
                }
                .to_owned(),
            );
        }
        args.extend(["-f", "tee"].map(str::to_owned));
        args.push(self.tee_spec());
        args
    }

    fn tee_spec(&self) -> String {
        let mut slaves = Vec::with_capacity(self.tracks.len() * 3 + 1);
        for (position, (ordinal, _, format)) in self.outputs().iter().enumerate() {
            let muxer = match format {
                RepresentationFormat::Sup => "sup",
                RepresentationFormat::Webvtt => "webvtt",
                RepresentationFormat::Matroska => "matroska",
            };
            slaves.push(format!(
                "[select={position}:f={muxer}:onfail=ignore]{}",
                tee_escape(
                    &self
                        .representation_path(*ordinal, *format)
                        .to_string_lossy()
                )
            ));
            if *format != RepresentationFormat::Matroska {
                slaves.push(format!(
                    "[select={position}:f=framecrc:onfail=ignore]{}",
                    tee_escape(&self.crc_path(*ordinal).to_string_lossy())
                ));
            }
        }
        // Mandatory: a tee whose every slave failed reports "All tee outputs
        // failed" and takes the process — and the index — down with it.
        slaves.push("[f=null]-".to_owned());
        slaves.join("|")
    }

    /// A stderr scan for this plan's slaves.
    pub(crate) fn stderr_scan(&self) -> StderrScan {
        StderrScan::new(self.slave_paths())
    }
}

/// Escape a slave's file name for the tee spec.
///
/// `libavformat/tee.c` splits the spec into slaves with
/// `av_get_token(&p, "|")`, which drops a backslash and keeps the character
/// after it, keeps everything between single quotes literally, and trims
/// unescaped leading and trailing whitespace. The file name is whatever
/// follows the slave's `[...]` options and is not unescaped again. So one
/// backslash before each of `\`, `|` and `'`, and before whitespace, is
/// exactly what survives that one pass. `[` and `]` are escaped too: after
/// the options they are literal either way, and escaped they cannot be read
/// as the start of an option list by any later change to the parser.
pub(crate) fn tee_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 8);
    for character in text.chars() {
        if matches!(character, '\\' | '|' | '[' | ']' | '\'') || character.is_whitespace() {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

// ---------------------------------------------------------------------------
// The stderr scan.

/// Per-slave failures read from the index child's stderr.
///
/// The second check, not the first: the lines are free text from
/// `libavformat/tee.c` and a patched build could word them differently, which
/// would fail open — so a track is judged first by the `framecrc` count, and
/// a failure here can only take a track *out* of `kept`. Both lines are
/// logged at `AV_LOG_ERROR`, so the index keeps `-loglevel error`.
#[derive(Debug, Default)]
pub(crate) struct StderrScan {
    slaves: Vec<String>,
    failures: BTreeMap<usize, String>,
    decoder_errors: Vec<Option<u64>>,
    partial: Vec<u8>,
}

impl StderrScan {
    fn new(slaves: Vec<String>) -> Self {
        Self {
            slaves,
            ..Self::default()
        }
    }

    /// Feed raw stderr bytes; complete lines are observed as they arrive.
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        for byte in bytes {
            if matches!(byte, b'\n' | b'\r') {
                self.flush_line();
            } else if self.partial.len() < MAX_STDERR_LINE_BYTES {
                self.partial.push(*byte);
            }
        }
    }

    /// End of stream: observe a final line with no terminator.
    pub(crate) fn finish(&mut self) {
        self.flush_line();
    }

    fn flush_line(&mut self) {
        if self.partial.is_empty() {
            return;
        }
        let line = String::from_utf8_lossy(&std::mem::take(&mut self.partial)).into_owned();
        self.observe(&line);
    }

    fn observe(&mut self, line: &str) {
        // A decoder can silently drop one malformed input packet from both
        // the VTT and its framecrc companion. Equal output counts then prove
        // nothing about completeness (M0 E5), so remember the source stream.
        if line.contains("Error decoding subtitles:") {
            let stream = line.split("sist#0:").nth(1).and_then(|tail| {
                tail.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<u64>()
                    .ok()
            });
            self.decoder_errors.push(stream);
        }
        // "Slave muxer #3 failed: No space left on device, continuing with 4/5 slaves."
        if let Some(rest) = line.split("Slave muxer #").nth(1) {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if let (Ok(slave), Some(error)) = (
                digits.parse::<usize>(),
                rest.split_once(" failed: ").map(|(_, error)| error),
            ) {
                let error = error
                    .split(", continuing with")
                    .next()
                    .unwrap_or(error)
                    .trim();
                self.failures
                    .entry(slave)
                    .or_insert_with(|| error.to_owned());
                return;
            }
        }
        // "Slave '[select=0:f=sup:onfail=ignore]/stage/s0.sup': error opening: No such file or directory"
        if let Some((head, error)) = line.split_once("error opening: ") {
            if let Some(slave) = self
                .slaves
                .iter()
                .position(|path| head.contains(path.as_str()))
            {
                self.failures
                    .entry(slave)
                    .or_insert_with(|| error.trim().to_owned());
            }
        }
    }

    /// The failure reported for slave `index`, if any.
    pub(crate) fn failure(&self, index: usize) -> Option<&str> {
        self.failures.get(&index).map(String::as_str)
    }

    fn decoder_failed(&self, stream_index: u64) -> bool {
        self.decoder_errors
            .iter()
            .any(|error| error.is_none_or(|index| index == stream_index))
    }
}

fn is_os_error(error: &str) -> bool {
    OS_ERRORS.iter().any(|known| error.contains(known))
}

// ---------------------------------------------------------------------------
// The verdict.

/// What the pass concluded about one track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrackOutcome {
    pub(crate) ordinal: i64,
    pub(crate) verdict: Verdict,
    /// The stored bytes' sha256, for a `kept` track.
    pub(crate) sha256: Option<String>,
    /// Bytes the pass wrote into the stage for this track, `.sup` and `.crc`.
    pub(crate) written: u64,
    /// Why the track is not `kept`, for the log.
    pub(crate) reason: Option<String>,
    pub(crate) representations: Vec<RepresentationOutcome>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RepresentationOutcome {
    pub(crate) format: RepresentationFormat,
    pub(crate) verdict: Verdict,
    pub(crate) sha256: Option<String>,
    pub(crate) bytes: u64,
    pub(crate) reason: Option<String>,
}

struct CrcTotals {
    header_lines: u64,
    packets: u64,
    bytes: u64,
}

/// Read a `framecrc` file: `#` header lines, then one line per packet whose
/// **fifth** comma-separated field is the packet size. Read by position —
/// optional `F=`/`S=` fields follow it.
fn crc_totals(text: &str) -> Result<CrcTotals, String> {
    let mut totals = CrcTotals {
        header_lines: 0,
        packets: 0,
        bytes: 0,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            totals.header_lines += 1;
            continue;
        }
        let size = line
            .split(',')
            .nth(4)
            .and_then(|field| field.trim().parse::<u64>().ok())
            .ok_or_else(|| format!("unreadable framecrc line {line:?}"))?;
        totals.packets += 1;
        totals.bytes = totals
            .bytes
            .checked_add(size)
            .ok_or_else(|| "framecrc byte total overflowed".to_owned())?;
    }
    Ok(totals)
}

struct SupWalk {
    /// `Σ(3 + lenᵢ)`: the packet bytes the `sup` muxer was handed.
    packet_bytes: u64,
    segments: u64,
    sha256: String,
}

/// Walk a `.sup` as segments of `13 + len` bytes, hashing as it goes. The
/// walk must consume the file exactly; a partial header, a missing `PG`
/// magic or a payload that runs off the end is not a segment stream.
fn walk_sup(file: std::fs::File) -> Result<SupWalk, String> {
    let mut reader = std::io::BufReader::with_capacity(256 * 1024, file);
    let mut hash = Sha256::new();
    let mut walk = SupWalk {
        packet_bytes: 0,
        segments: 0,
        sha256: String::new(),
    };
    let mut header = [0_u8; SEGMENT_HEADER_BYTES as usize];
    let mut payload = vec![0_u8; 64 * 1024];
    loop {
        let mut filled = 0;
        while filled < header.len() {
            match reader.read(&mut header[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(format!("reading the stored track: {error}")),
            }
        }
        if filled == 0 {
            break;
        }
        if filled < header.len() {
            return Err(format!("{filled} byte(s) left over after the last segment"));
        }
        if &header[..2] != b"PG" {
            return Err(format!("segment {} has no PG magic", walk.segments));
        }
        hash.update(header);
        let length = u64::from(u16::from_be_bytes([header[11], header[12]]));
        let mut remaining = length;
        while remaining > 0 {
            let take = remaining.min(payload.len() as u64) as usize;
            reader
                .read_exact(&mut payload[..take])
                .map_err(|_| format!("segment {} runs past the end of the file", walk.segments))?;
            hash.update(&payload[..take]);
            remaining -= take as u64;
        }
        walk.segments += 1;
        walk.packet_bytes += PACKET_SEGMENT_HEADER_BYTES + length;
    }
    walk.sha256 = hex::encode(hash.finalize());
    Ok(walk)
}

/// The inputs the verdict needs, detached from the plan so the blocking
/// worker owns them.
#[derive(Clone)]
struct VerdictInput {
    tracks: Vec<i64>,
    kinds: BTreeMap<i64, ProbedKind>,
    stream_indexes: BTreeMap<i64, u64>,
    stage: PathBuf,
}

/// Judge every track of a finished pass. Blocking: reads and parses.
///
/// `scan` is `None` when the stderr reader did not finish, so nothing can be
/// said about slave failures — every track is then `transient`.
fn verdicts(input: &VerdictInput, scan: Option<&StderrScan>) -> Vec<TrackOutcome> {
    let mut slave = 0;
    input
        .tracks
        .iter()
        .map(|ordinal| {
            let kind = input.kinds.get(ordinal).copied().unwrap_or(ProbedKind::Pgs);
            let sup = input.stage.join(format!("s{ordinal}.sup"));
            let vtt = input.stage.join(format!("s{ordinal}.vtt"));
            let mks = input.stage.join(format!("s{ordinal}.mks"));
            let crc = input.stage.join(format!("s{ordinal}.crc"));
            let written = [&sup, &vtt, &mks, &crc]
                .iter()
                .filter_map(|path| std::fs::metadata(path).ok())
                .map(|metadata| metadata.len())
                .sum();
            let mut representations = Vec::new();
            let first_format = if kind == ProbedKind::Pgs {
                RepresentationFormat::Sup
            } else {
                RepresentationFormat::Webvtt
            };
            let first = match scan {
                None => (
                    Verdict::Transient,
                    None,
                    Some("the stderr scan did not finish".to_owned()),
                ),
                Some(scan) if kind == ProbedKind::Pgs => {
                    judge(&sup, &crc, [scan.failure(slave), scan.failure(slave + 1)])
                }
                Some(scan) => judge_text_vtt(
                    &vtt,
                    &crc,
                    [scan.failure(slave), scan.failure(slave + 1)],
                    scan.decoder_failed(
                        input
                            .stream_indexes
                            .get(ordinal)
                            .copied()
                            .unwrap_or(*ordinal as u64),
                    ),
                ),
            };
            representations.push(RepresentationOutcome {
                format: first_format,
                verdict: first.0,
                sha256: first.1.clone(),
                bytes: std::fs::metadata(if kind == ProbedKind::Pgs { &sup } else { &vtt })
                    .map_or(0, |metadata| metadata.len()),
                reason: first.2.clone(),
            });
            slave += 2;
            if kind == ProbedKind::TextStyled {
                let next = match scan {
                    None => (
                        Verdict::Transient,
                        None,
                        Some("the stderr scan did not finish".to_owned()),
                    ),
                    Some(scan) => judge_text_mks(
                        &mks,
                        &crc,
                        scan.failure(slave),
                        scan.failure(slave - 1),
                        scan.decoder_failed(
                            input
                                .stream_indexes
                                .get(ordinal)
                                .copied()
                                .unwrap_or(*ordinal as u64),
                        ),
                    ),
                };
                representations.push(RepresentationOutcome {
                    format: RepresentationFormat::Matroska,
                    verdict: next.0,
                    sha256: next.1,
                    bytes: std::fs::metadata(&mks).map_or(0, |metadata| metadata.len()),
                    reason: next.2,
                });
                slave += 1;
            }
            TrackOutcome {
                ordinal: *ordinal,
                verdict: first.0,
                sha256: first.1,
                written,
                reason: first.2,
                representations,
            }
        })
        .collect()
}

/// §6.3's table, in its order of precedence.
fn judge(
    sup: &Path,
    crc: &Path,
    failures: [Option<&str>; 2],
) -> (Verdict, Option<String>, Option<String>) {
    let transient = |reason: String| (Verdict::Transient, None, Some(reason));
    let malformed = |reason: String| (Verdict::Malformed, None, Some(reason));
    let reported: Vec<&str> = failures.iter().flatten().copied().collect();
    if let Some(error) = reported.iter().find(|error| is_os_error(error)) {
        return transient(format!("a slave failed with an OS error: {error}"));
    }
    let text = match std::fs::read(crc) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => return transient(format!("the framecrc companion is missing: {error}")),
    };
    // Any reported failure takes the track out of `kept`, even when the
    // totals would agree: a failure at close comes after all data was written.
    if let Some(error) = reported.first() {
        return malformed(format!("a slave failed: {error}"));
    }
    let totals = match crc_totals(&text) {
        Ok(totals) => totals,
        Err(reason) => return malformed(reason),
    };
    if totals.packets == 0 {
        return if totals.header_lines > 0 {
            (Verdict::Empty, None, None)
        } else {
            transient("the framecrc companion was never written".to_owned())
        };
    }
    let metadata = match std::fs::metadata(sup) {
        Ok(metadata) => metadata,
        Err(error) => {
            return malformed(format!(
                "no stored track for {} packets: {error}",
                totals.packets
            ));
        }
    };
    if metadata.len() > MAX_TRACK_BYTES {
        return malformed(format!(
            "{} bytes is over the {MAX_TRACK_BYTES} byte track bound",
            metadata.len()
        ));
    }
    let walk = match std::fs::File::open(sup)
        .map_err(|error| error.to_string())
        .and_then(walk_sup)
    {
        Ok(walk) => walk,
        Err(reason) => return malformed(reason),
    };
    if walk.packet_bytes != totals.bytes {
        return malformed(format!(
            "the .sup carries {} packet bytes in {} segments and framecrc counted {} in {} packets",
            walk.packet_bytes, walk.segments, totals.bytes, totals.packets
        ));
    }
    // The same parser, with the same limits, the overlay's normaliser runs;
    // inspection keeps no pixels, so it costs a parse and not the pictures.
    let parsed = std::fs::File::open(sup)
        .map_err(|error| error.to_string())
        .and_then(|file| {
            plurx_pgs::inspect_sup_file(file, &plurx_pgs::ParserLimits::default())
                .map_err(|error| error.to_string())
        });
    if let Err(reason) = parsed {
        return malformed(format!("the PGS parser refused it: {reason}"));
    }
    (Verdict::Kept, Some(walk.sha256), None)
}

/// A WebVTT output is complete only when every encoded packet became one
/// syntactically valid cue. The stderr veto catches corrupt source packets
/// dropped by *both* the encoder and framecrc (M0 E5).
fn judge_text_vtt(
    vtt: &Path,
    crc: &Path,
    failures: [Option<&str>; 2],
    decoder_failed: bool,
) -> (Verdict, Option<String>, Option<String>) {
    let transient = |reason: String| (Verdict::Transient, None, Some(reason));
    let malformed = |reason: String| (Verdict::Malformed, None, Some(reason));
    if let Some(error) = failures.iter().flatten().find(|error| is_os_error(error)) {
        return transient(format!("a slave failed with an OS error: {error}"));
    }
    let crc_text = match std::fs::read_to_string(crc) {
        Ok(text) => text,
        Err(error) => return transient(format!("the framecrc companion is missing: {error}")),
    };
    if decoder_failed {
        return malformed(
            "ffmpeg reported a subtitle decoder error for this input stream".to_owned(),
        );
    }
    if let Some(error) = failures.iter().flatten().next() {
        return malformed(format!("a slave failed: {error}"));
    }
    let totals = match crc_totals(&crc_text) {
        Ok(totals) => totals,
        Err(reason) => return malformed(reason),
    };
    if totals.packets == 0 {
        if totals.header_lines == 0 {
            return transient("the framecrc companion was never written".to_owned());
        }
        return match std::fs::read(vtt)
            .map_err(|error| format!("reading empty WebVTT: {error}"))
            .and_then(|bytes| webvtt_cue_count(&bytes))
        {
            Ok(0) => (Verdict::Empty, None, None),
            Ok(cues) => malformed(format!("framecrc is empty but WebVTT has {cues} cues")),
            Err(reason) => malformed(reason),
        };
    }
    let metadata = match std::fs::metadata(vtt) {
        Ok(metadata) => metadata,
        Err(error) => return malformed(format!("the WebVTT output is missing: {error}")),
    };
    if metadata.len() == 0 || metadata.len() > 8 * 1024 * 1024 {
        return malformed(format!(
            "WebVTT is {} bytes, outside the 8 MiB sidecar bound",
            metadata.len()
        ));
    }
    let bytes = match std::fs::read(vtt) {
        Ok(bytes) => bytes,
        Err(error) => return transient(format!("reading WebVTT: {error}")),
    };
    let cues = match webvtt_cue_count(&bytes) {
        Ok(cues) => cues,
        Err(reason) => return malformed(reason),
    };
    if cues != totals.packets {
        return malformed(format!(
            "WebVTT has {cues} cues but framecrc counted {} packets",
            totals.packets
        ));
    }
    (
        Verdict::Kept,
        Some(hex::encode(Sha256::digest(&bytes))),
        None,
    )
}

fn webvtt_cue_count(bytes: &[u8]) -> Result<u64, String> {
    let text =
        std::str::from_utf8(bytes).map_err(|error| format!("WebVTT is not UTF-8: {error}"))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !text.starts_with("WEBVTT") || !text[6..].starts_with(['\n', '\r', ' ', '\t']) {
        return Err("WebVTT header is missing".to_owned());
    }
    let mut cues = 0;
    for line in text.lines() {
        if let Some((start, end)) = line.split_once(" --> ") {
            let end = end.split_ascii_whitespace().next().unwrap_or("");
            if !valid_vtt_timestamp(start) || !valid_vtt_timestamp(end) {
                return Err(format!("invalid WebVTT cue timing: {line}"));
            }
            cues += 1;
        }
    }
    Ok(cues)
}

fn valid_vtt_timestamp(text: &str) -> bool {
    let parts: Vec<_> = text.split(':').collect();
    if !(parts.len() == 2 || parts.len() == 3) {
        return false;
    }
    let (seconds, millis) = match parts.last().and_then(|part| part.split_once('.')) {
        Some(pair) => pair,
        None => return false,
    };
    seconds.len() == 2
        && seconds.parse::<u8>().is_ok_and(|value| value < 60)
        && millis.len() == 3
        && millis.parse::<u16>().is_ok()
        && parts[..parts.len() - 1]
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn judge_text_mks(
    mks: &Path,
    crc: &Path,
    failure: Option<&str>,
    crc_failure: Option<&str>,
    decoder_failed: bool,
) -> (Verdict, Option<String>, Option<String>) {
    let transient = |reason: String| (Verdict::Transient, None, Some(reason));
    let malformed = |reason: String| (Verdict::Malformed, None, Some(reason));
    if let Some(error) = [failure, crc_failure]
        .into_iter()
        .flatten()
        .find(|error| is_os_error(error))
    {
        return transient(format!("a slave failed with an OS error: {error}"));
    }
    if decoder_failed {
        return malformed(
            "ffmpeg reported a subtitle decoder error for this input stream".to_owned(),
        );
    }
    if let Some(error) = failure {
        return malformed(format!("the Matroska slave failed: {error}"));
    }
    let crc_text = match std::fs::read_to_string(crc) {
        Ok(text) => text,
        Err(error) => return transient(format!("the framecrc companion is missing: {error}")),
    };
    let totals = match crc_totals(&crc_text) {
        Ok(totals) => totals,
        Err(reason) => return malformed(reason),
    };
    if totals.packets == 0 {
        return if totals.header_lines > 0 {
            (Verdict::Empty, None, None)
        } else {
            transient("the framecrc companion was never written".to_owned())
        };
    }
    let bytes = match std::fs::read(mks) {
        Ok(bytes) => bytes,
        Err(error) => return malformed(format!("the Matroska output is missing: {error}")),
    };
    if bytes.is_empty() || bytes.len() as u64 > MAX_TRACK_BYTES {
        return malformed(format!(
            "Matroska is {} bytes, outside the track bound",
            bytes.len()
        ));
    }
    let mut probe = std::process::Command::new(crate::ffmpeg::ffprobe_bin());
    probe
        .args([
            "-v",
            "error",
            "-select_streams",
            "s:0",
            "-count_packets",
            "-show_entries",
            "stream=nb_read_packets",
            "-of",
            "default=nokey=1:noprint_wrappers=1",
        ])
        .arg(mks);
    // `verdicts` runs inside `spawn_blocking`, so the blocking launcher.
    let probe = crate::process_control::output_job_owned_blocking(
        &mut probe,
        crate::process_control::ChildWork::background("PGS ride-along verdict probe"),
    );
    let probe = match probe {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            return malformed(format!(
                "ffprobe refused Matroska: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Err(error) => return transient(format!("running ffprobe on Matroska: {error}")),
    };
    let packets = String::from_utf8_lossy(&probe.stdout).trim().parse::<u64>();
    if packets.as_ref().ok() != Some(&totals.packets) {
        return malformed(format!(
            "Matroska packet count {:?} differs from framecrc {}",
            packets, totals.packets
        ));
    }
    (
        Verdict::Kept,
        Some(hex::encode(Sha256::digest(&bytes))),
        None,
    )
}

// ---------------------------------------------------------------------------
// Harvest and publish.

/// A reaped pass's ride-along, not yet judged: the plan (and its stage) and
/// the stderr scan. The build hands this back so the verdict — which walks,
/// hashes and parses every track — runs outside the build future, which the
/// cluster worker races against lease loss and foreground preemption. A
/// preemption during the verdict must not discard a finished index.
#[derive(Debug)]
pub(crate) struct PendingRideAlong {
    plan: RideAlongPlan,
    scan: Option<StderrScan>,
}

impl PendingRideAlong {
    pub(crate) fn new(plan: RideAlongPlan, scan: Option<StderrScan>) -> Self {
        Self { plan, scan }
    }

    pub(crate) fn file_id(&self) -> i64 {
        self.plan.file_id
    }

    #[cfg(test)]
    pub(crate) fn stage(&self) -> &Path {
        self.plan.stage()
    }

    /// Take the verdicts.
    pub(crate) async fn judge(self) -> RideAlongHarvest {
        RideAlongHarvest::take(self.plan, self.scan).await
    }
}

/// A finished pass's ride-along: verdicts taken, nothing published yet. The
/// stage is still held; dropping this removes it.
#[derive(Debug)]
pub(crate) struct RideAlongHarvest {
    plan: RideAlongPlan,
    outcomes: Vec<TrackOutcome>,
}

pub(crate) struct ClusterPublishReceipt {
    pub(crate) manifest: Manifest,
    root: PathBuf,
    dir: PathBuf,
    previous: Option<Manifest>,
    placed: Vec<String>,
    written_rows: Vec<(SubtitleSourcePublication, Option<SubtitleSourcePublication>)>,
}

impl ClusterPublishReceipt {
    pub(crate) async fn rollback(self, catalog: &dyn Store) -> Result<(), String> {
        let _guard = store::file_lock(&self.root, self.manifest.file_id)
            .lock()
            .await;
        // Hydration can merge the manifest after our rename. Even then the
        // cancelled request's rows must go away. Compare the exact row first
        // so a later publisher's replacement is left intact.
        let current_rows = catalog
            .list_subtitle_source_publications(
                self.manifest.file_id,
                self.manifest.source.size as i64,
                self.manifest.source.mtime,
            )
            .await
            .map_err(|error| format!("reading rows for publication compensation: {error}"))?;
        let still_ours: Vec<_> = self
            .written_rows
            .iter()
            .filter(|(published, _)| current_rows.contains(published))
            .cloned()
            .collect();
        let current_manifest = store::read_manifest(&self.dir).await;
        if current_manifest.as_ref() != Some(&self.manifest) {
            rollback_cluster_rows(catalog, &still_ours).await?;
            PUBLISHED.fetch_sub(1, Ordering::Relaxed);
            return Err("subtitle-source manifest changed before local compensation".to_owned());
        }
        rollback_cluster_publish(
            catalog,
            &self.dir,
            self.previous.as_ref(),
            &self.placed,
            &still_ours,
        )
        .await?;
        PUBLISHED.fetch_sub(1, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) async fn commit(self) {
        let _guard = store::file_lock(&self.root, self.manifest.file_id)
            .lock()
            .await;
        if store::read_manifest(&self.dir).await.as_ref() == Some(&self.manifest) {
            if let Some(previous) = &self.previous {
                cleanup_replaced_files(&self.dir, previous, &self.manifest).await;
            }
        }
    }
}

impl RideAlongHarvest {
    /// Judge a reaped pass whose index was built. `scan` is `None` when the
    /// stderr reader did not finish.
    pub(crate) async fn take(plan: RideAlongPlan, scan: Option<StderrScan>) -> Self {
        let input = VerdictInput {
            tracks: plan.tracks.clone(),
            kinds: plan.kinds.clone(),
            stream_indexes: plan.stream_indexes.clone(),
            stage: plan.stage.path().to_owned(),
        };
        let fallback = input.clone();
        let outcomes = match tokio::task::spawn_blocking(move || verdicts(&input, scan.as_ref()))
            .await
        {
            Ok(outcomes) => outcomes,
            Err(error) => {
                tracing::warn!(%error, "the ride-along verdict task failed; every track is transient");
                verdicts(&fallback, None)
            }
        };
        for outcome in &outcomes {
            record_verdict(outcome);
            for representation in &outcome.representations {
                if let Some(reason) = &representation.reason {
                    tracing::info!(
                        file_id = plan.file_id,
                        ordinal = outcome.ordinal,
                        format = ?representation.format,
                        verdict = ?representation.verdict,
                        %reason,
                        "subtitle ride-along representation not kept"
                    );
                }
            }
        }
        Self { plan, outcomes }
    }

    pub(crate) fn outcomes(&self) -> &[TrackOutcome] {
        &self.outcomes
    }

    /// The manifest this harvest would publish, with every `kept` track under
    /// its content name. Pure: nothing is moved.
    fn manifest(&self) -> Manifest {
        let plan = &self.plan;
        let tracks = plan
            .probed
            .iter()
            .filter_map(|ordinal| {
                if let Some(entry) = plan.carried.iter().find(|entry| entry.ordinal == *ordinal) {
                    return Some(entry.clone());
                }
                let outcome = self
                    .outcomes
                    .iter()
                    .find(|outcome| outcome.ordinal == *ordinal)?;
                let attempts = plan
                    .prior_attempts
                    .get(ordinal)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1);
                let representations: Vec<_> = outcome
                    .representations
                    .iter()
                    .map(|representation| {
                        let named = representation
                            .sha256
                            .as_deref()
                            .filter(|_| representation.verdict == Verdict::Kept)
                            .and_then(|sha256| {
                                store::representation_file_name(
                                    *ordinal,
                                    representation.format,
                                    sha256,
                                )
                                .map(|name| (name, sha256.to_owned()))
                            });
                        RepresentationEntry {
                            format: representation.format,
                            origin: RepresentationOrigin::Extracted,
                            verdict: if representation.verdict == Verdict::Kept && named.is_none() {
                                Verdict::Malformed
                            } else {
                                representation.verdict
                            },
                            attempts,
                            file: named.as_ref().map(|(name, _)| name.clone()),
                            sha256: named.map(|(_, sha256)| sha256),
                            bytes: if representation.verdict == Verdict::Kept {
                                representation.bytes
                            } else {
                                0
                            },
                        }
                    })
                    .collect();
                let primary = representations.first()?;
                Some(TrackEntry {
                    ordinal: *ordinal,
                    kind: match plan.kind(*ordinal) {
                        ProbedKind::Pgs => TrackKind::Pgs,
                        ProbedKind::Text => TrackKind::Text,
                        ProbedKind::TextStyled => TrackKind::TextStyled,
                    },
                    representations: representations.clone(),
                    verdict: primary.verdict,
                    attempts,
                    file: primary.file.clone(),
                    sha256: primary.sha256.clone(),
                })
            })
            .collect();
        Manifest {
            version: MANIFEST_VERSION,
            file_id: plan.file_id,
            source: plan.source,
            ordinals: plan.probed.clone(),
            tracks,
        }
    }

    /// Publish into the store. The caller has already run the pass's
    /// freshness checks — `source_still_matches`, and the file row's
    /// size/mtime — so a pass that raced a rescan cannot recreate a directory
    /// the sweep just removed.
    ///
    /// The order is what lets a reader never see a manifest naming a file
    /// that is not there yet:
    ///
    /// 1. rename the new content-named `.sup` files into place;
    /// 2. write the manifest to a temporary file and rename it over the old
    ///    one;
    /// 3. only then delete the `.sup` files the replaced manifest named and
    ///    this one does not;
    /// 4. write the first `.access`, so a new directory is not first in line
    ///    under the size cap.
    #[cfg(test)]
    pub(crate) async fn publish(self) -> Result<Manifest, String> {
        self.publish_internal(None, None, None, || async {})
            .await
            .map(|receipt| receipt.manifest)
    }

    /// Publish the local bytes first, then expose the representations as
    /// `extracted` cluster rows. A peer can never fetch a row whose manifest
    /// rename has not happened yet.
    pub(crate) async fn publish_with_cluster(
        self,
        catalog: &dyn Store,
        node_id: &str,
        source_attestation: &str,
        guard: Option<&CancellationToken>,
    ) -> Result<Manifest, String> {
        if source_attestation.len() != 64
            || !source_attestation
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("invalid portable source attestation".to_owned());
        }
        self.publish_internal(
            Some((catalog, node_id, source_attestation)),
            guard,
            None,
            || async {},
        )
        .await
        .map(|receipt| receipt.manifest)
    }

    /// Worker publication keeps a compensation receipt until the fenced
    /// request has completed. Admin cancellation changes the request fence,
    /// which is checked around every externally visible write.
    pub(crate) async fn publish_guarded_worker(
        self,
        catalog: &dyn Store,
        node_id: &str,
        source_attestation: &str,
        lost: &CancellationToken,
        request: &AnalysisRequest,
    ) -> Result<ClusterPublishReceipt, String> {
        if source_attestation.len() != 64
            || !source_attestation
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("invalid portable source attestation".to_owned());
        }
        self.publish_internal(
            Some((catalog, node_id, source_attestation)),
            Some(lost),
            Some(request),
            || async {},
        )
        .await
    }

    async fn publish_internal<F, Fut>(
        self,
        cluster: Option<(&dyn Store, &str, &str)>,
        guard: Option<&CancellationToken>,
        request: Option<&AnalysisRequest>,
        after_row: F,
    ) -> Result<ClusterPublishReceipt, String>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = ()>,
    {
        let dir = store::file_dir(&self.plan.root, self.plan.file_id);
        let _file_guard = store::file_lock(&self.plan.root, self.plan.file_id)
            .lock()
            .await;
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|error| format!("creating {}: {error}", dir.display()))?;
        if guard.is_some_and(CancellationToken::is_cancelled) {
            let _ = tokio::fs::remove_dir(&dir).await;
            return Err("subtitle-source claim lost before publication".to_owned());
        }
        // Snapshot the exact rows this publish may replace. On a lost claim,
        // restore those rows rather than deleting coverage from a prior pass.
        let prior_rows = if let (Some((catalog, _, _)), Some(_)) = (cluster, guard) {
            match catalog
                .list_subtitle_source_publications(
                    self.plan.file_id,
                    self.plan.source.size as i64,
                    self.plan.source.mtime,
                )
                .await
            {
                Ok(rows) => rows,
                Err(error) => {
                    let _ = tokio::fs::remove_dir(&dir).await;
                    return Err(format!("reading prior subtitle-source rows: {error}"));
                }
            }
        } else {
            Vec::new()
        };
        let mut manifest = self.manifest();

        // (1)
        let mut placed = Vec::new();
        for track in &mut manifest.tracks {
            if self
                .plan
                .carried
                .iter()
                .any(|carried| carried.ordinal == track.ordinal)
            {
                continue;
            }
            for representation in &mut track.representations {
                let Some(name) = representation.file.clone() else {
                    continue;
                };
                // Durable before it is named: a manifest that survives a
                // power loss must not name bytes that did not.
                let staged = self
                    .plan
                    .representation_path(track.ordinal, representation.format);
                let synced = match tokio::fs::File::open(&staged).await {
                    Ok(file) => file.sync_all().await,
                    Err(error) => Err(error),
                };
                let placed_now = match synced {
                    Ok(()) => tokio::fs::rename(&staged, dir.join(&name)).await,
                    Err(error) => Err(error),
                };
                match placed_now {
                    Ok(()) => placed.push(name),
                    Err(error) => {
                        tracing::warn!(file_id = self.plan.file_id, ordinal = track.ordinal, format = ?representation.format, %error, "placing a stored subtitle representation");
                        representation.verdict = Verdict::Transient;
                        representation.file = None;
                        representation.sha256 = None;
                        representation.bytes = 0;
                    }
                }
            }
            if let Some(primary) = track.representations.first() {
                track.verdict = primary.verdict;
                track.file = primary.file.clone();
                track.sha256 = primary.sha256.clone();
            }
        }

        // What the directory's manifest names now, read at publish rather
        // than at plan time: another publish may have landed in between.
        let replaced = store::read_manifest(&dir).await;
        manifest = store::merge_manifest(manifest, replaced.as_ref());

        if !publication_claim_live(cluster, guard, request).await {
            remove_newly_placed(&dir, &placed, replaced.as_ref()).await;
            if replaced.is_none() {
                let _ = tokio::fs::remove_dir(&dir).await;
            }
            return Err("subtitle-source claim lost before manifest rename".to_owned());
        }

        // (2)
        if let Err(error) = write_manifest(&dir, &manifest).await {
            remove_newly_placed(&dir, &placed, replaced.as_ref()).await;
            if replaced.is_none() {
                let _ = tokio::fs::remove_dir(&dir).await;
            }
            return Err(error);
        }

        sync_directory(&dir).await;

        let mut written_rows = Vec::new();
        if let Some((catalog, node_id, source_attestation)) = cluster {
            for outcome in &self.outcomes {
                let Some(track) = manifest.track(outcome.ordinal) else {
                    continue;
                };
                for representation in &track.representations {
                    if representation.origin != RepresentationOrigin::Extracted
                        || !outcome
                            .representations
                            .iter()
                            .any(|produced| produced.format == representation.format)
                    {
                        continue;
                    }
                    if !publication_claim_live(cluster, guard, request).await {
                        rollback_cluster_publish(
                            catalog,
                            &dir,
                            replaced.as_ref(),
                            &placed,
                            &written_rows,
                        )
                        .await?;
                        return Err("subtitle-source claim lost during publication".to_owned());
                    }
                    let publication = SubtitleSourcePublication {
                        file_id: self.plan.file_id,
                        source_size: manifest.source.size as i64,
                        source_mtime: manifest.source.mtime,
                        source_attestation: source_attestation.to_owned(),
                        node_id: node_id.to_owned(),
                        ordinal: track.ordinal,
                        kind: match track.kind {
                            TrackKind::Pgs => "pgs",
                            TrackKind::Text => "text",
                            TrackKind::TextStyled => "text_styled",
                        }
                        .to_owned(),
                        format: representation.format.publication_name().to_owned(),
                        verdict: match representation.verdict {
                            Verdict::Kept => "kept",
                            Verdict::Empty => "empty",
                            Verdict::Malformed => "malformed",
                            Verdict::Transient => "transient",
                        }
                        .to_owned(),
                        attempts: i64::from(representation.attempts),
                        origin: "extracted".to_owned(),
                        sha256: representation.sha256.clone().unwrap_or_default(),
                        bytes: representation.bytes as i64,
                        published_at_ms: unix_ms(),
                    };
                    if guard.is_some() {
                        let prior = prior_rows
                            .iter()
                            .find(|row| {
                                row.node_id == publication.node_id
                                    && row.ordinal == publication.ordinal
                                    && row.format == publication.format
                            })
                            .cloned();
                        // Include this row before the write: a store error may
                        // follow a successful durable write.
                        written_rows.push((publication.clone(), prior));
                    }
                    if let Err(error) = catalog
                        .upsert_subtitle_source_publication(&publication)
                        .await
                    {
                        if guard.is_some() {
                            rollback_cluster_publish(
                                catalog,
                                &dir,
                                replaced.as_ref(),
                                &placed,
                                &written_rows,
                            )
                            .await?;
                        }
                        return Err(format!("publishing subtitle-source row: {error}"));
                    }
                    after_row().await;
                    if !publication_claim_live(cluster, guard, request).await {
                        rollback_cluster_publish(
                            catalog,
                            &dir,
                            replaced.as_ref(),
                            &placed,
                            &written_rows,
                        )
                        .await?;
                        return Err("subtitle-source claim lost during publication".to_owned());
                    }
                }
            }
        }

        if !publication_claim_live(cluster, guard, request).await {
            if let Some((catalog, _, _)) = cluster {
                rollback_cluster_publish(catalog, &dir, replaced.as_ref(), &placed, &written_rows)
                    .await?;
            }
            return Err("subtitle-source claim lost during publication".to_owned());
        }

        // (3) A guarded worker retains the prior bytes until its fenced
        // request completes, so cancellation can restore the old manifest.
        if guard.is_none() {
            if let Some(replaced) = &replaced {
                cleanup_replaced_files(&dir, replaced, &manifest).await;
            }
        }

        // (4)
        store::record_access(&dir).await;
        if !publication_claim_live(cluster, guard, request).await {
            if let Some((catalog, _, _)) = cluster {
                rollback_cluster_publish(catalog, &dir, replaced.as_ref(), &placed, &written_rows)
                    .await?;
            }
            return Err("subtitle-source claim lost at publication boundary".to_owned());
        }
        PUBLISHED.fetch_add(1, Ordering::Relaxed);
        Ok(ClusterPublishReceipt {
            manifest,
            root: self.plan.root.clone(),
            dir,
            previous: replaced,
            placed,
            written_rows,
        })
    }
}

async fn publication_claim_live(
    cluster: Option<(&dyn Store, &str, &str)>,
    guard: Option<&CancellationToken>,
    request: Option<&AnalysisRequest>,
) -> bool {
    if guard.is_some_and(CancellationToken::is_cancelled) {
        return false;
    }
    if let (Some((catalog, _, _)), Some(request)) = (cluster, request) {
        if !catalog
            .record_analysis_request_phase(request, "publishing", None, unix_ms())
            .await
            .unwrap_or(false)
        {
            return false;
        }
    }
    !guard.is_some_and(CancellationToken::is_cancelled)
}

async fn remove_newly_placed(dir: &Path, placed: &[String], replaced: Option<&Manifest>) {
    for name in placed {
        if !replaced.is_some_and(|old| names(old, name)) {
            let _ = tokio::fs::remove_file(dir.join(name)).await;
        }
    }
}

async fn cleanup_replaced_files(dir: &Path, replaced: &Manifest, manifest: &Manifest) {
    for track in &replaced.tracks {
        let reps = if track.representations.is_empty() {
            track
                .representation(RepresentationFormat::Sup)
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            track.representations.clone()
        };
        for representation in reps {
            let (Some(name), Some(sha256)) = (
                representation.file.as_deref(),
                representation.sha256.as_deref(),
            ) else {
                continue;
            };
            if store::representation_file_name(track.ordinal, representation.format, sha256)
                .as_deref()
                != Some(name)
                || names(manifest, name)
            {
                continue;
            }
            let _ = tokio::fs::remove_file(dir.join(name)).await;
        }
    }
}

/// A cancelled worker must leave neither its cluster rows nor a newly named
/// local manifest. The per-file lock is held by the caller throughout this
/// compensation, so no later local publish can be rolled back by mistake.
async fn rollback_cluster_publish(
    catalog: &dyn Store,
    dir: &Path,
    replaced: Option<&Manifest>,
    placed: &[String],
    written_rows: &[(SubtitleSourcePublication, Option<SubtitleSourcePublication>)],
) -> Result<(), String> {
    let mut errors = Vec::new();
    if let Err(error) = rollback_cluster_rows(catalog, written_rows).await {
        errors.push(error);
    }
    let restore = if let Some(replaced) = replaced {
        write_manifest(dir, replaced).await
    } else {
        match tokio::fs::remove_file(dir.join(MANIFEST_NAME)).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("removing cancelled manifest: {error}")),
        }
    };
    if let Err(error) = restore {
        errors.push(error);
    }
    remove_newly_placed(dir, placed, replaced).await;
    if replaced.is_none() {
        let _ = tokio::fs::remove_file(dir.join(".access")).await;
    }
    sync_directory(dir).await;
    if replaced.is_none() {
        let _ = tokio::fs::remove_dir(dir).await;
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

async fn rollback_cluster_rows(
    catalog: &dyn Store,
    written_rows: &[(SubtitleSourcePublication, Option<SubtitleSourcePublication>)],
) -> Result<(), String> {
    let mut errors = Vec::new();
    for (row, prior) in written_rows.iter().rev() {
        if let Err(error) = catalog
            .delete_subtitle_source_publication(
                row.file_id,
                row.source_size,
                row.source_mtime,
                &row.node_id,
                row.ordinal,
                &row.format,
            )
            .await
        {
            errors.push(format!("removing cancelled subtitle-source row: {error}"));
        }
        if let Some(prior) = prior {
            if let Err(error) = catalog.upsert_subtitle_source_publication(prior).await {
                errors.push(format!("restoring prior subtitle-source row: {error}"));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn names(manifest: &Manifest, name: &str) -> bool {
    manifest.tracks.iter().any(|track| {
        track.file.as_deref() == Some(name)
            || track
                .representations
                .iter()
                .any(|representation| representation.file.as_deref() == Some(name))
    })
}

/// Reconcile rows lost while the verified local manifest survived. Only a
/// representation this node extracted can establish cluster coverage;
/// hydrated bytes remain available to peers but never become extraction.
pub(crate) async fn publish_existing_manifest_rows(
    catalog: &dyn Store,
    node_id: &str,
    root: &Path,
    file_id: i64,
    source: &std::fs::File,
    source_attestation: &str,
) -> Result<usize, String> {
    if source_attestation.len() != 64
        || !source_attestation
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("invalid portable source attestation".to_owned());
    }
    let live = crate::fragment_index_cluster::source_stamp(
        &source
            .metadata()
            .map_err(|error| format!("fstat subtitle source: {error}"))?,
    );
    let _guard = store::file_lock(root, file_id).lock().await;
    let dir = store::file_dir(root, file_id);
    let Some(manifest) = store::read_manifest(&dir).await.filter(|manifest| {
        manifest.file_id == file_id && manifest.source_matches(&live, Consumer::Burn)
    }) else {
        return Ok(0);
    };
    let mut written = 0;
    for track in &manifest.tracks {
        let representations = if track.representations.is_empty() {
            track
                .representation(RepresentationFormat::Sup)
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            track.representations.clone()
        };
        for representation in representations {
            if representation.origin != RepresentationOrigin::Extracted {
                continue;
            }
            let bytes = if representation.verdict == Verdict::Kept {
                let Some((_, bytes)) = store::open_verified_for_peer(
                    root,
                    file_id,
                    track.ordinal,
                    representation.format,
                )
                .await
                else {
                    continue;
                };
                bytes as i64
            } else {
                0
            };
            let publication = SubtitleSourcePublication {
                file_id,
                source_size: manifest.source.size as i64,
                source_mtime: manifest.source.mtime,
                source_attestation: source_attestation.to_owned(),
                node_id: node_id.to_owned(),
                ordinal: track.ordinal,
                kind: match track.kind {
                    TrackKind::Pgs => "pgs",
                    TrackKind::Text => "text",
                    TrackKind::TextStyled => "text_styled",
                }
                .to_owned(),
                format: representation.format.publication_name().to_owned(),
                verdict: match representation.verdict {
                    Verdict::Kept => "kept",
                    Verdict::Empty => "empty",
                    Verdict::Malformed => "malformed",
                    Verdict::Transient => "transient",
                }
                .to_owned(),
                attempts: i64::from(representation.attempts),
                origin: "extracted".to_owned(),
                sha256: representation.sha256.unwrap_or_default(),
                bytes,
                published_at_ms: unix_ms(),
            };
            catalog
                .upsert_subtitle_source_publication(&publication)
                .await
                .map_err(|error| format!("reconciling subtitle-source row: {error}"))?;
            written += 1;
        }
    }
    Ok(written)
}

/// Make a directory's entries — the renames just done — durable. Best effort:
/// a platform that cannot open a directory for sync loses only the
/// durability, and the latch re-checks every `kept` file before trusting it.
async fn sync_directory(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = tokio::fs::File::open(dir).await {
        let _ = handle.sync_all().await;
    }
    #[cfg(not(unix))]
    let _ = dir;
}

async fn write_manifest(dir: &Path, manifest: &Manifest) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    let bytes =
        serde_json::to_vec(manifest).map_err(|error| format!("encoding the manifest: {error}"))?;
    let temporary = dir.join(format!(
        ".{MANIFEST_NAME}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await?;
        file.write_all(&bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temporary, dir.join(MANIFEST_NAME)).await
    }
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(format!(
            "writing {}: {error}",
            dir.join(MANIFEST_NAME).display()
        ));
    }
    Ok(())
}

/// Remove stage and self-test directories no process owns. Run where no
/// pass can be riding: at startup, before the self-test can open the gate,
/// and on the store's sweep tick for anything older than an hour.
pub(crate) async fn sweep_stale_stages(root: &Path, older_than: Duration) -> usize {
    let Ok(mut entries) = tokio::fs::read_dir(root).await else {
        return 0;
    };
    let mut removed = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !(name.starts_with(STAGE_PREFIX) || name.starts_with(SELF_TEST_PREFIX)) {
            continue;
        }
        let old_enough = entry
            .metadata()
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= older_than);
        if old_enough && tokio::fs::remove_dir_all(entry.path()).await.is_ok() {
            removed += 1;
        }
    }
    removed
}

// ---------------------------------------------------------------------------
// The startup self-test.

/// What a passing self-test measured.
#[derive(Debug)]
pub(crate) struct SelfTestReport {
    pub(crate) elapsed: Duration,
    /// The version both `ffmpeg` and `ffprobe` report.
    pub(crate) version: String,
}

/// The self-test, once, off the startup path. The ride-along stays off until
/// it passes; a failure is recorded with its reason for the Developer page.
pub(crate) async fn startup_self_test(
    runtime_cache: PathBuf,
    shutdown: tokio_util::sync::CancellationToken,
) {
    set_self_test_state(SelfTest::Running);
    let root = store::store_root(&runtime_cache);
    // Nothing can be riding yet — the gate is closed until this passes — so
    // every stage here is a previous process's leftover.
    let swept = sweep_stale_stages(&root, Duration::ZERO).await;
    if swept > 0 {
        tracing::info!(
            swept,
            "removed PGS ride-along stages a previous process left behind"
        );
    }
    let bin = crate::ffmpeg::ffmpeg_bin();
    let prober = crate::ffmpeg::ffprobe_bin();
    let started = Instant::now();
    let result = tokio::select! {
        result = tokio::time::timeout(SELF_TEST_BUDGET, run_self_test(&bin, &prober, &root, &runtime_cache)) => {
            result.unwrap_or_else(|_| Err(format!("did not finish within {}s", SELF_TEST_BUDGET.as_secs())))
        }
        () = shutdown.cancelled() => {
            set_self_test_state(SelfTest::NotRun);
            return;
        }
    };
    match result {
        Ok(report) => {
            tracing::info!(
                elapsed_ms = report.elapsed.as_millis() as u64,
                version = %report.version,
                "PGS ride-along self-test passed; the fragment-index pass may keep PGS tracks"
            );
            set_self_test_state(SelfTest::Passed {
                elapsed_ms: u64::try_from(report.elapsed.as_millis()).unwrap_or(u64::MAX),
                version: report.version,
            });
        }
        Err(reason) => {
            tracing::warn!(
                %reason,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "PGS ride-along self-test failed; the fragment-index pass will not keep PGS tracks"
            );
            set_self_test_state(SelfTest::Failed { reason });
        }
    }
}

/// Run the behaviour check against `bin`: the exact index argv plus the tee,
/// over a tiny synthetic h264 source with two PGS tracks, the second with its
/// first segment's length corrupted to `0xFFFF`. It requires:
///
/// - exit 0 from both the baseline and the ride-along run;
/// - index output byte-identical to the baseline;
/// - verdicts `kept` for the intact track and `malformed` for the corrupted
///   one, by the `framecrc` rule;
/// - the stderr failure line for the corrupted track's `sup` slave
///   recognised, **and** the corrupted track `malformed` by the byte
///   arithmetic alone, with no stderr failure to lean on — the `framecrc`
///   rule is the primary check and must be shown to catch it by itself;
/// - `ffprobe` (`prober`) reporting the same version as `ffmpeg`. The
///   ride-along's ordinals come from `ffprobe` and its hard maps are resolved
///   by `ffmpeg`; two builds that count subtitle streams differently — an
///   older prober reading a newer subtitle codec as `data` — would map a
///   track `ffmpeg` cannot find and fail the index pass itself.
///
/// `PLURX_FFMPEG` can name a build the design was never measured on, and a
/// muxer list proves neither the isolation nor the wording.
pub(crate) async fn run_self_test(
    bin: &str,
    prober: &str,
    root: &Path,
    runtime_cache: &Path,
) -> Result<SelfTestReport, String> {
    let started = Instant::now();
    let version = matching_versions(bin, prober, runtime_cache).await?;
    let work = StageDir::create(root, SELF_TEST_PREFIX)
        .map_err(|error| format!("creating the self-test directory: {error}"))?;
    let source = synthetic_source(bin, work.path(), runtime_cache).await?;
    let file = synthetic_media_file(&source, 4_000)?;
    let stamp = crate::fragment_index_cluster::source_stamp(
        &std::fs::metadata(&source)
            .map_err(|error| format!("stat the synthetic source: {error}"))?,
    );
    let plan = RideAlongPlan::standalone(work.path(), 0, stamp, vec![0, 1])
        .map_err(|error| format!("creating the self-test stage: {error}"))?;
    let video = plurx_core::transcode::CopyVideoOptions::new(false, false);
    let input = source.to_string_lossy().into_owned();
    let baseline_args = crate::fragindex::index_argv(&file, video, Some(&input), None);
    let riding_args = crate::fragindex::index_argv(&file, video, Some(&input), Some(&plan));

    let baseline = run_ffmpeg(bin, &baseline_args, runtime_cache).await?;
    if !baseline.status_ok {
        return Err(format!(
            "the baseline index run failed: {}",
            baseline.stderr_text()
        ));
    }
    let riding = run_ffmpeg(bin, &riding_args, runtime_cache).await?;
    if !riding.status_ok {
        return Err(format!(
            "the index run with the tee exited non-zero: {}",
            riding.stderr_text()
        ));
    }
    if baseline.stdout.is_empty() || baseline.stdout != riding.stdout {
        return Err(format!(
            "the index output changed with the tee ({} bytes without, {} with)",
            baseline.stdout.len(),
            riding.stdout.len()
        ));
    }
    let mut scan = plan.stderr_scan();
    scan.feed(&riding.stderr);
    scan.finish();
    if scan.failure(2).is_none() {
        return Err(format!(
            "the corrupted track's slave failure was not recognised on stderr: {}",
            riding.stderr_text()
        ));
    }
    let input = VerdictInput {
        tracks: plan.tracks.clone(),
        kinds: plan.kinds.clone(),
        stream_indexes: plan.stream_indexes.clone(),
        stage: plan.stage().to_owned(),
    };
    let (corrupted_sup, corrupted_crc) = (plan.sup_path(1), plan.crc_path(1));
    let (outcomes, by_arithmetic) = tokio::task::spawn_blocking(move || {
        (
            verdicts(&input, Some(&scan)),
            judge(&corrupted_sup, &corrupted_crc, [None, None]),
        )
    })
    .await
    .map_err(|error| format!("the self-test verdict task failed: {error}"))?;
    let arithmetic = by_arithmetic
        .2
        .as_deref()
        .is_some_and(|reason| reason.contains("framecrc counted"));
    if by_arithmetic.0 != Verdict::Malformed || !arithmetic {
        return Err(format!(
            "the framecrc rule alone judged the corrupted track {:?}, not malformed ({:?})",
            by_arithmetic.0, by_arithmetic.2
        ));
    }
    let got: Vec<Verdict> = outcomes.iter().map(|outcome| outcome.verdict).collect();
    if got != [Verdict::Kept, Verdict::Malformed] {
        return Err(format!(
            "expected [kept, malformed] from the framecrc rule, got {got:?} ({:?})",
            outcomes
                .iter()
                .map(|outcome| &outcome.reason)
                .collect::<Vec<_>>()
        ));
    }
    Ok(SelfTestReport {
        elapsed: started.elapsed(),
        version,
    })
}

/// The version token of `bin -version`'s first line (`ffmpeg version 8.0.1-3
/// Copyright …` → `8.0.1-3`), for both executables, and the one both report.
async fn matching_versions(
    bin: &str,
    prober: &str,
    runtime_cache: &Path,
) -> Result<String, String> {
    let version_of = |run: &FfmpegRun, name: &str| -> Result<String, String> {
        let text = String::from_utf8_lossy(&run.stdout);
        version_token(&text)
            .ok_or_else(|| format!("{name} -version printed no version: {}", run.stderr_text()))
    };
    let arg = ["-version".to_owned()];
    let engine = version_of(&run_ffmpeg(bin, &arg, runtime_cache).await?, bin)?;
    let probe = version_of(&run_ffmpeg(prober, &arg, runtime_cache).await?, prober)?;
    if engine != probe {
        return Err(format!(
            "ffprobe ({prober}) is version {probe} and ffmpeg ({bin}) is version {engine}: \
             the ride-along takes its subtitle ordinals from one and hands them to the other"
        ));
    }
    Ok(engine)
}

fn version_token(text: &str) -> Option<String> {
    let line = text.lines().find(|line| !line.trim().is_empty())?;
    let mut words = line.split_whitespace();
    words.find(|word| *word == "version")?;
    words.next().map(str::to_owned)
}

struct FfmpegRun {
    status_ok: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl FfmpegRun {
    fn stderr_text(&self) -> String {
        let text = String::from_utf8_lossy(&self.stderr);
        let text = text.trim();
        let start = text.len().saturating_sub(600);
        let start = (start..text.len())
            .find(|index| text.is_char_boundary(*index))
            .unwrap_or(text.len());
        text[start..].to_owned()
    }
}

/// One bounded `ffmpeg` run: killed on drop, reaped under a timeout, stdout
/// capped.
async fn run_ffmpeg(bin: &str, args: &[String], runtime_cache: &Path) -> Result<FfmpegRun, String> {
    let mut command = tokio::process::Command::new(bin);
    crate::producer_spawn::configure_ffmpeg_runtime(&mut command, runtime_cache);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(
        SELF_TEST_STEP_BUDGET,
        crate::process_control::output_job_owned(
            &mut command,
            crate::process_control::ChildWork::background("PGS ride-along self-test"),
        ),
    )
    .await
    .map_err(|_| {
        format!(
            "{bin} did not finish within {}s",
            SELF_TEST_STEP_BUDGET.as_secs()
        )
    })?
    .map_err(|error| format!("running {bin}: {error}"))?;
    if output.stdout.len() > SELF_TEST_MAX_STDOUT_BYTES {
        return Err(format!(
            "{bin} wrote {} bytes to stdout",
            output.stdout.len()
        ));
    }
    Ok(FfmpegRun {
        status_ok: output.status.success(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

/// Write the synthetic source: four seconds of 64x64 h264 and the authored
/// PGS track twice, then corrupt the second track's first segment length.
async fn synthetic_source(bin: &str, dir: &Path, runtime_cache: &Path) -> Result<PathBuf, String> {
    let sup = dir.join("authored.sup");
    tokio::fs::write(&sup, SELF_TEST_PGS)
        .await
        .map_err(|error| format!("writing the authored PGS track: {error}"))?;
    let source = dir.join("source.mkv");
    let args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=64x64:r=5:d=4",
        "-i",
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .chain([sup.to_string_lossy().into_owned()])
    .chain(
        [
            "-map",
            "0:v",
            "-map",
            "1:s",
            "-map",
            "1:s",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "5",
            "-c:s",
            "copy",
            "-f",
            "matroska",
        ]
        .iter()
        .map(|arg| (*arg).to_owned()),
    )
    .chain([source.to_string_lossy().into_owned()])
    .collect();
    let muxed = run_ffmpeg(bin, &args, runtime_cache).await?;
    if !muxed.status_ok {
        return Err(format!(
            "could not generate the synthetic source: {}",
            muxed.stderr_text()
        ));
    }
    let mut bytes = tokio::fs::read(&source)
        .await
        .map_err(|error| format!("reading the synthetic source: {error}"))?;
    corrupt_first_segment_length(&mut bytes, 3)
        .ok_or_else(|| "could not find the second PGS track's first block to corrupt".to_owned())?;
    tokio::fs::write(&source, &bytes)
        .await
        .map_err(|error| format!("writing the corrupted source: {error}"))?;
    Ok(source)
}

/// Set the first segment length of `track`'s first PGS block to `0xFFFF`.
///
/// Matroska carries a block as its element ID (`0xA1` Block or `0xA3`
/// SimpleBlock), a size, the track number as a one-byte EBML varint, a
/// two-byte timecode and a flags byte, then the payload. The authored track's
/// first display set opens with a presentation composition segment (`0x16`)
/// of 19 bytes, so the payload starts `16 00 13`. The `sup` muxer walks a
/// packet's segments by that length and refuses one that runs past the
/// packet; `framecrc`, which does not parse PGS, counts every byte anyway.
pub(crate) fn corrupt_first_segment_length(bytes: &mut [u8], track: u8) -> Option<()> {
    let track_byte = 0x80 | track;
    let position = (0..bytes.len().saturating_sub(10)).find(|&at| {
        matches!(bytes[at], 0xA1 | 0xA3)
            && bytes[at + 1] & 0xC0 == 0x40
            && bytes[at + 3] == track_byte
            && bytes[at + 7..at + 10] == [0x16, 0x00, 0x13]
    })?;
    bytes[position + 8] = 0xFF;
    bytes[position + 9] = 0xFF;
    Some(())
}

pub(crate) fn synthetic_media_file(
    source: &Path,
    duration_ms: i64,
) -> Result<plurx_core::domain::MediaFile, String> {
    let metadata =
        std::fs::metadata(source).map_err(|error| format!("stat the synthetic source: {error}"))?;
    Ok(plurx_core::domain::MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 0,
        item_id: 0,
        path: source.to_owned(),
        size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
        mtime: crate::fragment_index_cluster::source_stamp(&metadata).mtime,
        duration_ms: Some(duration_ms),
        container: Some("matroska".to_owned()),
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
        bitrate: None,
        audio_streams: vec![],
        subtitle_streams: vec![],
        scanned_at: 0,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: Default::default(),
    })
}

// ---------------------------------------------------------------------------
// Counters.

static ATTEMPTED: AtomicU64 = AtomicU64::new(0);
static VERDICTS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static WRITTEN_BYTES: AtomicU64 = AtomicU64::new(0);
static PUBLISHED: AtomicU64 = AtomicU64::new(0);
static DISCARDED: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// Why a finished riding pass's tracks were not published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Discard {
    /// The switch was turned off while the pass ran: the consumers ignore
    /// the store now, so nothing is added to it.
    SwitchOff,
    /// The held source is no longer the object the pass read.
    SourceMoved,
    /// The catalog row's size or mtime moved during the pass.
    CatalogMoved,
}

const DISCARD_LABELS: [(Discard, &str); 3] = [
    (Discard::SwitchOff, "switch_off"),
    (Discard::SourceMoved, "source_moved"),
    (Discard::CatalogMoved, "catalog_moved"),
];

fn discard_index(reason: Discard) -> usize {
    match reason {
        Discard::SwitchOff => 0,
        Discard::SourceMoved => 1,
        Discard::CatalogMoved => 2,
    }
}

/// Count a harvest discarded before publication.
pub(crate) fn record_discard(reason: Discard) {
    DISCARDED[discard_index(reason)].fetch_add(1, Ordering::Relaxed);
}

/// Harvests discarded since this process started, by reason.
pub(crate) fn discarded(reason: Discard) -> u64 {
    DISCARDED[discard_index(reason)].load(Ordering::Relaxed)
}

const VERDICT_LABELS: [(Verdict, &str); 4] = [
    (Verdict::Kept, "kept"),
    (Verdict::Empty, "empty"),
    (Verdict::Malformed, "malformed"),
    (Verdict::Transient, "transient"),
];

fn verdict_index(verdict: Verdict) -> usize {
    match verdict {
        Verdict::Kept => 0,
        Verdict::Empty => 1,
        Verdict::Malformed => 2,
        Verdict::Transient => 3,
    }
}

fn record_verdict(outcome: &TrackOutcome) {
    ATTEMPTED.fetch_add(1, Ordering::Relaxed);
    VERDICTS[verdict_index(outcome.verdict)].fetch_add(1, Ordering::Relaxed);
    WRITTEN_BYTES.fetch_add(outcome.written, Ordering::Relaxed);
}

/// `(tracks attempted, [kept, empty, malformed, transient], bytes written,
/// manifests published)` since this process started.
pub(crate) fn snapshot() -> (u64, [u64; 4], u64, u64) {
    (
        ATTEMPTED.load(Ordering::Relaxed),
        [0, 1, 2, 3].map(|index| VERDICTS[index].load(Ordering::Relaxed)),
        WRITTEN_BYTES.load(Ordering::Relaxed),
        PUBLISHED.load(Ordering::Relaxed),
    )
}

/// Prometheus exposition for the producer.
pub(crate) fn prometheus() -> String {
    use std::fmt::Write as _;

    let (attempted, _, written, published) = snapshot();
    let mut out = format!(
        "# HELP plurx_subtitle_ride_along_tracks_total PGS tracks the fragment-index pass tried to keep.\n\
         # TYPE plurx_subtitle_ride_along_tracks_total counter\n\
         plurx_subtitle_ride_along_tracks_total {attempted}\n\
         # HELP plurx_subtitle_ride_along_verdicts_total Ride-along track verdicts by kind.\n\
         # TYPE plurx_subtitle_ride_along_verdicts_total counter\n"
    );
    for (verdict, label) in VERDICT_LABELS {
        let _ = writeln!(
            out,
            "plurx_subtitle_ride_along_verdicts_total{{verdict=\"{label}\"}} {}",
            VERDICTS[verdict_index(verdict)].load(Ordering::Relaxed)
        );
    }
    let _ = write!(
        out,
        "# HELP plurx_subtitle_ride_along_written_bytes_total Bytes the ride-along wrote into its stages.\n\
         # TYPE plurx_subtitle_ride_along_written_bytes_total counter\n\
         plurx_subtitle_ride_along_written_bytes_total {written}\n\
         # HELP plurx_subtitle_ride_along_published_total Stored-subtitle manifests the ride-along published.\n\
         # TYPE plurx_subtitle_ride_along_published_total counter\n\
         plurx_subtitle_ride_along_published_total {published}\n\
         # HELP plurx_subtitle_ride_along_discarded_total Finished riding passes whose tracks were not published, by reason.\n\
         # TYPE plurx_subtitle_ride_along_discarded_total counter\n"
    );
    for (reason, label) in DISCARD_LABELS {
        let _ = writeln!(
            out,
            "plurx_subtitle_ride_along_discarded_total{{reason=\"{label}\"}} {}",
            discarded(reason)
        );
    }
    out
}

#[cfg(test)]
pub(crate) mod testing {
    //! Fixtures for the producer's tests: real sources muxed by `ffmpeg`,
    //! with the authored PGS track the self-test also uses.

    use super::*;
    use plurx_core::testfixtures;

    /// One subtitle track of a fixture, in stream order.
    #[derive(Clone, Copy, Debug)]
    pub(crate) enum Sub {
        /// The authored PGS track: two display sets, sixteen segments.
        Pgs,
        /// The same track delayed past the end of the file, so the output
        /// `-t` cuts every packet: a real PGS track with no cues.
        EmptyPgs,
        /// A text track, which shifts every later subtitle ordinal.
        Srt,
    }

    pub(crate) struct Fixture {
        pub(crate) dir: tempfile::TempDir,
        pub(crate) source: PathBuf,
        pub(crate) file: plurx_core::domain::MediaFile,
    }

    pub(crate) fn run_ffmpeg_to_completion(args: &[String]) {
        testfixtures::run(std::process::Command::new(testfixtures::ffmpeg()).args(args));
    }

    /// A sixteen-second 64x64 h264 Matroska file with `subs` in order,
    /// catalogued as file `id` — distinct per test, because a ride-along
    /// claims its file for the length of the pass.
    pub(crate) fn fixture(id: i64, subs: &[Sub]) -> Fixture {
        testfixtures::require_ffmpeg();
        let dir = crate::test_tempdir().expect("fixture directory");
        let sup = dir.path().join("authored.sup");
        std::fs::write(&sup, SELF_TEST_PGS).expect("authored track");
        let srt = dir.path().join("text.srt");
        std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:02,000\nhello\n\n").expect("text track");
        let source = dir.path().join("source.mkv");
        let mut args: Vec<String> = [
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x64:r=5:d=16",
            "-i",
        ]
        .map(str::to_owned)
        .to_vec();
        args.push(sup.to_string_lossy().into_owned());
        args.extend(["-itsoffset", "100", "-i"].map(str::to_owned));
        args.push(sup.to_string_lossy().into_owned());
        args.push("-i".to_owned());
        args.push(srt.to_string_lossy().into_owned());
        args.extend(["-map", "0:v"].map(str::to_owned));
        for sub in subs {
            args.push("-map".to_owned());
            args.push(
                match sub {
                    Sub::Pgs => "1:s",
                    Sub::EmptyPgs => "2:s",
                    Sub::Srt => "3:s",
                }
                .to_owned(),
            );
        }
        args.extend(
            [
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "5",
                "-c:s",
                "copy",
                "-t",
                "16",
                "-f",
                "matroska",
            ]
            .map(str::to_owned),
        );
        args.push(source.to_string_lossy().into_owned());
        run_ffmpeg_to_completion(&args);
        let mut file = synthetic_media_file(&source, 16_000).expect("fixture media file");
        file.id = id;
        Fixture { dir, source, file }
    }

    /// Corrupt the first segment of `track`'s first block (Matroska track
    /// numbers: video is 1, the first subtitle 2).
    pub(crate) fn corrupt(fixture: &mut Fixture, track: u8) {
        let mut bytes = std::fs::read(&fixture.source).expect("fixture bytes");
        corrupt_first_segment_length(&mut bytes, track).expect("a block to corrupt");
        std::fs::write(&fixture.source, bytes).expect("corrupted fixture");
        let id = fixture.file.id;
        fixture.file = synthetic_media_file(&fixture.source, 16_000).expect("fixture media file");
        fixture.file.id = id;
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::fragindex::{IndexBuild, IndexOutcome};
    use crate::subtitle_source::testing::{settled as settled_entry, stamp_of, write_manifest};
    use crate::subtitle_source::{Live, Lookup, TRANSIENT_ATTEMPTS};
    use plurx_core::store::ClusterFragmentIndexStore;

    const VIDEO: plurx_core::transcode::CopyVideoOptions =
        plurx_core::transcode::CopyVideoOptions::new(false, false);

    fn probe(streams: &[(u64, &str, &str)]) -> String {
        serde_json::json!({
            "streams": streams
                .iter()
                .map(|(index, kind, codec)| serde_json::json!({
                    "index": index, "codec_type": kind, "codec_name": codec,
                }))
                .collect::<Vec<_>>()
        })
        .to_string()
    }

    // -- ordinals --------------------------------------------------------

    #[test]
    fn pgs_ordinals_count_every_subtitle_stream_in_stream_order() {
        // Out of order on purpose: the ordinal follows the stream index.
        let raw = probe(&[
            (3, "subtitle", PGS_CODEC),
            (0, "video", "h264"),
            (1, "subtitle", "subrip"),
            (4, "audio", "aac"),
            (2, "subtitle", PGS_CODEC),
            (5, "subtitle", "ass"),
            (6, "subtitle", PGS_CODEC),
        ]);
        assert_eq!(pgs_ordinals_from_probe(&raw), vec![1, 2, 4]);
        assert!(pgs_ordinals_from_probe(&probe(&[(0, "video", "h264")])).is_empty());
        // A document this cannot read is no ordinals, never a guess.
        assert!(pgs_ordinals_from_probe("not json").is_empty());
        let unindexed = serde_json::json!({"streams": [
            {"index": 0, "codec_type": "subtitle", "codec_name": PGS_CODEC},
            {"codec_type": "subtitle", "codec_name": PGS_CODEC},
        ]});
        assert!(pgs_ordinals_from_probe(&unindexed.to_string()).is_empty());
    }

    #[test]
    fn eligible_text_ordinals_are_typed_from_the_held_fd_probe() {
        let raw = probe(&[
            (8, "subtitle", "ssa"),
            (0, "video", "h264"),
            (1, "subtitle", "subrip"),
            (2, "subtitle", "dvd_subtitle"),
            (3, "subtitle", "ass"),
            (4, "subtitle", "mov_text"),
            (5, "subtitle", "dvb_subtitle"),
            (6, "subtitle", "webvtt"),
            (7, "subtitle", "text"),
            (9, "subtitle", PGS_CODEC),
            (10, "subtitle", "unknown"),
        ]);
        let actual: Vec<_> = eligible_tracks_from_probe(&raw)
            .into_iter()
            .map(|track| (track.ordinal, track.kind, track.stream_index))
            .collect();
        assert_eq!(
            actual,
            vec![
                (0, ProbedKind::Text, 1),
                (2, ProbedKind::TextStyled, 3),
                (3, ProbedKind::Text, 4),
                (5, ProbedKind::Text, 6),
                (6, ProbedKind::Text, 7),
                (7, ProbedKind::TextStyled, 8),
                (8, ProbedKind::Pgs, 9),
            ]
        );
    }

    #[test]
    fn ass_double_map_has_webvtt_and_matroska_slaves_after_the_index_pipe() {
        let root = crate::test_tempdir().expect("stage");
        let source = root.path().join("source");
        std::fs::write(&source, b"x").expect("source");
        let mut plan = RideAlongPlan::standalone(root.path(), 1, stamp_of(&source), vec![0, 1, 2])
            .expect("plan");
        plan.kinds.insert(0, ProbedKind::Text);
        plan.kinds.insert(1, ProbedKind::TextStyled);
        let args = plan.args();
        assert_eq!(args.iter().filter(|arg| arg.as_str() == "0:s:1").count(), 2);
        assert!(args.windows(2).any(|pair| pair == ["-c:s:2", "copy"]));
        assert!(args.windows(2).any(|pair| pair == ["-c:s:1", "webvtt"]));
        let spec = args.last().expect("tee spec");
        assert!(spec.contains("[select=0:f=webvtt:onfail=ignore]"));
        assert!(spec.contains("[select=1:f=webvtt:onfail=ignore]"));
        assert!(spec.contains("[select=2:f=matroska:onfail=ignore]"));
        assert!(spec.contains("[select=3:f=sup:onfail=ignore]"));
        assert!(spec.ends_with("|[f=null]-"));
    }

    // -- tee escaping ----------------------------------------------------

    /// `libavutil/avstring.c`'s `av_get_token`, as `tee.c` calls it with
    /// `"|"`: the one unescaping pass a slave's file name goes through.
    fn av_get_token(input: &str) -> (String, &str) {
        const WHITESPACE: &[char] = &[' ', '\n', '\t', '\r'];
        let mut rest = input.trim_start_matches(WHITESPACE);
        let mut out = String::new();
        let mut end = 0;
        while let Some(character) = rest.chars().next() {
            if character == '|' {
                break;
            }
            rest = &rest[character.len_utf8()..];
            if character == '\\' {
                if let Some(next) = rest.chars().next() {
                    out.push(next);
                    rest = &rest[next.len_utf8()..];
                    end = out.len();
                    continue;
                }
                out.push(character);
            } else if character == '\'' {
                let close = rest.find('\'');
                let quoted = &rest[..close.unwrap_or(rest.len())];
                out.push_str(quoted);
                rest = &rest[quoted.len()..];
                if close.is_some() {
                    rest = &rest[1..];
                    end = out.len();
                }
            } else {
                out.push(character);
            }
        }
        let trimmed = out.trim_end_matches(WHITESPACE).len().max(end);
        out.truncate(trimmed);
        (out, rest)
    }

    #[test]
    fn a_tee_escaped_name_survives_the_tee_tokeniser_exactly() {
        for name in [
            "/cache/runtime/subtitle-source-v1/.stage-1/s0.sup",
            r"C:\cache\runtime\s0.sup",
            "/a b/ c |d/[e]/'f'/g\\h/s1.crc ",
            "/trailing space /s0.sup",
        ] {
            let slave = format!("[select=0:f=sup:onfail=ignore]{}", tee_escape(name));
            let spec = format!("{slave}|[f=null]-");
            let (token, rest) = av_get_token(&spec);
            assert_eq!(
                rest, "|[f=null]-",
                "the slave ends at its own delimiter: {spec}"
            );
            assert_eq!(
                token.strip_prefix("[select=0:f=sup:onfail=ignore]"),
                Some(name),
                "{spec}"
            );
        }
        // Unescaped, the Windows path is a different path (§6.2 fact 8).
        let (token, _) = av_get_token(r"[f=sup]C:\cache\s0.sup");
        assert_eq!(token, "[f=sup]C:caches0.sup");
    }

    // -- stderr ----------------------------------------------------------

    #[test]
    fn the_stderr_scan_reads_both_failure_lines_across_chunk_boundaries() {
        let slaves = vec![
            "/st/s0.sup".to_owned(),
            "/st/s0.crc".to_owned(),
            "/st/s1.sup".to_owned(),
            "/st/s1.crc".to_owned(),
            "/st/s11.sup".to_owned(),
        ];
        let mut scan = StderrScan::new(slaves);
        let text = "[sup @ 0x1] Not enough data, skipping 884 bytes\n\
                    [tee @ 0x2] Slave muxer #2 failed: Invalid data found when processing input, continuing with 4/5 slaves.\n\
                    [tee @ 0x2] Slave '[select=0:f=framecrc:onfail=ignore]/st/s0.crc': error opening: No space left on device\r\n\
                    [tee @ 0x2] Slave muxer #7 failed: Input/output error, continuing with 3/5 slaves.";
        for chunk in text.as_bytes().chunks(7) {
            scan.feed(chunk);
        }
        scan.finish();
        assert_eq!(
            scan.failure(2),
            Some("Invalid data found when processing input")
        );
        assert_eq!(scan.failure(1), Some("No space left on device"));
        assert_eq!(scan.failure(7), Some("Input/output error"));
        assert_eq!(scan.failure(0), None, "s0.sup is not s0.crc");
        assert_eq!(scan.failure(4), None, "/st/s1.sup names s1, not s11");
        assert!(is_os_error(scan.failure(1).unwrap_or_default()));
        assert!(!is_os_error(scan.failure(2).unwrap_or_default()));
    }

    // -- the verdict table, on hand-made stages --------------------------

    const INTACT_CRC: &str = "#software: Lavf62.3.100\n#tb 0: 1/1000\n#media_type 0: subtitle\n\
        #codec_id 0: hdmv_pgs_subtitle\n\
        0,       1000,       1000,        0,     1142, 0x5021ba81\n\
        0,       7000,       7000,        0,       30, 0x29b90386, F=0x0\n\
        0,       9000,       9000,        0,     1142, 0x61a7ba85, S=1, 8, 0x00000000\n\
        0,      15000,      15000,        0,       30, 0x29e30388\n";

    fn stage_with(sup: Option<&[u8]>, crc: Option<&str>) -> tempfile::TempDir {
        let stage = crate::test_tempdir().expect("stage");
        if let Some(sup) = sup {
            std::fs::write(stage.path().join("s0.sup"), sup).expect("sup");
        }
        if let Some(crc) = crc {
            std::fs::write(stage.path().join("s0.crc"), crc).expect("crc");
        }
        stage
    }

    fn judged(stage: &Path, failures: [Option<&str>; 2]) -> Verdict {
        judge(&stage.join("s0.sup"), &stage.join("s0.crc"), failures).0
    }

    /// The authored track has four packets carrying sixteen segments. The
    /// sums agree only at ten bytes of header per segment, with the segment
    /// count taken from walking the `.sup` — never at thirteen, and never
    /// with the packet count.
    #[test]
    fn the_framecrc_rule_is_ten_bytes_per_walked_segment() {
        let size = SELF_TEST_PGS.len() as u64;
        let walk = walk_sup(
            std::fs::File::open(stage_with(Some(SELF_TEST_PGS), None).path().join("s0.sup"))
                .expect("open"),
        )
        .expect("walk");
        let totals = crc_totals(INTACT_CRC).expect("crc");
        assert_eq!((size, walk.segments, totals.packets), (2504, 16, 4));
        assert_eq!(
            walk.packet_bytes, totals.bytes,
            "Σ(3 + len) is the framecrc total"
        );
        assert_eq!(size - 10 * walk.segments, totals.bytes);
        assert_ne!(
            size - 13 * walk.segments,
            totals.bytes,
            "minus 13 per segment"
        );
        assert_ne!(size - 10 * totals.packets, totals.bytes, "per packet");
        let stage = stage_with(Some(SELF_TEST_PGS), Some(INTACT_CRC));
        assert_eq!(judged(stage.path(), [None, None]), Verdict::Kept);
    }

    #[test]
    fn every_row_of_the_verdict_table() {
        let header = "#software: Lavf\n#tb 0: 1/1000\n";
        // Zero packets under a header, no failure: a real track with no cues.
        assert_eq!(
            judged(stage_with(Some(b""), Some(header)).path(), [None, None]),
            Verdict::Empty
        );
        // An OS error on either slave, or no companion at all.
        for failure in [
            [Some("No space left on device"), None],
            [None, Some("Permission denied")],
            [
                Some("Input/output error"),
                Some("Invalid data found when processing input"),
            ],
        ] {
            assert_eq!(
                judged(
                    stage_with(Some(SELF_TEST_PGS), Some(INTACT_CRC)).path(),
                    failure
                ),
                Verdict::Transient
            );
        }
        assert_eq!(
            judged(stage_with(Some(SELF_TEST_PGS), None).path(), [None, None]),
            Verdict::Transient
        );
        assert_eq!(
            judged(stage_with(Some(b""), Some("")).path(), [None, None]),
            Verdict::Transient,
            "never written"
        );
        // Any other reported failure, even when the totals agree.
        assert_eq!(
            judged(
                stage_with(Some(SELF_TEST_PGS), Some(INTACT_CRC)).path(),
                [None, Some("Broken pipe at close")]
            ),
            Verdict::Malformed
        );
        // Totals that disagree: a track cut at a display-set boundary is a
        // valid shorter stream (§6.2 fact 6) — three of the four sets here.
        let shorter = &SELF_TEST_PGS[..2444];
        assert!(walk_sup(
            std::fs::File::open(stage_with(Some(shorter), None).path().join("s0.sup"))
                .expect("open")
        )
        .is_ok());
        assert_eq!(
            judged(
                stage_with(Some(shorter), Some(INTACT_CRC)).path(),
                [None, None]
            ),
            Verdict::Malformed
        );
        // A walk that leaves bytes over, or runs off the end.
        let mut extra = SELF_TEST_PGS.to_vec();
        extra.extend_from_slice(b"PG");
        assert_eq!(
            judged(
                stage_with(Some(&extra), Some(INTACT_CRC)).path(),
                [None, None]
            ),
            Verdict::Malformed
        );
        assert_eq!(
            judged(
                stage_with(Some(&SELF_TEST_PGS[..2500]), Some(INTACT_CRC)).path(),
                [None, None]
            ),
            Verdict::Malformed
        );
        // Packets counted and no track written.
        assert_eq!(
            judged(stage_with(None, Some(INTACT_CRC)).path(), [None, None]),
            Verdict::Malformed
        );
        // An unreadable framecrc line.
        assert_eq!(
            judged(
                stage_with(Some(SELF_TEST_PGS), Some("#h\n0, 1, 1, 0\n")).path(),
                [None, None]
            ),
            Verdict::Malformed
        );
        // Totals that agree over bytes the PGS parser refuses: every segment
        // well framed, the first one's type no segment type at all.
        let mut unparseable = SELF_TEST_PGS.to_vec();
        unparseable[10] = 0x42;
        assert_eq!(
            judged(
                stage_with(Some(&unparseable), Some(INTACT_CRC)).path(),
                [None, None]
            ),
            Verdict::Malformed
        );
    }

    #[test]
    fn text_packet_cue_count_rule_requires_a_valid_webvtt_cue_per_packet() {
        let stage = crate::test_tempdir().expect("stage");
        let vtt = stage.path().join("s0.vtt");
        let crc = stage.path().join("s0.crc");
        let two = "#software: Lavf\n#media_type 0: subtitle\n0, 1000, 1000, 0, 5, 0x1\n0, 3000, 3000, 0, 5, 0x2\n";
        std::fs::write(&crc, two).expect("crc");
        std::fs::write(
            &vtt,
            b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\none\n\n00:00:03.000 --> 00:00:04.000\ntwo\n",
        )
        .expect("vtt");
        assert_eq!(
            judge_text_vtt(&vtt, &crc, [None, None], false).0,
            Verdict::Kept
        );
        std::fs::write(&vtt, b"WEBVTT\n\n00:00:03.000 --> 00:00:04.000\ntwo\n").expect("short vtt");
        assert_eq!(
            judge_text_vtt(&vtt, &crc, [None, None], false).0,
            Verdict::Malformed
        );
        std::fs::write(&vtt, b"WEBVTT\n\ninvalid --> cue\ntext\n").expect("bad vtt");
        assert_eq!(
            judge_text_vtt(&vtt, &crc, [None, None], false).0,
            Verdict::Malformed
        );
    }

    #[test]
    fn corrupt_text_packet_decoder_error_vetoes_equal_output_counts_for_only_its_stream() {
        let stage = crate::test_tempdir().expect("stage");
        let vtt = stage.path().join("s0.vtt");
        let crc = stage.path().join("s0.crc");
        // M0 E5: ffmpeg silently dropped the corrupt first packet from both
        // outputs, leaving one cue and one framecrc packet.
        std::fs::write(&vtt, b"WEBVTT\n\n00:00:03.500 --> 00:00:04.500\nsecond\n").expect("vtt");
        std::fs::write(&crc, b"#software: Lavf\n0, 3500, 3500, 0, 6, 0x1\n").expect("crc");
        let mut scan = StderrScan::new(Vec::new());
        scan.feed(b"[sist#0:1/subrip @ 0x1] [dec:srt @ 0x2] Error decoding subtitles: Invalid data found when processing input\n");
        scan.finish();
        assert!(scan.decoder_failed(1));
        assert!(
            !scan.decoder_failed(2),
            "an independent text stream is unaffected"
        );
        assert_eq!(
            judge_text_vtt(&vtt, &crc, [None, None], true).0,
            Verdict::Malformed
        );
        assert_eq!(
            judge_text_vtt(&vtt, &crc, [None, None], false).0,
            Verdict::Kept
        );
    }

    /// Build a source whose first cue is two seconds in. `source_start` moves
    /// the whole container timeline; the index and direct sidecar see the
    /// same source bytes under both timestamp regimes.
    async fn text_source_case(
        codec: &str,
        source_start: &str,
    ) -> (tempfile::TempDir, RideAlongPlan, Vec<u8>) {
        plurx_core::testfixtures::require_ffmpeg();
        let dir = crate::test_tempdir().expect("text fixture");
        let subtitle = dir.path().join(if codec == "ass" {
            "input.ass"
        } else {
            "input.srt"
        });
        if codec == "ass" {
            std::fs::write(&subtitle, "[Script Info]\nScriptType: v4.00+\nPlayResX: 64\nPlayResY: 64\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,18,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:02.00,0:00:04.00,Default,,0,0,0,,{\\pos(12,20)\\b1}Styled line\nDialogue: 0,0:00:05.00,0:00:06.00,Default,,0,0,0,,Second line\n").expect("ass");
        } else {
            std::fs::write(&subtitle, "1\n00:00:02,000 --> 00:00:04,000\nFirst cue\n\n2\n00:00:05,000 --> 00:00:06,000\nSecond cue\n\n").expect("srt");
        }
        let container = if codec == "mov_text" {
            "mp4"
        } else {
            "matroska"
        };
        let source = dir.path().join(if codec == "mov_text" {
            "source.mp4"
        } else {
            "source.mkv"
        });
        let mut command = std::process::Command::new(plurx_core::testfixtures::ffmpeg());
        command
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=64x64:r=5:d=8",
                "-i",
            ])
            .arg(&subtitle)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "1:s:0",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-c:s",
                codec,
                "-output_ts_offset",
                source_start,
                "-f",
                container,
            ])
            .arg(&source);
        plurx_core::testfixtures::run(&mut command);
        let mut file = synthetic_media_file(&source, 8_000).expect("media file");
        file.id =
            90_000 + i64::from(codec.as_bytes()[0]) + if source_start == "0" { 0 } else { 1_000 };
        let probe = std::process::Command::new(crate::ffmpeg::ffprobe_bin())
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .arg(&source)
            .output()
            .expect("probe");
        assert!(probe.status.success());
        let tracks = eligible_tracks_from_probe(&String::from_utf8_lossy(&probe.stdout));
        assert_eq!(tracks.len(), 1, "one eligible subtitle track");
        assert_eq!(tracks[0].ordinal, 0);
        let handle = std::fs::File::open(&source).expect("source");
        let gate = RideAlongGate::for_test(dir.path().join("store"));
        let plan = plan_tracks(&gate, file.id, &handle, &tracks)
            .await
            .expect("ride");
        let input = source.to_string_lossy().into_owned();
        let bin = plurx_core::testfixtures::ffmpeg();
        let bare = run_ffmpeg(
            &bin,
            &crate::fragindex::index_argv(&file, VIDEO, Some(&input), None),
            dir.path(),
        )
        .await
        .expect("bare pass");
        let riding = run_ffmpeg(
            &bin,
            &crate::fragindex::index_argv(&file, VIDEO, Some(&input), Some(&plan)),
            dir.path(),
        )
        .await
        .expect("riding pass");
        assert!(
            bare.status_ok && riding.status_ok,
            "{}",
            riding.stderr_text()
        );
        assert_eq!(
            bare.stdout, riding.stdout,
            "the index digest's bytes are unchanged"
        );
        let mut scan = plan.stderr_scan();
        scan.feed(&riding.stderr);
        scan.finish();
        let verdict_input = VerdictInput {
            tracks: plan.tracks.clone(),
            kinds: plan.kinds.clone(),
            stream_indexes: plan.stream_indexes.clone(),
            stage: plan.stage().to_owned(),
        };
        let outcomes = verdicts(&verdict_input, Some(&scan));
        assert_eq!(outcomes[0].verdict, Verdict::Kept, "{outcomes:?}");
        let direct = dir.path().join("direct.vtt");
        let mut command = std::process::Command::new(&bin);
        command
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
            .arg(&source)
            .args(["-map", "0:s:0", "-f", "webvtt"])
            .arg(&direct);
        plurx_core::testfixtures::run(&mut command);
        let vtt = std::fs::read(plan.vtt_path(0)).expect("tee VTT");
        assert_eq!(
            vtt,
            std::fs::read(direct).expect("direct VTT"),
            "tee and extract_vtt bytes"
        );
        assert_eq!(webvtt_cue_count(&vtt).expect("cues"), 2);
        (dir, plan, vtt)
    }

    macro_rules! text_case {
        ($name:ident, $codec:literal, $start:literal) => {
            #[tokio::test]
            async fn $name() {
                let (_dir, plan, _vtt) = text_source_case($codec, $start).await;
                if $codec == "ass" {
                    let mks = plan.mks_path(0);
                    assert!(mks.is_file(), "styled text also keeps Matroska");
                    assert!(std::fs::metadata(mks).expect("mks").len() > 0);
                }
            }
        };
    }

    text_case!(subrip_nonzero_first_cue_zero_source_start, "subrip", "0");
    text_case!(
        subrip_nonzero_first_cue_nonzero_source_start,
        "subrip",
        "7.5"
    );
    text_case!(ass_nonzero_first_cue_zero_source_start, "ass", "0");
    text_case!(ass_nonzero_first_cue_nonzero_source_start, "ass", "7.5");
    text_case!(
        mov_text_nonzero_first_cue_zero_source_start,
        "mov_text",
        "0"
    );
    text_case!(
        mov_text_nonzero_first_cue_nonzero_source_start,
        "mov_text",
        "7.5"
    );

    #[tokio::test]
    async fn ass_positioning_and_styling_burn_identically_from_stored_mks_and_source() {
        let filters = std::process::Command::new(plurx_core::testfixtures::ffmpeg())
            .args(["-hide_banner", "-filters"])
            .output()
            .expect("query FFmpeg filters");
        assert!(
            filters.status.success(),
            "querying FFmpeg filters: {}",
            String::from_utf8_lossy(&filters.stderr)
        );
        if !String::from_utf8_lossy(&filters.stdout)
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some("subtitles"))
        {
            eprintln!("skipping ASS burn comparison: this FFmpeg lacks the subtitles filter");
            return;
        }
        let (dir, plan, _) = text_source_case("ass", "0").await;
        let source = dir.path().join("source.mkv");
        let render = |subtitle: &Path| {
            let output = std::process::Command::new(plurx_core::testfixtures::ffmpeg())
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-nostdin",
                    "-ss",
                    "3",
                    "-i",
                ])
                .arg(&source)
                .args([
                    "-vf",
                    &format!("subtitles=filename='{}':si=0", subtitle.display()),
                    "-frames:v",
                    "1",
                    "-pix_fmt",
                    "rgb24",
                    "-f",
                    "rawvideo",
                    "pipe:1",
                ])
                .output()
                .expect("render");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!output.stdout.is_empty());
            output.stdout
        };
        assert_eq!(render(&source), render(&plan.mks_path(0)));
    }

    // -- the gate ------------------------------------------------------------

    #[test]
    fn advisory_readiness_reports_the_switch_self_test_and_local_cache() {
        let passed = SelfTest::Passed {
            elapsed_ms: 1,
            version: "8.0.1".into(),
        };
        let local = || Ok::<_, String>("ext4".to_owned());
        let roomy = || Ok::<_, String>("plenty".to_owned());
        let root = PathBuf::from("/cache/runtime/subtitle-source-v1");
        assert!(gate_from(true, &passed, local(), roomy(), root.clone()).is_ok());
        assert!(gate_from(false, &passed, local(), roomy(), root.clone()).is_err());
        let full = gate_from(
            true,
            &passed,
            local(),
            Err("0.2 GiB free".into()),
            root.clone(),
        )
        .expect_err("a full cache does not ride");
        assert!(full.contains("too full"), "{full}");
        for state in [
            SelfTest::NotRun,
            SelfTest::Running,
            SelfTest::Failed {
                reason: "boom".into(),
            },
        ] {
            let refused = gate_from(true, &state, local(), roomy(), root.clone())
                .expect_err("off until it passes");
            assert!(refused.contains("self-test"), "{refused}");
        }
        assert!(gate_from(true, &passed, Err("NFS (0x6969)".into()), roomy(), root).is_err());
    }

    /// Review finding 8: the margin is one gibibyte or two percent of the
    /// filesystem, whichever is larger.
    #[test]
    fn the_free_space_margin_is_the_larger_of_a_gibibyte_and_two_percent() {
        const GIB: u64 = 1 << 30;
        assert_eq!(free_space_margin(10 * GIB), GIB);
        assert_eq!(free_space_margin(1000 * GIB), 20 * GIB);
        assert!(free_space_verdict(2 * GIB, 10 * GIB).is_ok());
        assert!(free_space_verdict(GIB - 1, 10 * GIB).is_err());
        assert!(free_space_verdict(19 * GIB, 1000 * GIB).is_err());
        assert!(free_space_verdict(21 * GIB, 1000 * GIB).is_ok());
    }

    #[test]
    fn a_version_token_is_read_from_the_banner() {
        assert_eq!(
            version_token("ffmpeg version 8.0.1-3ubuntu2 Copyright (c) 2000-2025\n").as_deref(),
            Some("8.0.1-3ubuntu2")
        );
        assert_eq!(
            version_token("\nffprobe version n7.1 Copyright").as_deref(),
            Some("n7.1")
        );
        assert_eq!(version_token("no banner"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn network_and_fuse_filesystems_are_not_local() {
        for magic in [0x6969_u32, 0x517B, 0xFF53_4D42, 0xFE53_4D42, 0x6573_5546] {
            assert!(classify_filesystem_magic(magic).is_err(), "0x{magic:x}");
        }
        for magic in [
            0xEF53_u32,
            0x0102_1994,
            0x5846_5342,
            0x9123_683E,
            0x794C_7630,
        ] {
            assert!(classify_filesystem_magic(magic).is_ok(), "0x{magic:x}");
        }
        let here = crate::test_tempdir().expect("tempdir");
        assert!(
            local_filesystem(here.path()).is_ok(),
            "the test host's temp dir"
        );
        assert!(local_filesystem(&here.path().join("missing")).is_err());
    }

    // -- the latch -----------------------------------------------------------

    struct LatchCase {
        _dir: tempfile::TempDir,
        root: PathBuf,
        source_path: PathBuf,
        source: std::fs::File,
        stamp: SourceStamp,
    }

    fn latch_case() -> LatchCase {
        let dir = crate::test_tempdir().expect("latch");
        let path = dir.path().join("source.mkv");
        std::fs::write(&path, b"source").expect("source");
        LatchCase {
            root: dir.path().join(store::STORE_DIR),
            source_path: path.clone(),
            source: std::fs::File::open(&path).expect("open"),
            stamp: stamp_of(&path),
            _dir: dir,
        }
    }

    fn transient(ordinal: i64, attempts: u32) -> TrackEntry {
        TrackEntry {
            attempts,
            ..settled_entry(ordinal, Verdict::Transient)
        }
    }

    #[tokio::test]
    async fn the_latch_rides_until_every_probed_track_is_settled() {
        let case = latch_case();
        let gate = RideAlongGate::for_test(case.root.clone());
        let id = 70_001;

        let fresh = plan(&gate, id, &case.source, &[0, 2])
            .await
            .expect("no manifest rides");
        assert_eq!(fresh.tracks(), &[0, 2]);
        assert!(fresh.stage().is_dir());
        assert!(
            plan(&gate, id, &case.source, &[0, 2]).await.is_none(),
            "one ride per file at a time"
        );
        let stage = fresh.stage().to_owned();
        drop(fresh);
        assert!(!stage.exists(), "the guard removes an unused stage");

        let dir = store::file_dir(&case.root, id);
        let stored = crate::subtitle_source::testing::kept(&dir, 0, b"PG kept");
        write_manifest(
            &case.root,
            id,
            case.stamp,
            vec![stored.clone(), settled_entry(2, Verdict::Malformed)],
        );
        assert!(
            plan(&gate, id, &case.source, &[0, 2]).await.is_none(),
            "latched"
        );
        // Review finding 4: a `kept` entry whose file is gone — a power loss
        // before the rename was durable, a deletion — is not settled. That
        // track alone rides again, rather than missing for good.
        std::fs::remove_file(dir.join(stored.file.as_deref().expect("name"))).expect("lose it");
        let lost = plan(&gate, id, &case.source, &[0, 2])
            .await
            .expect("a kept track with no file rides again");
        assert_eq!(lost.tracks(), &[0]);
        drop(lost);

        // A transient track rides again, alone, until it has used its tries.
        for attempts in 1..TRANSIENT_ATTEMPTS {
            write_manifest(
                &case.root,
                id,
                case.stamp,
                vec![settled_entry(0, Verdict::Empty), transient(2, attempts)],
            );
            let retry = plan(&gate, id, &case.source, &[0, 2])
                .await
                .expect("transient retries");
            assert_eq!(retry.tracks(), &[2]);
            assert_eq!(retry.prior_attempts.get(&2), Some(&attempts));
            assert_eq!(retry.carried.len(), 1);
        }
        write_manifest(
            &case.root,
            id,
            case.stamp,
            vec![
                settled_entry(0, Verdict::Empty),
                transient(2, TRANSIENT_ATTEMPTS),
            ],
        );
        assert!(
            plan(&gate, id, &case.source, &[0, 2]).await.is_none(),
            "tries used"
        );

        // A probed track the manifest has no word for.
        let grown = plan(&gate, id, &case.source, &[0, 2, 3])
            .await
            .expect("new track");
        assert_eq!(grown.tracks(), &[3]);
        drop(grown);

        // Another source identity: everything again, attempts from zero.
        for moved in [
            SourceStamp {
                size: case.stamp.size + 1,
                ..case.stamp
            },
            SourceStamp {
                mtime: case.stamp.mtime + 1,
                ..case.stamp
            },
            SourceStamp {
                ino: case.stamp.ino.map(|ino| ino + 1),
                ..case.stamp
            },
        ] {
            write_manifest(
                &case.root,
                id,
                moved,
                vec![
                    settled_entry(0, Verdict::Kept),
                    settled_entry(2, Verdict::Kept),
                ],
            );
            let again = plan(&gate, id, &case.source, &[0, 2])
                .await
                .expect("a moved source rides");
            assert_eq!(again.tracks(), &[0, 2]);
            assert!(again.prior_attempts.values().all(|attempts| *attempts == 0));
        }
    }

    /// PR 3: a ride in flight is listed with its tracks and what its stage
    /// holds, and is gone once its plan is.
    #[tokio::test]
    async fn a_ride_in_flight_is_listed_until_its_plan_ends() {
        let case = latch_case();
        let gate = RideAlongGate::for_test(case.root.clone());
        let id = 70_020;
        let riding = plan(&gate, id, &case.source, &[0, 3]).await.expect("rides");
        std::fs::write(riding.stage().join("s0.sup"), [0_u8; 1234]).expect("written");
        let listed = active_rides()
            .into_iter()
            .find(|ride| ride.file_id == id)
            .expect("listed while it runs");
        assert_eq!(listed.tracks, 2);
        assert_eq!(listed.bytes_written, 1234);
        assert!(listed.started_at_ms > 0, "{listed:?}");
        assert!(
            (0..60_000).contains(&listed.running_ms),
            "running since the plan was made: {listed:?}"
        );
        // The settings read carries the same list, and says why the next
        // pass would not ride when the switch is off.
        let diagnostics = crate::subtitle_source::diagnostics(false, &case.root);
        assert!(diagnostics.riding.iter().any(|ride| ride.file_id == id));
        assert!(!diagnostics.gate.open);
        assert_eq!(
            diagnostics.gate.reason.as_deref(),
            Some("subtitles.stored_sources is off")
        );
        drop(riding);
        assert!(!active_rides().iter().any(|ride| ride.file_id == id));
    }

    /// A file with no eligible subtitle track has nothing to latch, and never rides.
    #[tokio::test]
    async fn a_file_with_no_eligible_subtitle_track_never_rides() {
        let case = latch_case();
        let gate = RideAlongGate::for_test(case.root.clone());
        for _ in 0..3 {
            assert!(plan(&gate, 70_002, &case.source, &[]).await.is_none());
        }
        assert!(
            !store::file_dir(&case.root, 70_002).exists(),
            "and writes nothing"
        );
    }

    /// The argv is built from the probe's ordinals alone: a track the
    /// scanner once saw and the probe does not is never mapped.
    #[tokio::test]
    async fn a_stale_scan_time_ordinal_is_never_in_the_argv() {
        let case = latch_case();
        let gate = RideAlongGate::for_test(case.root.clone());
        let probed =
            pgs_ordinals_from_probe(&probe(&[(0, "video", "h264"), (1, "subtitle", PGS_CODEC)]));
        let plan = plan(&gate, 70_003, &case.source, &probed)
            .await
            .expect("plan");
        let args = plan.args();
        assert!(args.windows(2).any(|pair| pair == ["-map", "0:s:0"]));
        assert!(!args.iter().any(|arg| arg == "0:s:1"), "{args:?}");
        assert!(
            !args.iter().any(|arg| arg.ends_with('?')),
            "hard maps only: {args:?}"
        );
        assert_eq!(
            args.last().map(|spec| spec.ends_with("|[f=null]-")),
            Some(true)
        );
        assert_eq!(&args[..2], ["-nostdin", "-y"]);
    }

    // -- the digest ----------------------------------------------------------

    /// The ride-along never reaches the cluster cache key: the digest of a
    /// file with PGS tracks is the digest of the same file without them, and
    /// the pass's argv is the hashed argv with the ride-along appended after
    /// `pipe:1`.
    #[test]
    fn the_pipeline_digest_is_identical_with_and_without_the_ride_along() {
        let dir = crate::test_tempdir().expect("digest");
        let source = dir.path().join("source.mkv");
        std::fs::write(&source, b"x").expect("source");
        let mut file = synthetic_media_file(&source, 60_000).expect("file");
        let bare = file.clone();
        file.subtitle_streams = [(1, "subrip"), (2, "ass"), (3, PGS_CODEC)]
            .into_iter()
            .map(|(index, codec)| plurx_core::domain::SubtitleStream {
                index,
                codec: codec.to_owned(),
                language: Some("eng".into()),
                title: None,
                default: index == 1,
                forced: false,
                hearing_impaired: false,
            })
            .collect();
        let engine = "e".repeat(64);
        let mut plan =
            RideAlongPlan::standalone(dir.path(), file.id, stamp_of(&source), vec![0, 1, 2])
                .expect("plan");
        plan.kinds.insert(0, ProbedKind::Text);
        plan.kinds.insert(1, ProbedKind::TextStyled);

        let hashed = plurx_core::transcode::copy_index_pipe_args(&file, VIDEO);
        let without = crate::fragindex::index_argv(&file, VIDEO, None, None);
        let with = crate::fragindex::index_argv(&file, VIDEO, None, Some(&plan));
        assert_eq!(without, hashed);
        assert_eq!(
            &with[..hashed.len()],
            hashed.as_slice(),
            "appended, never inserted"
        );
        assert_eq!(hashed.last().map(String::as_str), Some("pipe:1"));
        assert!(with.len() > hashed.len());
        assert!(
            !hashed
                .iter()
                .any(|arg| arg == "tee" || arg.starts_with("0:s:")),
            "{hashed:?}"
        );

        let digest = crate::fragment_index_cluster::pipeline_digest(&file, &engine, VIDEO);
        assert_eq!(
            digest,
            crate::fragment_index_cluster::pipeline_digest(&bare, &engine, VIDEO)
        );
        // And the pin can fail: hashing the riding argv is a different key.
        let mut riding = with.clone();
        if let Some(input) = riding
            .windows(2)
            .position(|pair| pair[0] == "-i")
            .map(|at| at + 1)
        {
            riding[input] = "{attested-source-fd}".to_owned();
        }
        assert_ne!(
            Some(digest),
            plurx_core::store::cluster_fragment_index_pipeline_digest(&engine, &riding)
        );
    }

    // -- real passes ----------------------------------------------------------

    fn fresh_cache() -> tempfile::TempDir {
        crate::test_tempdir().expect("runtime cache")
    }

    /// A pass, judged as the callers judge it: after the build has returned.
    struct Judged {
        outcome: IndexOutcome,
        ride_along: Option<RideAlongHarvest>,
        /// Each track judged again with no stderr failures at all — what the
        /// byte arithmetic says on its own.
        by_arithmetic: Vec<(i64, Verdict, Option<String>)>,
    }

    async fn pass(fixture: &Fixture, cache: &Path, gate: Option<&RideAlongGate>) -> Judged {
        let source = std::fs::File::open(&fixture.source).expect("held source");
        let IndexBuild {
            outcome,
            ride_along,
            ..
        } = crate::fragindex::build_from_attested_file_with_progress(
            &fixture.file,
            &source,
            "fixture-object-v1",
            VIDEO,
            cache,
            Duration::from_secs(60),
            |_: &crate::fragindex::PassProgress| {},
            gate,
        )
        .await;
        let mut by_arithmetic = Vec::new();
        if let Some(pending) = &ride_along {
            for ordinal in pending.plan.tracks() {
                let stage = pending.stage();
                let (verdict, _, reason) = judge(
                    &stage.join(format!("s{ordinal}.sup")),
                    &stage.join(format!("s{ordinal}.crc")),
                    [None, None],
                );
                by_arithmetic.push((*ordinal, verdict, reason));
            }
        }
        let ride_along = match ride_along {
            Some(pending) => Some(pending.judge().await),
            None => None,
        };
        Judged {
            outcome,
            ride_along,
            by_arithmetic,
        }
    }

    fn verdicts_of(build: &Judged) -> Vec<(i64, Verdict)> {
        build
            .ride_along
            .as_ref()
            .expect("a harvest")
            .outcomes()
            .iter()
            .map(|outcome| (outcome.ordinal, outcome.verdict))
            .collect()
    }

    /// (a) A corrupted first segment in one track, beside an intact track:
    /// the index is exactly the baseline's — byte for byte on the pipe, and
    /// row for row once indexed — the corrupted track is `malformed` and the
    /// intact one `kept`.
    #[tokio::test]
    async fn a_corrupted_track_is_malformed_and_the_index_is_untouched() {
        let mut fixture = fixture(71_001, &[Sub::Pgs, Sub::Pgs]);
        corrupt(&mut fixture, 3);
        let cache = fresh_cache();
        let root = store::store_root(cache.path());
        let gate = RideAlongGate::for_test(root.clone());

        let baseline = pass(&fixture, cache.path(), None).await;
        assert!(
            matches!(baseline.outcome, IndexOutcome::Built(_)),
            "{:?}",
            baseline.outcome
        );
        assert!(baseline.ride_along.is_none());
        let riding = pass(&fixture, cache.path(), Some(&gate)).await;
        assert_eq!(
            riding.outcome, baseline.outcome,
            "the index rows are the baseline's"
        );
        assert_eq!(
            verdicts_of(&riding),
            vec![(0, Verdict::Kept), (1, Verdict::Malformed)]
        );
        // Review finding 2: stderr reported the corrupted track's failure, and
        // `judge` stops there. The framecrc rule is the primary check, so it
        // must catch the track by itself: with no stderr failure at all, the
        // byte arithmetic alone says `malformed` (0 bytes in the `.sup`
        // against the 2344 packet bytes framecrc counted), and the intact
        // track is still `kept`.
        assert_eq!(riding.by_arithmetic[0].1, Verdict::Kept);
        assert_eq!(riding.by_arithmetic[1].1, Verdict::Malformed);
        assert!(
            riding.by_arithmetic[1]
                .2
                .as_deref()
                .is_some_and(|reason| reason.contains("framecrc counted")),
            "{:?}",
            riding.by_arithmetic[1]
        );

        // The pipe itself, byte for byte, with the same argv.
        let plan =
            RideAlongPlan::standalone(fixture.dir.path(), 1, stamp_of(&fixture.source), vec![0, 1])
                .expect("plan");
        let input = fixture.source.to_string_lossy().into_owned();
        let bin = plurx_core::testfixtures::ffmpeg();
        let without = run_ffmpeg(
            &bin,
            &crate::fragindex::index_argv(&fixture.file, VIDEO, Some(&input), None),
            cache.path(),
        )
        .await
        .expect("baseline");
        let with = run_ffmpeg(
            &bin,
            &crate::fragindex::index_argv(&fixture.file, VIDEO, Some(&input), Some(&plan)),
            cache.path(),
        )
        .await
        .expect("riding");
        assert!(without.status_ok && with.status_ok);
        assert!(!without.stdout.is_empty());
        assert!(
            without.stdout == with.stdout,
            "the index pipe is byte-identical"
        );

        // Published, the intact track is served and the corrupted one is not.
        let manifest = riding
            .ride_along
            .expect("harvest")
            .publish()
            .await
            .expect("publish");
        assert_eq!(manifest.ordinals, vec![0, 1]);
        let access = store::StoreAccess::new(root, true);
        let live = stamp_of(&fixture.source);
        let Lookup::Kept(kept) = store::lookup(
            &access,
            Consumer::Overlay,
            &fixture.file,
            0,
            Live::Stamp(live),
        )
        .await
        else {
            panic!("the intact track is kept");
        };
        let mut stored = Vec::new();
        std::io::Read::read_to_end(
            &mut store::open_verified(&kept).await.expect("verified"),
            &mut stored,
        )
        .expect("read");
        // Byte-identical to a whole-source extraction of the same track, the
        // bytes the overlay's own demux writes (§6.2 fact 9).
        let extracted = fixture.dir.path().join("extracted.sup");
        let mut extract: Vec<String> =
            ["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"]
                .map(str::to_owned)
                .to_vec();
        extract.push(fixture.source.to_string_lossy().into_owned());
        extract.extend(["-map", "0:s:0", "-c:s", "copy", "-f", "sup"].map(str::to_owned));
        extract.push(extracted.to_string_lossy().into_owned());
        run_ffmpeg_to_completion(&extract);
        assert_eq!(
            stored,
            std::fs::read(&extracted).expect("extracted"),
            "byte-identical to the demux"
        );
        assert!(matches!(
            store::lookup(
                &access,
                Consumer::Overlay,
                &fixture.file,
                1,
                Live::Stamp(live)
            )
            .await,
            Lookup::Miss(_)
        ));
    }

    /// (b) Several segments per packet: kept, and the arithmetic that keeps
    /// it is the ten-byte rule over walked segments.
    #[tokio::test]
    async fn a_multi_segment_packet_track_is_kept_by_the_ten_byte_rule() {
        let fixture = fixture(71_002, &[Sub::Pgs]);
        let cache = fresh_cache();
        let riding = pass(
            &fixture,
            cache.path(),
            Some(&RideAlongGate::for_test(store::store_root(cache.path()))),
        )
        .await;
        assert_eq!(verdicts_of(&riding), vec![(0, Verdict::Kept)]);
        let stage = riding
            .ride_along
            .as_ref()
            .expect("harvest")
            .plan
            .stage()
            .to_owned();
        assert!(riding
            .by_arithmetic
            .iter()
            .all(|row| row.1 == Verdict::Kept));
        let sup = std::fs::read(stage.join("s0.sup")).expect("sup");
        let totals = crc_totals(&std::fs::read_to_string(stage.join("s0.crc")).expect("crc"))
            .expect("totals");
        let walk = walk_sup(std::fs::File::open(stage.join("s0.sup")).expect("sup")).expect("walk");
        assert!(
            walk.segments > totals.packets,
            "more than one segment per packet"
        );
        let size = sup.len() as u64;
        assert_eq!(size - 10 * walk.segments, totals.bytes);
        assert_ne!(size - 13 * walk.segments, totals.bytes);
        assert_ne!(size - 10 * totals.packets, totals.bytes);
    }

    /// (c) through a real pass: a text track shifts the PGS ordinal, and a
    /// scan-time track list that claims more PGS tracks than the file has is
    /// never read — the index builds and only the probed track is kept.
    #[tokio::test]
    async fn the_real_pass_maps_only_what_the_probe_found() {
        let mut fixture = fixture(71_003, &[Sub::Srt, Sub::Pgs]);
        fixture.file.subtitle_streams = [(1, "subrip"), (2, PGS_CODEC), (3, PGS_CODEC)]
            .into_iter()
            .map(|(index, codec)| plurx_core::domain::SubtitleStream {
                index,
                codec: codec.to_owned(),
                language: None,
                title: None,
                default: false,
                forced: false,
                hearing_impaired: false,
            })
            .collect();
        let cache = fresh_cache();
        let riding = pass(
            &fixture,
            cache.path(),
            Some(&RideAlongGate::for_test(store::store_root(cache.path()))),
        )
        .await;
        assert!(
            matches!(riding.outcome, IndexOutcome::Built(_)),
            "{:?}",
            riding.outcome
        );
        assert_eq!(
            verdicts_of(&riding),
            vec![(0, Verdict::Kept), (1, Verdict::Kept)]
        );
    }

    /// (d) A real PGS track with no cues is `empty`.
    #[tokio::test]
    async fn a_track_with_no_cues_is_empty() {
        let fixture = fixture(71_004, &[Sub::EmptyPgs, Sub::Pgs]);
        let cache = fresh_cache();
        let riding = pass(
            &fixture,
            cache.path(),
            Some(&RideAlongGate::for_test(store::store_root(cache.path()))),
        )
        .await;
        assert_eq!(
            verdicts_of(&riding),
            vec![(0, Verdict::Empty), (1, Verdict::Kept)]
        );
        let manifest = riding
            .ride_along
            .expect("harvest")
            .publish()
            .await
            .expect("publish");
        assert!(manifest.latched());
        assert_eq!(
            manifest.track(0).map(|track| track.file.is_none()),
            Some(true)
        );
    }

    /// (e) A cache path with a backslash — and every other character the
    /// tee tokeniser treats specially — still lands every slave in the stage.
    #[tokio::test]
    async fn a_stage_path_with_a_backslash_is_written_where_it_says() {
        let fixture = fixture(71_005, &[Sub::Pgs]);
        let cache = fresh_cache();
        let odd = cache.path().join("back\\slash |pipe 'quote' [bracket]");
        let root = store::store_root(&odd);
        let riding = pass(
            &fixture,
            cache.path(),
            Some(&RideAlongGate::for_test(root.clone())),
        )
        .await;
        assert_eq!(verdicts_of(&riding), vec![(0, Verdict::Kept)]);
        riding
            .ride_along
            .expect("harvest")
            .publish()
            .await
            .expect("publish");
        let dir = store::file_dir(&root, fixture.file.id);
        assert!(std::fs::read_dir(&dir)
            .expect("published")
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".sup")));
        // Nothing escaped the stage into a sibling the unescaped name names.
        assert!(!cache
            .path()
            .join("backslash |pipe 'quote' [bracket]")
            .exists());
    }

    // -- publish ---------------------------------------------------------------

    fn harvest_of(root: &Path, file_id: i64, stamp: SourceStamp, bytes: &[u8]) -> RideAlongHarvest {
        let plan = RideAlongPlan::standalone(root, file_id, stamp, vec![0]).expect("plan");
        std::fs::write(plan.sup_path(0), bytes).expect("staged");
        RideAlongHarvest {
            outcomes: vec![TrackOutcome {
                ordinal: 0,
                verdict: Verdict::Kept,
                sha256: Some(hex::encode(Sha256::digest(bytes))),
                written: bytes.len() as u64,
                reason: None,
                representations: vec![RepresentationOutcome {
                    format: RepresentationFormat::Sup,
                    verdict: Verdict::Kept,
                    sha256: Some(hex::encode(Sha256::digest(bytes))),
                    bytes: bytes.len() as u64,
                    reason: None,
                }],
            }],
            plan,
        }
    }

    #[tokio::test]
    async fn publish_places_then_swaps_then_deletes_then_marks() {
        let case = latch_case();
        let id = 70_010;
        let first = harvest_of(&case.root, id, case.stamp, b"PG first");
        let first_stage = first.plan.stage().to_owned();
        let one = first.publish().await.expect("first");
        assert!(!first_stage.exists(), "the stage goes with the harvest");
        let dir = store::file_dir(&case.root, id);
        let first_name = one
            .track(0)
            .and_then(|track| track.file.clone())
            .expect("named");
        assert!(dir.join(&first_name).is_file());
        assert!(dir.join(".access").is_file(), "(4) the first access marker");
        assert_eq!(one.track(0).map(|track| track.attempts), Some(1));

        let two = harvest_of(&case.root, id, case.stamp, b"PG second")
            .publish()
            .await
            .expect("second");
        let second_name = two
            .track(0)
            .and_then(|track| track.file.clone())
            .expect("named");
        assert_ne!(first_name, second_name, "content-named");
        assert!(dir.join(&second_name).is_file(), "(1) placed");
        assert!(
            !dir.join(&first_name).exists(),
            "(3) the replaced track is deleted"
        );
        let on_disk = store::read_manifest(&dir).await.expect("(2) manifest");
        assert_eq!(on_disk, two);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[tokio::test]
    async fn a_publication_row_is_written_only_after_the_manifest_rename() {
        let case = latch_case();
        let id = 70_012;
        let catalog = plurx_core::store::SqliteStore::open_in_memory().expect("catalog");
        let manifest = harvest_of(&case.root, id, case.stamp, b"PG source")
            .publish_with_cluster(&catalog, "nuc4", &"a".repeat(64), None)
            .await
            .expect("publish");
        let rows = catalog
            .list_subtitle_source_publications(id, case.stamp.size as i64, case.stamp.mtime)
            .await
            .expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].origin, "extracted");
        let dir = store::file_dir(&case.root, id);
        assert_eq!(store::read_manifest(&dir).await, Some(manifest.clone()));
        assert!(dir
            .join(
                manifest
                    .track(0)
                    .and_then(|entry| entry.file.as_deref())
                    .expect("name")
            )
            .is_file());
    }

    #[tokio::test]
    async fn cancellation_during_publish_removes_new_cluster_rows_and_local_entry() {
        let case = latch_case();
        let id = 70_013;
        let catalog = plurx_core::store::SqliteStore::open_in_memory().expect("catalog");
        let lost = CancellationToken::new();
        let result = harvest_of(&case.root, id, case.stamp, b"cancelled source")
            .publish_internal(
                Some((&catalog, "nuc4", &"a".repeat(64))),
                Some(&lost),
                None,
                || async { lost.cancel() },
            )
            .await;
        assert!(result.is_err(), "a cancelled publish cannot commit");
        assert!(lost.is_cancelled());
        assert!(
            catalog
                .list_subtitle_source_publications(id, case.stamp.size as i64, case.stamp.mtime)
                .await
                .expect("rows")
                .is_empty(),
            "the row written at the controlled seam was compensated"
        );
        let dir = store::file_dir(&case.root, id);
        assert!(store::read_manifest(&dir).await.is_none());
        assert!(
            !dir.join(
                store::representation_file_name(
                    0,
                    RepresentationFormat::Sup,
                    &hex::encode(Sha256::digest(b"cancelled source"))
                )
                .expect("name")
            )
            .exists(),
            "the unpublished content file is removed"
        );
    }

    #[tokio::test]
    async fn admin_cancellation_during_publish_compensates_rows_and_manifest() {
        use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
        use plurx_core::store::{LibraryStore, MediaStore, NewAnalysisRequest};

        let case = latch_case();
        let catalog = plurx_core::store::SqliteStore::open_in_memory().expect("catalog");
        let library = catalog
            .create_library(&NewLibrary {
                name: "Source".to_owned(),
                kind: LibraryKind::Movies,
                paths: vec![case.root.clone()],
                anime: false,
            })
            .await
            .expect("library");
        let item = catalog
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Source".to_owned(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let id = catalog
            .upsert_file(
                item,
                case.source_path.to_str().expect("source path"),
                case.stamp.size as i64,
                case.stamp.mtime,
                &ProbeResult::default(),
            )
            .await
            .expect("file");
        let now = unix_ms();
        let request = catalog
            .enqueue_analysis_request(&NewAnalysisRequest {
                request_id: "cancel-publish".to_owned(),
                file_id: id,
                source_size: case.stamp.size as i64,
                source_mtime: case.stamp.mtime,
                component: "subtitle_source".to_owned(),
                pipeline_version: "test-pipeline".to_owned(),
                video_identity: String::new(),
                requested_generation: "cancel-publish".to_owned(),
                priority: "foreground".to_owned(),
                trigger: "playback".to_owned(),
                force_rebuild: false,
                target_node_id: String::new(),
                not_before_ms: now,
                created_at_ms: now,
            })
            .await
            .expect("enqueue");
        let claimed = catalog
            .claim_analysis_request("nuc4", now, now + 60_000)
            .await
            .expect("claim")
            .expect("running");
        assert_eq!(claimed.request_id, request.request_id);
        let lost = CancellationToken::new();
        assert!(!lost.is_cancelled(), "admin cancel is a separate fence");
        let result = harvest_of(&case.root, id, case.stamp, b"admin cancelled")
            .publish_internal(
                Some((&catalog, "nuc4", &"a".repeat(64))),
                Some(&lost),
                Some(&claimed),
                || async {
                    catalog
                        .cancel_analysis_request_admin(&claimed.request_id, now + 1)
                        .await
                        .expect("cancel");
                },
            )
            .await;
        assert!(result.is_err(), "the cancelled fence cannot publish");
        assert!(store::read_manifest(&store::file_dir(&case.root, id))
            .await
            .is_none());
        assert!(catalog
            .list_subtitle_source_publications(id, case.stamp.size as i64, case.stamp.mtime)
            .await
            .expect("rows")
            .is_empty());
    }

    /// Readers racing a stream of publishes see the old manifest or the new
    /// one, and a manifest that stays on disk across a check never names a
    /// file that is not there. A lookup either verifies one of the published
    /// contents or misses; it never fails.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_reader_mid_publish_sees_the_old_or_the_new_manifest() {
        let case = latch_case();
        let id = 70_011;
        let dir = store::file_dir(&case.root, id);
        let contents: [&[u8]; 2] = [b"PG alpha", b"PG bravo bravo"];
        harvest_of(&case.root, id, case.stamp, contents[0])
            .publish()
            .await
            .expect("seed");
        let file =
            synthetic_media_file(&case.root.join("..").join("source.mkv"), 1_000).expect("file");
        let file = plurx_core::domain::MediaFile { id, ..file };
        let access = store::StoreAccess::new(case.root.clone(), true);
        let done = std::sync::atomic::AtomicBool::new(false);

        let publisher = async {
            for round in 0..40 {
                harvest_of(&case.root, id, case.stamp, contents[round % 2])
                    .publish()
                    .await
                    .expect("publish");
            }
            done.store(true, Ordering::SeqCst);
        };
        let reader = async {
            let (mut hits, mut checks) = (0, 0);
            while !done.load(Ordering::SeqCst) {
                let before =
                    std::fs::read(dir.join(MANIFEST_NAME)).expect("a manifest is always there");
                let manifest = store::parse_manifest(&before).expect("never a torn manifest");
                let name = manifest
                    .track(0)
                    .and_then(|track| track.file.clone())
                    .expect("named");
                let present = dir.join(&name).is_file();
                let after = std::fs::read(dir.join(MANIFEST_NAME)).expect("manifest");
                if before == after {
                    assert!(
                        present,
                        "the manifest on disk named {name}, which was not there"
                    );
                    checks += 1;
                }
                match store::lookup(
                    &access,
                    Consumer::Overlay,
                    &file,
                    0,
                    Live::Stamp(case.stamp),
                )
                .await
                {
                    Lookup::Kept(kept) => {
                        if let Some(mut opened) = store::open_verified(&kept).await {
                            let mut bytes = Vec::new();
                            std::io::Read::read_to_end(&mut opened, &mut bytes).expect("read");
                            assert!(contents.contains(&bytes.as_slice()), "{bytes:?}");
                            hits += 1;
                        }
                    }
                    other => panic!("a current manifest is found: {other:?}"),
                }
                tokio::task::yield_now().await;
            }
            (hits, checks)
        };
        let ((), (hits, checks)) = tokio::join!(publisher, reader);
        assert!(hits > 0 && checks > 0, "hits={hits} checks={checks}");
    }

    // -- the self-test --------------------------------------------------------

    #[tokio::test]
    async fn the_self_test_passes_on_this_ffmpeg() {
        plurx_core::testfixtures::require_ffmpeg();
        let cache = fresh_cache();
        let root = store::store_root(cache.path());
        let report = run_self_test(
            &plurx_core::testfixtures::ffmpeg(),
            &plurx_core::testfixtures::ffprobe(),
            &root,
            cache.path(),
        )
        .await
        .expect("the self-test passes");
        assert!(report.elapsed < SELF_TEST_BUDGET);
        assert!(!report.version.is_empty());
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .expect("root")
            .filter_map(Result::ok)
            .collect();
        assert!(leftovers.is_empty(), "the self-test cleans up after itself");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_self_test_fails_on_an_ffmpeg_that_does_not_behave() {
        use std::os::unix::fs::PermissionsExt;

        let cache = fresh_cache();
        let root = store::store_root(cache.path());
        let real = plurx_core::testfixtures::ffmpeg();
        let prober = plurx_core::testfixtures::ffprobe();
        let script = |name: &str, body: String| {
            let path = cache.path().join(name);
            std::fs::write(&path, body).expect("fake");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            path.to_string_lossy().into_owned()
        };
        // Answers `-version` as the real build does, then exits 0 and does
        // nothing: it "muxes" no source.
        let silent = script(
            "silent-ffmpeg",
            format!("#!/bin/sh\ncase \"$1\" in -version) exec {real} \"$@\" ;; esac\nexit 0\n"),
        );
        let reason = run_self_test(&silent, &prober, &root, cache.path())
            .await
            .expect_err("nothing was produced");
        assert!(reason.contains("synthetic source"), "{reason}");
        // Real ffmpeg for the source, a build whose tee never writes a slave
        // for the second track: the verdicts are not the expected pair.
        let partial = script(
            "partial-ffmpeg",
            format!(
                "#!/bin/sh\ncase \"$*\" in *\"-f tee \"*) exec {real} \"$@\" 2>/dev/null ;; *) exec {real} \"$@\" ;; esac\n"
            ),
        );
        let reason = run_self_test(&partial, &prober, &root, cache.path())
            .await
            .expect_err("the stderr line is not recognised when it never arrives");
        assert!(reason.contains("not recognised"), "{reason}");
        assert!(
            run_self_test("/nonexistent/ffmpeg", &prober, &root, cache.path())
                .await
                .is_err()
        );
        // Review finding 1: an ffprobe of another build is refused before
        // anything runs, and the reason names both versions.
        let other_prober = script(
            "old-ffprobe",
            "#!/bin/sh\necho \"ffprobe version 4.4.2-0ubuntu0 Copyright (c) 2007-2021\"\n"
                .to_owned(),
        );
        let reason = run_self_test(&real, &other_prober, &root, cache.path())
            .await
            .expect_err("mismatched builds");
        assert!(reason.contains("4.4.2-0ubuntu0"), "{reason}");
    }

    /// The operator may enable the producer before its advisory self-test;
    /// only the saved manual switch controls entry to the pass.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn the_ride_along_enable_is_not_gated_by_advisory_readiness() {
        plurx_core::testfixtures::require_ffmpeg();
        let cache = fresh_cache();
        let catalog = Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("catalog"));
        set_self_test_state(SelfTest::NotRun);
        assert!(RideAlongGate::open(catalog.clone(), cache.path())
            .await
            .is_some());
        set_self_test_state(SelfTest::Failed {
            reason: "test".into(),
        });
        assert!(RideAlongGate::open(catalog.clone(), cache.path())
            .await
            .is_some());
        startup_self_test(
            cache.path().to_owned(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
        assert!(
            matches!(self_test_state(), SelfTest::Passed { .. }),
            "{:?}",
            self_test_state()
        );
        assert!(RideAlongGate::open(catalog.clone(), cache.path())
            .await
            .is_some());
        // One switch: off stops the producer too.
        plurx_core::store::SettingsStore::put_setting(
            catalog.as_ref(),
            plurx_core::store::keys::SUBTITLE_STORED_SOURCES,
            "0",
        )
        .await
        .expect("switch off");
        assert!(RideAlongGate::open(catalog, cache.path()).await.is_none());
    }

    #[tokio::test]
    async fn a_second_nodes_index_pass_skips_cluster_extracted_coverage_for_the_same_source() {
        use plurx_core::domain::{
            ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult, SubtitleStream,
        };
        use plurx_core::store::SqliteStore;

        let temp = crate::test_tempdir().expect("catalogue and source");
        let source_path = temp.path().join("source.mkv");
        std::fs::write(&source_path, b"portable-source-digest-fixture").expect("source");
        let catalog: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("catalogue"));
        let library = catalog
            .create_library(&NewLibrary {
                name: "Other node".to_owned(),
                kind: LibraryKind::Movies,
                paths: vec![temp.path().to_owned()],
                anime: false,
            })
            .await
            .expect("library");
        let item = catalog
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Source".to_owned(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let metadata = std::fs::metadata(&source_path).expect("metadata");
        let stamp = crate::fragment_index_cluster::source_stamp(&metadata);
        let file_id = catalog
            .upsert_file(
                item,
                source_path.to_str().expect("source path"),
                stamp.size as i64,
                stamp.mtime,
                &ProbeResult {
                    container: Some("mkv".to_owned()),
                    subtitle_streams: vec![SubtitleStream {
                        index: 0,
                        codec: "subrip".to_owned(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )
            .await
            .expect("catalogued source");
        for key in [
            plurx_core::store::keys::SUBTITLE_STORED_SOURCES,
            plurx_core::store::keys::SUBTITLE_CLUSTER_SOURCES,
        ] {
            catalog.put_setting(key, "1").await.expect("switch");
        }
        let file = catalog
            .get_file(file_id)
            .await
            .expect("lookup")
            .expect("file");
        let digest = crate::fragment_index_cluster::attest_source("", &file, None, &|_| {})
            .await
            .expect("attest source")
            .observation
            .source_sha256;
        let mut row = SubtitleSourcePublication {
            file_id,
            source_size: stamp.size as i64,
            source_mtime: stamp.mtime,
            source_attestation: digest,
            node_id: "node-a".to_owned(),
            ordinal: 0,
            kind: "text".to_owned(),
            format: "webvtt".to_owned(),
            verdict: "empty".to_owned(),
            attempts: 1,
            origin: "extracted".to_owned(),
            sha256: String::new(),
            bytes: 0,
            published_at_ms: 1,
        };
        catalog
            .upsert_subtitle_source_publication(&row)
            .await
            .expect("node A publication");
        let gate = RideAlongGate::open(catalog.clone(), temp.path())
            .await
            .expect("ride enabled");
        let held = std::fs::File::open(&source_path).expect("held source");
        let tracks = [ProbedTrack {
            ordinal: 0,
            kind: ProbedKind::Text,
            stream_index: 1,
        }];
        assert!(plan_tracks(&gate, file_id, &held, &tracks).await.is_none());

        row.source_attestation = "f".repeat(64);
        catalog
            .upsert_subtitle_source_publication(&row)
            .await
            .expect("stale publication");
        assert!(plan_tracks(&gate, file_id, &held, &tracks).await.is_some());
    }
}
