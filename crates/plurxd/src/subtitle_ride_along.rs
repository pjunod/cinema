//! The subtitle-source store's producer: the fragment-index pass keeps every
//! PGS track it is already reading.
//!
//! Design: `docs/clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md` §6.3. The
//! index pass is one `ffmpeg` child that demuxes the whole file from byte zero
//! and discards the subtitle packets with `-sn`. This module adds one more
//! output to that child — a `tee` of a `sup` slave and a `framecrc` companion
//! per PGS track, and a mandatory `null` sentinel — and afterwards decides,
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
//! - **Nothing rides unless** the Developer switch is on, the startup
//!   self-test proved this `ffmpeg` behaves as the design measured, and the
//!   cache is on a local filesystem.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::subtitle_source::{
    self as store, Consumer, Manifest, SourceStamp, TrackEntry, Verdict, MANIFEST_NAME,
    MANIFEST_VERSION, MAX_TRACK_BYTES,
};

/// The codec name the probe reports for a PGS track.
pub(crate) const PGS_CODEC: &str = "hdmv_pgs_subtitle";
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
pub(crate) fn pgs_ordinals_from_probe(raw: &str) -> Vec<i64> {
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
    let mut pgs = Vec::new();
    for (_, stream) in indexed {
        if stream.get("codec_type").and_then(serde_json::Value::as_str) != Some("subtitle") {
            continue;
        }
        if stream.get("codec_name").and_then(serde_json::Value::as_str) == Some(PGS_CODEC) {
            pgs.push(ordinal);
        }
        ordinal += 1;
    }
    pgs
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
#[derive(Clone, Debug)]
pub(crate) struct RideAlongGate {
    root: PathBuf,
}

impl RideAlongGate {
    /// Read the switch, the self-test and the filesystem, now.
    ///
    /// `None` is the behaviour that shipped before the producer existed. It
    /// is asked once per pass, so turning the switch off stops the next pass
    /// from riding without a restart.
    pub(crate) async fn open(
        store: &dyn plurx_core::store::Store,
        runtime_cache: &Path,
    ) -> Option<Self> {
        let switch_on = crate::subtitle_source::enabled(store).await;
        let root = store::store_root(runtime_cache);
        if switch_on && gate_overridden() {
            return Some(Self { root });
        }
        let self_test = self_test_state();
        // Checked only when the cheaper conditions hold, and after the root
        // exists: the checks are of the directory the stage will be made in.
        let (filesystem, space) = if switch_on && matches!(self_test, SelfTest::Passed { .. }) {
            match tokio::fs::create_dir_all(&root).await {
                Ok(()) => (local_filesystem(&root), free_space(&root)),
                Err(error) => {
                    let reason = format!("creating {}: {error}", root.display());
                    (Err(reason.clone()), Err(reason))
                }
            }
        } else {
            (Err("not checked".to_owned()), Err("not checked".to_owned()))
        };
        match gate_from(switch_on, &self_test, filesystem, space, root) {
            Ok(gate) => Some(gate),
            Err(reason) => {
                tracing::debug!(%reason, "the PGS ride-along is off for this pass");
                None
            }
        }
    }

    /// A gate for a test that exercises the pass itself.
    #[cfg(test)]
    pub(crate) fn for_test(root: PathBuf) -> Self {
        Self { root }
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
            return Err("the startup self-test has not passed yet".to_owned())
        }
        SelfTest::Failed { reason } => {
            return Err(format!("the startup self-test failed: {reason}"))
        }
    }
    filesystem.map_err(|reason| format!("the cache is not usable for the ride-along: {reason}"))?;
    space.map_err(|reason| format!("the cache is too full for the ride-along: {reason}"))?;
    Ok(RideAlongGate { root })
}

/// Whether `path` is on a local filesystem, by `statfs` type. `Ok` names the
/// filesystem; `Err` says why the ride-along must not use it.
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
    /// PGS tracks this pass is keeping.
    pub tracks: usize,
    /// Bytes written into the stage so far — the `.sup` and `.crc` files.
    pub bytes_written: u64,
    pub started_at_ms: i64,
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
    rides
        .into_iter()
        .map(|(file_id, ride)| ActiveRide {
            file_id,
            tracks: ride.tracks,
            bytes_written: stage_bytes(&ride.stage),
            started_at_ms: ride.started_at_ms,
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
    /// Every PGS ordinal the probe found, in order — the new manifest's
    /// `ordinals`.
    probed: Vec<i64>,
    /// The ordinals this pass extracts; track `i` of the tee is `tracks[i]`.
    tracks: Vec<i64>,
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
/// and every probed ordinal is settled. A file with no PGS track has nothing
/// to latch: no plan, no argv, ever.
pub(crate) async fn plan(
    gate: &RideAlongGate,
    file_id: i64,
    source: &std::fs::File,
    probed: &[i64],
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
    let mut tracks = Vec::new();
    let mut carried = Vec::new();
    let mut prior_attempts = BTreeMap::new();
    for ordinal in probed {
        match previous
            .as_ref()
            .and_then(|manifest| manifest.track(*ordinal))
        {
            // A `kept` entry is settled only while its file is there: after a
            // power loss or a deletion the manifest can name a track the disk
            // no longer has, and trusting it would miss for good.
            Some(entry) if entry.settled() && kept_file_present(&dir, entry).await => {
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
            tracing::warn!(file_id, %error, "creating a PGS ride-along stage; this pass does not ride");
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
        probed: probed.to_vec(),
        tracks,
        carried,
        prior_attempts,
        stage,
        _claim: Some(claim),
    })
}

/// A `kept` entry's stored file exists as a regular file; any other verdict
/// has no file to check.
async fn kept_file_present(dir: &Path, entry: &TrackEntry) -> bool {
    if entry.verdict != Verdict::Kept {
        return true;
    }
    match entry.file.as_deref() {
        Some(name) => tokio::fs::metadata(dir.join(name))
            .await
            .is_ok_and(|metadata| metadata.is_file()),
        None => false,
    }
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

    /// The slaves in tee order: track `i`'s `sup` is `#2i`, its `framecrc`
    /// companion `#2i+1`. The sentinel, last, is not listed.
    fn slave_paths(&self) -> Vec<String> {
        self.tracks
            .iter()
            .flat_map(|ordinal| [self.sup_path(*ordinal), self.crc_path(*ordinal)])
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }

    /// The argv appended to the index pass, after `pipe:1`.
    ///
    /// `-nostdin` and `-y` are global options and change nothing about the
    /// index output (the self-test proves that on every start). The maps are
    /// hard: an ordinal the file does not have kills the pass rather than
    /// silently shifting another track into its place.
    pub(crate) fn args(&self) -> Vec<String> {
        let mut args = vec!["-nostdin".to_owned(), "-y".to_owned()];
        for ordinal in &self.tracks {
            args.push("-map".to_owned());
            args.push(format!("0:s:{ordinal}"));
        }
        args.extend(["-c:s", "copy", "-f", "tee"].map(str::to_owned));
        args.push(self.tee_spec());
        args
    }

    fn tee_spec(&self) -> String {
        let mut slaves = Vec::with_capacity(self.tracks.len() * 2 + 1);
        for (position, ordinal) in self.tracks.iter().enumerate() {
            slaves.push(format!(
                "[select={position}:f=sup:onfail=ignore]{}",
                tee_escape(&self.sup_path(*ordinal).to_string_lossy())
            ));
            slaves.push(format!(
                "[select={position}:f=framecrc:onfail=ignore]{}",
                tee_escape(&self.crc_path(*ordinal).to_string_lossy())
            ));
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
    stage: PathBuf,
}

/// Judge every track of a finished pass. Blocking: reads and parses.
///
/// `scan` is `None` when the stderr reader did not finish, so nothing can be
/// said about slave failures — every track is then `transient`.
fn verdicts(input: &VerdictInput, scan: Option<&StderrScan>) -> Vec<TrackOutcome> {
    input
        .tracks
        .iter()
        .enumerate()
        .map(|(position, ordinal)| {
            let sup = input.stage.join(format!("s{ordinal}.sup"));
            let crc = input.stage.join(format!("s{ordinal}.crc"));
            let written = [&sup, &crc]
                .iter()
                .filter_map(|path| std::fs::metadata(path).ok())
                .map(|metadata| metadata.len())
                .sum();
            let (verdict, sha256, reason) = match scan {
                None => (
                    Verdict::Transient,
                    None,
                    Some("the stderr scan did not finish".to_owned()),
                ),
                Some(scan) => judge(
                    &sup,
                    &crc,
                    [scan.failure(2 * position), scan.failure(2 * position + 1)],
                ),
            };
            TrackOutcome {
                ordinal: *ordinal,
                verdict,
                sha256,
                written,
                reason,
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
            ))
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

impl RideAlongHarvest {
    /// Judge a reaped pass whose index was built. `scan` is `None` when the
    /// stderr reader did not finish.
    pub(crate) async fn take(plan: RideAlongPlan, scan: Option<StderrScan>) -> Self {
        let input = VerdictInput {
            tracks: plan.tracks.clone(),
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
            if let Some(reason) = &outcome.reason {
                tracing::info!(
                    file_id = plan.file_id,
                    ordinal = outcome.ordinal,
                    verdict = ?outcome.verdict,
                    %reason,
                    "PGS ride-along track not kept"
                );
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
                let named = outcome
                    .sha256
                    .as_deref()
                    .filter(|_| outcome.verdict == Verdict::Kept)
                    .and_then(|sha256| {
                        store::sup_file_name(*ordinal, sha256).map(|name| (name, sha256.to_owned()))
                    });
                Some(TrackEntry {
                    ordinal: *ordinal,
                    verdict: match (&named, outcome.verdict) {
                        (None, Verdict::Kept) => Verdict::Malformed,
                        (_, verdict) => verdict,
                    },
                    attempts,
                    file: named.as_ref().map(|(name, _)| name.clone()),
                    sha256: named.map(|(_, sha256)| sha256),
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
    pub(crate) async fn publish(self) -> Result<Manifest, String> {
        let dir = store::file_dir(&self.plan.root, self.plan.file_id);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|error| format!("creating {}: {error}", dir.display()))?;
        let mut manifest = self.manifest();

        // (1)
        let mut placed = Vec::new();
        for track in &mut manifest.tracks {
            let Some(name) = track.file.clone() else {
                continue;
            };
            if self
                .plan
                .carried
                .iter()
                .any(|carried| carried.ordinal == track.ordinal)
            {
                continue;
            }
            // Durable before it is named: a manifest that survives a power
            // loss must not name bytes that did not.
            let staged = self.plan.sup_path(track.ordinal);
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
                    tracing::warn!(file_id = self.plan.file_id, ordinal = track.ordinal, %error, "placing a stored PGS track");
                    track.verdict = Verdict::Transient;
                    track.file = None;
                    track.sha256 = None;
                }
            }
        }

        // What the directory's manifest names now, read at publish rather
        // than at plan time: another publish may have landed in between.
        let replaced = store::read_manifest(&dir).await;

        // (2)
        if let Err(error) = write_manifest(&dir, &manifest).await {
            for name in placed {
                let named_before = replaced.as_ref().is_some_and(|old| names(old, &name));
                if !named_before {
                    let _ = tokio::fs::remove_file(dir.join(name)).await;
                }
            }
            return Err(error);
        }

        sync_directory(&dir).await;

        // (3)
        if let Some(replaced) = replaced {
            for track in &replaced.tracks {
                let (Some(name), Some(sha256)) = (track.file.as_deref(), track.sha256.as_deref())
                else {
                    continue;
                };
                // Only a content name this build would write; a manifest
                // naming a path is not one it will delete through.
                if store::sup_file_name(track.ordinal, sha256).as_deref() != Some(name)
                    || names(&manifest, name)
                {
                    continue;
                }
                let _ = tokio::fs::remove_file(dir.join(name)).await;
            }
        }

        // (4)
        store::record_access(&dir).await;
        PUBLISHED.fetch_add(1, Ordering::Relaxed);
        Ok(manifest)
    }
}

fn names(manifest: &Manifest, name: &str) -> bool {
    manifest
        .tracks
        .iter()
        .any(|track| track.file.as_deref() == Some(name))
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
    let output = tokio::time::timeout(SELF_TEST_STEP_BUDGET, command.output())
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
         plurx_subtitle_ride_along_published_total {published}\n"
    );
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

    // -- the gate ------------------------------------------------------------

    #[test]
    fn the_gate_needs_the_switch_the_self_test_and_a_local_cache() {
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
        source: std::fs::File,
        stamp: SourceStamp,
    }

    fn latch_case() -> LatchCase {
        let dir = crate::test_tempdir().expect("latch");
        let path = dir.path().join("source.mkv");
        std::fs::write(&path, b"source").expect("source");
        LatchCase {
            root: dir.path().join(store::STORE_DIR),
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
        let diagnostics = crate::subtitle_source::diagnostics();
        assert!(diagnostics.riding.iter().any(|ride| ride.file_id == id));
        assert_eq!(
            diagnostics.cap_bytes,
            crate::subtitle_source::MAX_STORE_BYTES
        );
        drop(riding);
        assert!(!active_rides().iter().any(|ride| ride.file_id == id));
    }

    /// A file with no PGS track has nothing to latch, and never rides.
    #[tokio::test]
    async fn a_file_with_no_pgs_track_never_rides() {
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
        file.subtitle_streams = (1..=3)
            .map(|index| plurx_core::domain::SubtitleStream {
                index,
                codec: PGS_CODEC.to_owned(),
                language: Some("eng".into()),
                title: None,
                default: index == 1,
                forced: false,
                hearing_impaired: false,
            })
            .collect();
        let engine = "e".repeat(64);
        let plan = RideAlongPlan::standalone(dir.path(), file.id, stamp_of(&source), vec![0, 1, 2])
            .expect("plan");

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
        assert_eq!(verdicts_of(&riding), vec![(1, Verdict::Kept)]);
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

    /// The gate is closed until the startup self-test passes, and open after.
    /// Linux only: elsewhere the filesystem check keeps it closed for good.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn the_ride_along_is_off_until_the_startup_self_test_passes() {
        plurx_core::testfixtures::require_ffmpeg();
        let cache = fresh_cache();
        let catalog = plurx_core::store::SqliteStore::open_in_memory().expect("catalog");
        set_self_test_state(SelfTest::NotRun);
        assert!(RideAlongGate::open(&catalog, cache.path()).await.is_none());
        set_self_test_state(SelfTest::Failed {
            reason: "test".into(),
        });
        assert!(RideAlongGate::open(&catalog, cache.path()).await.is_none());
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
        assert!(RideAlongGate::open(&catalog, cache.path()).await.is_some());
        // One switch: off stops the producer too.
        plurx_core::store::SettingsStore::put_setting(
            &catalog,
            plurx_core::store::keys::SUBTITLE_STORED_SOURCES,
            "0",
        )
        .await
        .expect("switch off");
        assert!(RideAlongGate::open(&catalog, cache.path()).await.is_none());
    }
}
