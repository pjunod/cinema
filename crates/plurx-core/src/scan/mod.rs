//! Library scanning: walk the tree, place items in the hierarchy, record
//! files with their probe metadata, and reconcile away vanished files.
//!
//! Incremental by design (REQ-LIB-3): a file whose size and mtime are
//! unchanged is skipped without re-probing, so a rescan of a large library is
//! cheap and easy on shared storage. Probing runs sequentially for the same
//! reason — a scan should not thrash a NAS.

pub mod dv_title;
pub mod exif;
pub mod home;
pub mod nfo;
pub mod parse;
pub mod probe;
mod probe_batch;
pub mod recordings;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::domain::{Item, ItemKind, Library, LibraryKind, MetadataPatch, NewItem, ProbeResult};
use crate::error::{ProbeError, StoreError};
use crate::store::{
    PublicationStore, ReconcileOutcome, RootFingerprintStatus, RootRemovalOutcome, Store,
};

/// A cancelled census must never reach reconcile, even if its last page was
/// empty. Blocking walkers and bounded probe children are joined by their
/// owners before this error reaches the caller's admission guard.
fn check_scan_cancellation() -> Result<(), StoreError> {
    crate::process::bounded::check_cancellation()
        .map_err(|error| StoreError::Task(error.to_string()))
}

const WALK_PAGE: usize = 256;
const WALK_PAGES_IN_FLIGHT: usize = 4;
const SCAN_PHASE_BUCKETS: [(u64, &str); 6] = [
    (1_000_000_000, "1"),
    (5_000_000_000, "5"),
    (15_000_000_000, "15"),
    (60_000_000_000, "60"),
    (300_000_000_000, "300"),
    (900_000_000_000, "900"),
];

#[derive(Debug)]
enum WalkEvent {
    Candidate(PathBuf),
    Error { at: String, error: String },
    RootNotDirectory(PathBuf),
}

#[derive(Clone, Copy)]
struct WalkFilter {
    kind: LibraryKind,
}

impl WalkFilter {
    fn accepts(self, path: &Path) -> bool {
        match self.kind {
            LibraryKind::Books => book_kind_for_path(path).is_some(),
            LibraryKind::Home => {
                is_video(path) || (home::is_photo(path) && !home::is_artwork_sidecar(path))
            }
            _ => is_video(path),
        }
    }
}

fn spawn_walker(
    roots: Vec<PathBuf>,
    filter: WalkFilter,
) -> (
    tokio::sync::mpsc::Receiver<Vec<WalkEvent>>,
    tokio::task::JoinHandle<()>,
) {
    spawn_walker_with_entry_hook(roots, filter, || {})
}

fn spawn_walker_with_entry_hook<F>(
    roots: Vec<PathBuf>,
    filter: WalkFilter,
    entry_hook: F,
) -> (
    tokio::sync::mpsc::Receiver<Vec<WalkEvent>>,
    tokio::task::JoinHandle<()>,
)
where
    F: Fn() + Send + 'static,
{
    let cancellation = crate::process::bounded::cancellation();
    let (tx, rx) = tokio::sync::mpsc::channel(WALK_PAGES_IN_FLIGHT);
    let handle = tokio::task::spawn_blocking(move || {
        let mut page = Vec::with_capacity(WALK_PAGE);
        let mut last_root = None;
        for root in roots {
            if cancellation
                .as_ref()
                .is_some_and(|token| token.is_cancelled())
            {
                return;
            }
            last_root = Some(root.clone());
            if !root.is_dir() {
                page.push(WalkEvent::RootNotDirectory(root.clone()));
            } else {
                for entry in WalkDir::new(&root).follow_links(true) {
                    // Candidate pages are deliberately based on matching media,
                    // but cancellation is based on every visited entry. A large
                    // subtitles/documents tree may produce no page at all; only
                    // checking `blocking_send` would keep walking that tree after
                    // the scan and its lease were gone.
                    if tx.is_closed()
                        || cancellation
                            .as_ref()
                            .is_some_and(|token| token.is_cancelled())
                    {
                        tracing::warn!(
                            path = %root.display(),
                            "walker orphaned: consumer gone before walk finished"
                        );
                        return;
                    }
                    entry_hook();
                    let event = match entry {
                        Ok(entry)
                            if entry.file_type().is_file() && filter.accepts(entry.path()) =>
                        {
                            Some(WalkEvent::Candidate(entry.into_path()))
                        }
                        Ok(_) => None,
                        Err(error) => Some(WalkEvent::Error {
                            at: error
                                .path()
                                .map(|path| path.display().to_string())
                                .unwrap_or_else(|| root.display().to_string()),
                            error: error.to_string(),
                        }),
                    };
                    if let Some(event) = event {
                        page.push(event);
                    }
                    if page.len() == WALK_PAGE {
                        let next = Vec::with_capacity(WALK_PAGE);
                        if tx
                            .blocking_send(std::mem::replace(&mut page, next))
                            .is_err()
                        {
                            tracing::warn!(
                                path = %root.display(),
                                "walker orphaned: consumer gone before walk finished"
                            );
                            return;
                        }
                    }
                }
            }
        }
        if !page.is_empty() && tx.blocking_send(page).is_err() {
            let root = last_root
                .as_deref()
                .map(Path::display)
                .map(|path| path.to_string())
                .unwrap_or_else(|| "<none>".to_owned());
            tracing::warn!(path = %root, "walker orphaned: consumer gone before walk finished");
        }
    });
    (rx, handle)
}

#[derive(Default)]
struct ScanPhaseCell {
    count: AtomicU64,
    elapsed_nanos: AtomicU64,
    buckets: [AtomicU64; SCAN_PHASE_BUCKETS.len()],
}

#[derive(Default)]
struct ScanPhaseMetrics {
    walk: ScanPhaseCell,
    process: ScanPhaseCell,
}

static SCAN_PHASE_METRICS: ScanPhaseMetrics = ScanPhaseMetrics {
    walk: ScanPhaseCell {
        count: AtomicU64::new(0),
        elapsed_nanos: AtomicU64::new(0),
        buckets: [const { AtomicU64::new(0) }; SCAN_PHASE_BUCKETS.len()],
    },
    process: ScanPhaseCell {
        count: AtomicU64::new(0),
        elapsed_nanos: AtomicU64::new(0),
        buckets: [const { AtomicU64::new(0) }; SCAN_PHASE_BUCKETS.len()],
    },
};

fn saturating_add(target: &AtomicU64, amount: u64) {
    let _ = target.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        (current != u64::MAX).then(|| current.saturating_add(amount))
    });
}

fn observe_scan_phase(cell: &ScanPhaseCell, elapsed: Duration) {
    let nanos = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
    saturating_add(&cell.count, 1);
    saturating_add(&cell.elapsed_nanos, nanos);
    if let Some(index) = SCAN_PHASE_BUCKETS
        .iter()
        .position(|(upper, _)| nanos <= *upper)
    {
        saturating_add(&cell.buckets[index], 1);
    }
}

/// Render fixed-cardinality scan phase latency metrics without touching a scan.
pub fn prometheus_scan_walk() -> String {
    use std::fmt::Write;

    let mut out = String::from(
        "# HELP plurx_scan_walk_seconds Filesystem scan latency by phase.\n\
         # TYPE plurx_scan_walk_seconds histogram\n",
    );
    for (phase, cell) in [
        ("walk", &SCAN_PHASE_METRICS.walk),
        ("process", &SCAN_PHASE_METRICS.process),
    ] {
        let mut cumulative = 0_u64;
        for (index, (_, upper)) in SCAN_PHASE_BUCKETS.iter().enumerate() {
            cumulative = cumulative.saturating_add(cell.buckets[index].load(Ordering::Relaxed));
            let _ = writeln!(
                out,
                "plurx_scan_walk_seconds_bucket{{phase=\"{phase}\",le=\"{upper}\"}} {cumulative}"
            );
        }
        let count = cell.count.load(Ordering::Relaxed);
        let _ = writeln!(
            out,
            "plurx_scan_walk_seconds_bucket{{phase=\"{phase}\",le=\"+Inf\"}} {count}"
        );
        let seconds = cell.elapsed_nanos.load(Ordering::Relaxed) as f64 / 1_000_000_000.0;
        let _ = writeln!(
            out,
            "plurx_scan_walk_seconds_sum{{phase=\"{phase}\"}} {seconds:.9}"
        );
        let _ = writeln!(
            out,
            "plurx_scan_walk_seconds_count{{phase=\"{phase}\"}} {count}"
        );
    }
    out
}

/// Container extensions we treat as playable video.
const VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "mov", "ts", "m2ts", "webm", "wmv", "flv", "mpg", "mpeg", "vob",
    "ogv", "3gp",
];

static PROBE_OK: AtomicU64 = AtomicU64::new(0);
static PROBE_FAILED: AtomicU64 = AtomicU64::new(0);
static PROBE_TRANSIENT: AtomicU64 = AtomicU64::new(0);
static PROBE_PARSE: AtomicU64 = AtomicU64::new(0);

async fn probe_with_outcome(path: &Path) -> Result<ProbeResult, ProbeError> {
    let result = probe::probe(path).await;
    let counter = match &result {
        Ok(_) => &PROBE_OK,
        Err(ProbeError::Spawn(_) | ProbeError::Failed { .. }) => &PROBE_FAILED,
        Err(ProbeError::Transient { .. }) => &PROBE_TRANSIENT,
        Err(ProbeError::Parse(_)) => &PROBE_PARSE,
    };
    counter.fetch_add(1, Ordering::Relaxed);
    result
}

/// Process-lifetime scan probe outcomes. Fixed labels keep the series bounded;
/// paths and error text belong in the scan report and log, not metric labels.
pub fn prometheus_probe_outcomes() -> String {
    format!(
        "# HELP plurx_scan_probe_outcomes_total Scan ffprobe outcomes by result class.\n\
         # TYPE plurx_scan_probe_outcomes_total counter\n\
         plurx_scan_probe_outcomes_total{{outcome=\"ok\"}} {}\n\
         plurx_scan_probe_outcomes_total{{outcome=\"failed\"}} {}\n\
         plurx_scan_probe_outcomes_total{{outcome=\"transient\"}} {}\n\
         plurx_scan_probe_outcomes_total{{outcome=\"parse\"}} {}\n",
        PROBE_OK.load(Ordering::Relaxed),
        PROBE_FAILED.load(Ordering::Relaxed),
        PROBE_TRANSIENT.load(Ordering::Relaxed),
        PROBE_PARSE.load(Ordering::Relaxed),
    )
}

/// Audio containers accepted in a Books library. These are formats ffmpeg's
/// existing probe/direct-play stack understands on the project's supported
/// platforms; M4B is listed separately even though it is an MP4-family
/// container because the extension is the strongest audiobook signal.
pub const AUDIOBOOK_EXTS: &[&str] = &[
    "m4b", "m4a", "mp3", "aac", "flac", "ogg", "opus", "wav", "wma",
];

/// Readable text/illustrated book formats. They are indexed and served as the
/// original bytes, never sent through ffprobe or the audio/video transcoder.
pub const TEXT_BOOK_EXTS: &[&str] = &["epub", "pdf", "mobi", "azw", "azw3", "fb2", "cbz", "cbr"];

/// What a scan did, in two orthogonal dimensions — mixing them is what made the
/// old status line read as four events when only two files were involved.
///
/// **What happened to each file's record** — `added`, `updated`, `unchanged`,
/// `skipped`, `unreadable` — is a true partition: every candidate file lands in
/// exactly one, and they sum to the number of files the walk found. Assert on
/// that sum, not on `errors`.
///
/// **Whether it came through cleanly** — `degraded`, and the library-wide walk
/// failures — is a *flag*, not a bucket. A file whose probe failed is both
/// `added` and `degraded`: it really is in the library (it shows up, it plays)
/// and it really is missing its codec and duration. Counting it as an error
/// *instead of* added would make a scan claim nothing was added while two new
/// items sat there in the UI, which is a worse lie than the overlap.
///
/// `errors` stays the headline count the UI reddens: `unreadable + degraded +
/// walk failures`. It deliberately overlaps the record buckets, and the fields
/// below let the UI show *which* — "2 added (2 incomplete)" rather than "2
/// added … 2 errors" with no stated relationship between the two numbers.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScanReport {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub removed_files: usize,
    pub pruned_items: usize,
    pub skipped: usize,
    /// Files the walk listed but that couldn't be stat'd, so nothing was
    /// recorded for them at all. Its own bucket: these are *not* added,
    /// updated, unchanged or skipped. Counted in `errors` too.
    pub unreadable: usize,
    /// Files recorded *without* media detail because ffprobe refused them. A
    /// subset of `added` + `updated`, never a bucket of its own. Counted in
    /// `errors` too — an item that can't be described can't have its playback
    /// decided, only guessed.
    pub degraded: usize,
    /// Files whose media details were recovered on this run after an earlier
    /// probe failed — the permissions were fixed, the disk came back. Counted
    /// inside `unchanged` (nothing about the file itself changed), and reported
    /// separately because it is the visible half of a repair.
    pub repaired: usize,
    pub errors: usize,
    /// Home libraries only: items whose metadata was seeded from an `.nfo`
    /// sidecar on this run. Seeding happens at most once per item, ever.
    pub seeded: usize,
    /// Human-readable problems worth showing the operator: missing roots,
    /// unreadable directories, files that couldn't be read or probed, a scan
    /// that found no video files at all. A non-empty list means the scan's
    /// counts don't tell the whole story.
    ///
    /// INVARIANT: every increment of `errors` also records a line here (or
    /// bumps `suppressed`, once the list is full) *and* logs at ERROR level. A
    /// red error count with nothing to click on is a dead end for whoever has
    /// to fix the library — the count must always be accompanied by what went
    /// wrong and where. ERROR rather than WARN specifically so the operator can
    /// filter the log view down to *only* these and still find them after a
    /// noisy enrichment pass has rolled through the ring buffer.
    pub problems: Vec<String>,
    /// Skipped files, collapsed to one entry per folder.
    ///
    /// Skips are the one problem that arrives in bulk: a show whose files carry
    /// no `S01E02` skips *every episode*, so a single mis-named series produced
    /// two dozen near-identical lines and a real library produced hundreds. The
    /// per-file list was technically complete and practically unreadable — the
    /// operator had to infer "oh, it's Paper Moons" from twenty-four paths
    /// that differed only in the episode number.
    ///
    /// One row per folder with a count says the same thing in one line, and
    /// says the thing the operator actually acts on: which SHOW needs renaming.
    pub skip_groups: Vec<SkipGroup>,
    /// Problems that occurred but aren't listed above, because `problems` hit
    /// [`MAX_PROBLEMS`]. Rendered as a trailing "…and N more" line rather than
    /// serialized, so the operator knows the list is truncated, not complete.
    #[serde(skip)]
    suppressed: usize,
}

/// One file a scan placed: what it became, and where it is.
///
/// Returned by the targeted scan so the caller can act on the result instead
/// of polling to find out what happened.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlacedFile {
    pub item_id: i64,
    pub file_id: i64,
    pub path: String,
}

/// One folder's skipped files, collapsed into a single reportable row.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkipGroup {
    /// The folder the files sit under — the show directory, not the release
    /// subfolder. Grouping any deeper reproduces the per-file list one level
    /// up: `Season 1/Paper.Moons.S01E02.DVDRip.XviD-GROUP/` is still one
    /// row per episode.
    pub folder: String,
    /// How many files were skipped there. Never capped — the samples are.
    pub count: usize,
    /// Why, in the same words the per-file line used.
    pub reason: String,
    /// A few of the filenames, so the shape of the problem is visible without
    /// opening the server log.
    pub samples: Vec<String>,
}

impl ScanReport {
    /// Record a problem, capped. Past the cap only the count grows; the
    /// trailing summary line is added by [`Self::seal_problems`]. Use this for
    /// anything that also bumps `errors` — never push to `problems` directly
    /// from the scan loop, or the cap stops holding.
    fn note(&mut self, problem: String) {
        if self.problems.len() < MAX_PROBLEMS {
            self.problems.push(problem);
        } else {
            self.suppressed += 1;
        }
    }

    /// Append the "…and N more" line, if the cap swallowed anything. Called
    /// once, at the end of a scan.
    fn seal_problems(&mut self) {
        if self.suppressed > 0 {
            let n = self.suppressed;
            let s = if n == 1 { "" } else { "s" };
            self.problems.push(format!(
                "…and {n} more problem{s} not listed — see the server log \
                 (Settings → System) for every one"
            ));
        }
    }

    /// Add a bounded problem after the filesystem pass has already sealed its
    /// report. Targeted-scan integrations use this for post-placement hints;
    /// rebuilding the summary keeps the same cap as scanner-originated notes.
    pub fn add_problem(&mut self, problem: String) {
        if self.suppressed > 0 {
            self.problems.pop();
        }
        self.note(problem);
        self.seal_problems();
    }
}

/// What a re-probe pass did. Its own type rather than a [`ScanReport`] because
/// nothing here walks the disk: no file is added, removed or reconciled, and
/// saying so with empty scan counters would invite reading them as real.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ReprobeReport {
    /// Files that had no media details when the pass started.
    pub attempted: usize,
    /// Files that have them now.
    pub repaired: usize,
    /// Files that failed again — same reason or a new one; see `problems`.
    pub still_failing: usize,
    /// Files whose record had vanished (or whose path had) by the time the pass
    /// reached them. Not an error: a rescan removed them, which is correct.
    pub gone: usize,
    pub problems: Vec<String>,
}

/// Re-run ffprobe over files whose probe never succeeded, and record whatever
/// it says now.
///
/// This exists because the incremental scan is keyed on size and mtime, and the
/// usual fixes for a failed probe — `chmod`, remounting a share, replacing a
/// truncated download with the same bytes — change neither. Without a way to
/// ask again, a file that failed once is a permanent placeholder in the
/// library: no codec, no duration, and playback decisions made by guessing.
///
/// `files` are the records to retry; pass [`Store::files_missing_probe`] to
/// sweep a library, or one item's files for a single "reanalyze". Sequential on
/// purpose, like the scan: a repair pass must not thrash a NAS either.
pub async fn reprobe_files(
    store: &dyn Store,
    files: &[crate::domain::MediaFile],
) -> Result<ReprobeReport, StoreError> {
    reprobe_files_with_publication(&PublicationStore::unfenced(store), files).await
}

pub async fn reprobe_files_with_publication(
    store: &PublicationStore<'_>,
    files: &[crate::domain::MediaFile],
) -> Result<ReprobeReport, StoreError> {
    let mut report = ReprobeReport::default();
    let pipeline = probe::pipeline_digest().await;
    for batch in files.chunks(128) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
        let pending = probe_batch::enqueue(store, batch, pipeline.as_deref()).await?;
        // Complete admitted leaves before any legacy local work in this page.
        let ordered = batch
            .iter()
            .filter(|file| pending.contains_key(&file.id))
            .chain(batch.iter().filter(|file| !pending.contains_key(&file.id)));
        for file in ordered {
            check_scan_cancellation()?;
            report.attempted += 1;
            let path_str = file.path.to_string_lossy().into_owned();
            // Re-stat rather than trusting the stored size/mtime: if the file has
            // changed since, the fresh values are what belong in the record.
            let (size, mtime) = match file_stat(&file.path).await {
                Ok(stat) => stat,
                Err(e) => {
                    report.gone += 1;
                    tracing::warn!(path = %path_str, error = %e, "cannot stat file during re-probe");
                    report.problems.push(format!(
                        "`{path_str}` could not be read at all: {e} — check that the path still \
                     exists and is readable by the plurx user"
                    ));
                    continue;
                }
            };
            if let Some(job_id) = pending.get(&file.id) {
                match probe_batch::consume(store, file, job_id, deadline).await? {
                    Some(true) => {
                        report.repaired += 1;
                        continue;
                    }
                    Some(false) => {
                        report.gone += 1;
                        continue;
                    }
                    None => {
                        report.still_failing += 1;
                        report.problems.push(format!("`{path_str}`: durable leaf probe failed; inspect background job {job_id}"));
                        continue;
                    }
                }
            }
            match probe_with_outcome(&file.path).await {
                Ok(probe) => {
                    if file_stat(&file.path).await.ok() != Some((size, mtime)) {
                        report.gone += 1;
                        continue;
                    }
                    store
                        .upsert_file(file.item_id, &path_str, size, mtime, &probe)
                        .await?;
                    report.repaired += 1;
                    tracing::info!(path = %path_str, "media details recovered");
                }
                Err(e @ ProbeError::Transient { .. }) => {
                    report.still_failing += 1;
                    tracing::error!(path = %path_str, error = %e, "re-probe did not finish");
                    report.problems.push(format!(
                    "`{path_str}` still has no media details because its probe did not finish: {e}"
                ));
                }
                Err(e) => {
                    report.still_failing += 1;
                    tracing::error!(path = %path_str, error = %e, "re-probe failed");
                    report
                        .problems
                        .push(format!("`{path_str}` still has no media details: {e}"));
                }
            }
        }
    }
    Ok(report)
}

/// Cap on individual problem messages recorded per scan (the counts still
/// include everything; this just keeps the report readable). Generous enough
/// that a handful of bad files in a big library all get named.
const MAX_PROBLEMS: usize = 25;

/// Cap on skipped-file *groups* reported. Skips are grouped by folder, so this
/// is a cap on distinct mis-named shows rather than on files — a library where
/// more than this many folders are broken has a bigger problem than the report
/// can usefully enumerate, and the counts still cover everything.
const MAX_SKIP_GROUPS: usize = 20;

/// Filenames kept per group, to show the shape of the problem without
/// reprinting the list this grouping exists to collapse.
const SKIP_SAMPLES: usize = 3;

/// Live counters a running scan updates as it goes. Shared with whoever wants
/// to display progress (the HTTP status endpoint samples these atomics without
/// touching the scan itself). Probing a big library over a NAS takes minutes;
/// "processing 412 of 3801" is the difference between progress and a hang.
#[derive(Debug, Default)]
pub struct ScanProgress {
    /// Candidate media files discovered by the directory walk.
    pub found: AtomicUsize,
    /// Files handled so far (unchanged, added, updated, skipped, or errored).
    pub processed: AtomicUsize,
    /// Files added or updated so far.
    pub changed: AtomicUsize,
    /// The ids of those files, in the order the walk recorded them, for work
    /// that should reach new and changed media before its backlog (the
    /// fragment-index pass). The scan's owner takes them when it ends.
    pub changed_files: std::sync::Mutex<Vec<i64>>,
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn extension_in(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| extensions.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// The first-class book media type implied by a file extension.
pub fn book_kind_for_path(path: &Path) -> Option<ItemKind> {
    if extension_in(path, AUDIOBOOK_EXTS) {
        Some(ItemKind::Audiobook)
    } else if extension_in(path, TEXT_BOOK_EXTS) {
        Some(ItemKind::Book)
    } else {
        None
    }
}

fn text_book_probe(path: &Path) -> ProbeResult {
    let container = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    // `raw_json` is also the durable "analysis succeeded" bit. Text books do
    // not belong in ffprobe, but leaving this null would make every retry pass
    // treat a healthy EPUB/PDF as a failed media probe forever.
    let raw_json = container.as_ref().map(|kind| {
        serde_json::json!({
            "book": { "format": kind },
            "streams": [],
            "chapters": []
        })
        .to_string()
    });
    ProbeResult {
        container,
        raw_json,
        ..ProbeResult::default()
    }
}

fn recognized_extensions(library: &Library) -> String {
    match library.kind {
        LibraryKind::Home => format!("{}, {}", VIDEO_EXTS.join(", "), home::PHOTO_EXTS.join(", ")),
        LibraryKind::Books => format!(
            "{}, {}",
            AUDIOBOOK_EXTS.join(", "),
            TEXT_BOOK_EXTS.join(", ")
        ),
        _ => VIDEO_EXTS.join(", "),
    }
}

/// File size and mtime (unix seconds). The `io::Error` is kept rather than
/// flattened to `None` so a stat failure can tell the operator *why* (denied,
/// dangling symlink, vanished mid-scan) instead of just incrementing a counter.
/// A file's catalogue identity on disk: size in bytes and mtime in whole
/// seconds, the two values every scan stores and compares.
pub async fn file_stat(path: &Path) -> std::io::Result<(i64, i64)> {
    let meta = tokio::fs::metadata(path).await?;
    let size = meta.len() as i64;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Ok((size, mtime))
}

/// Scan one library end to end. `store` is the full store; only media methods
/// are used. Returns a tally of what changed.
pub async fn scan_library(store: &dyn Store, library: &Library) -> Result<ScanReport, StoreError> {
    scan_library_with_progress(store, library, None).await
}

/// Like [`scan_library`], updating `progress` (when given) as the scan runs.
pub async fn scan_library_with_progress(
    store: &dyn Store,
    library: &Library,
    progress: Option<&ScanProgress>,
) -> Result<ScanReport, StoreError> {
    scan_library_with_progress_and_prune_percent(
        store,
        library,
        progress,
        crate::config::DEFAULT_SCAN_PRUNE_PERCENT,
    )
    .await
}

/// Scan with a deletion ceiling expressed as a share of the library's known
/// files. Integer division rounds down so the configured percentage is a hard
/// ceiling; zero disables automatic removal.
pub async fn scan_library_with_progress_and_prune_percent(
    store: &dyn Store,
    library: &Library,
    progress: Option<&ScanProgress>,
    prune_percent: u8,
) -> Result<ScanReport, StoreError> {
    let known = store.library_file_paths(library.id).await?.len() as u64;
    let percent = u64::from(prune_percent.min(100));
    let prune_limit = if percent == 0 || known == 0 {
        0
    } else {
        (known.saturating_mul(percent) / 100).max(1)
    };
    scan_library_with_publication_and_prune_limit(
        &PublicationStore::unfenced(store),
        library,
        progress,
        prune_limit,
    )
    .await
}

/// Fenced counterpart used by cluster-wide scan owners.
pub async fn scan_library_with_publication_and_prune_percent(
    store: &PublicationStore<'_>,
    library: &Library,
    progress: Option<&ScanProgress>,
    prune_percent: u8,
) -> Result<ScanReport, StoreError> {
    let known = store.library_file_paths(library.id).await?.len() as u64;
    let percent = u64::from(prune_percent.min(100));
    let prune_limit = if percent == 0 || known == 0 {
        0
    } else {
        (known.saturating_mul(percent) / 100).max(1)
    };
    scan_library_with_publication_and_prune_limit(store, library, progress, prune_limit).await
}

/// Cluster-ready scan entry point. `prune_limit` is the maximum number of
/// known file rows one completed walk may remove. A stale-but-present mount
/// therefore cannot turn one bad view of shared storage into an unbounded
/// cluster-wide delete.
pub async fn scan_library_with_progress_and_prune_limit(
    store: &dyn Store,
    library: &Library,
    progress: Option<&ScanProgress>,
    prune_limit: u64,
) -> Result<ScanReport, StoreError> {
    scan_library_with_publication_and_prune_limit(
        &PublicationStore::unfenced(store),
        library,
        progress,
        prune_limit,
    )
    .await
}

pub async fn scan_library_with_publication_and_prune_limit(
    store: &PublicationStore<'_>,
    library: &Library,
    progress: Option<&ScanProgress>,
    prune_limit: u64,
) -> Result<ScanReport, StoreError> {
    check_scan_cancellation()?;
    let mut report = ScanReport::default();
    let mut seen: HashSet<String> = HashSet::new();
    // Capture the configured path-set identity before walking. A root that
    // vanishes mid-scan must produce one bounded refusal, not a successful
    // walk followed by a fatal error and an immediate full retry. The same
    // canonical read gives the spellings a catalogued path can carry (see
    // `superseded_paths`).
    let fingerprint_paths = library.paths.clone();
    let (fingerprint, spellings) = match tokio::task::spawn_blocking(move || {
        (
            library_root_fingerprint(&fingerprint_paths),
            root_spellings(&fingerprint_paths),
        )
    })
    .await
    .map_err(|error| StoreError::Task(format!("library root fingerprint task failed: {error}")))?
    {
        (Ok(fingerprint), spellings) => (Some(fingerprint), spellings),
        (Err(error), _) => {
            report.note(format!(
                "vanished-file cleanup skipped: library root identity could not be read: {error}"
            ));
            (None, None)
        }
    };

    // Collect candidate files first, then process each.
    // A root that is missing or unreadable is a loud, actionable problem — the
    // most common cause is a container path mix-up (the library was configured
    // with a host path that isn't mounted inside the container) or an
    // unmounted NAS. Either way, silently scanning nothing is the worst
    // possible answer.
    let walk_started = Instant::now();
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    let mut walk_errors = 0usize;
    if let Some(p) = progress {
        p.found.store(0, Ordering::Relaxed);
    }
    let (mut pages, walker) =
        spawn_walker(library.paths.clone(), WalkFilter { kind: library.kind });
    while let Some(page) = pages.recv().await {
        let mut page_candidates = 0usize;
        for event in page {
            match event {
                WalkEvent::Candidate(path) => {
                    candidates.push(path);
                    page_candidates += 1;
                }
                WalkEvent::Error { at, error } => {
                    report.errors += 1;
                    walk_errors += 1;
                    tracing::error!(path = %at, error = %error, "cannot read directory entry");
                    report.note(format!("cannot read `{at}`: {error}"));
                }
                WalkEvent::RootNotDirectory(root) => {
                    report.errors += 1;
                    walk_errors += 1;
                    tracing::error!(path = %root.display(), "library path is not a directory");
                    report.note(format!(
                        "library path `{}` does not exist on the server — if plurxd runs in a \
                         container, use the path as mounted inside the container (e.g. `/media/…`), \
                         and check the mount is present",
                        root.display()
                    ));
                }
            }
        }
        if let Some(p) = progress {
            p.found.fetch_add(page_candidates, Ordering::Relaxed);
        }
    }
    walker
        .await
        .map_err(|error| StoreError::Task(format!("library walker task failed: {error}")))?;
    candidates.sort();
    check_scan_cancellation()?;
    observe_scan_phase(&SCAN_PHASE_METRICS.walk, walk_started.elapsed());

    // An empty recordings folder is normal after explicit DVR deletions.
    // Unexpected missing catalog entries still hit reconciliation below.
    if candidates.is_empty() && walk_errors == 0 && library.kind != LibraryKind::Recordings {
        let (what, exts) = match library.kind {
            LibraryKind::Home => ("video or photo files", recognized_extensions(library)),
            LibraryKind::Books => ("book or audiobook files", recognized_extensions(library)),
            _ => ("video files", VIDEO_EXTS.join(", ")),
        };
        report.note(format!(
            "no {what} found under {} — the path exists but contains no recognized \
             media files ({exts})",
            library
                .paths
                .iter()
                .map(|p| format!("`{}`", p.display()))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }

    let process_started = Instant::now();
    let mut placed: Vec<PlacedFile> = Vec::new();
    let skips = record_candidates(
        store,
        library,
        candidates,
        progress,
        &mut report,
        &mut seen,
        Some(&mut placed),
    )
    .await?;
    observe_scan_phase(&SCAN_PHASE_METRICS.process, process_started.elapsed());

    // Reconcile: anything in the DB for this library but not seen on disk is
    // gone. NEVER reconcile after a partial walk — if a root was missing or a
    // directory unreadable (NAS unmounted, permissions), the files under it
    // are invisible, not deleted, and removing them here would wipe the
    // library's records over a transient mount problem.
    check_scan_cancellation()?;
    if walk_errors == 0 {
        let known = store.library_file_paths(library.id).await?;
        // Entries a configuration change left behind, as opposed to files
        // that vanished: under none of the library's paths now (an operator
        // removed the path they were under), or the same file this walk
        // recorded under another spelling of its path. Neither is evidence of
        // a lost mount, so neither counts against the prune bound or the
        // identity's establishment. They go once the identity checks out —
        // after the walk, which has already re-attached media moved to a
        // newly added path to its items, keeping identity and watch state.
        let superseded = superseded_paths(&known, &seen, spellings.as_deref());
        let superseded_ids: HashSet<i64> = superseded.iter().copied().collect();
        let known_count = known.len() - superseded.len();
        let gone: Vec<i64> = known
            .into_iter()
            .filter(|(id, p)| {
                !superseded_ids.contains(id) && !seen.contains(p.to_string_lossy().as_ref())
            })
            .map(|(id, _)| id)
            .collect();
        let Some(fingerprint) = fingerprint else {
            report.errors += 1;
            report.note(
                "vanished-file cleanup skipped because the root identity was unavailable"
                    .to_owned(),
            );
            report.seal_problems();
            return Ok(report);
        };
        let root_status = store
            .ensure_library_root_fingerprint(
                library.id,
                &library.paths,
                &fingerprint,
                known_count == 0 || (!seen.is_empty() && (gone.len() as u64) <= prune_limit),
            )
            .await?;
        match root_status {
            RootFingerprintStatus::Established | RootFingerprintStatus::Matched => {
                // An edit that landed since the walk leaves nothing for this
                // scan to judge: what it walked is no longer the library.
                let current = remove_superseded(store, library, &superseded, &mut report).await?;
                let gone: &[i64] = if current { &gone } else { &[] };
                match store
                    .reconcile_library(library.id, &fingerprint, gone, prune_limit)
                    .await?
                {
                    ReconcileOutcome::Applied {
                        deleted_files,
                        pruned_items,
                    } => {
                        report.removed_files += deleted_files as usize;
                        report.pruned_items += pruned_items as usize;
                    }
                    ReconcileOutcome::RefusedRoot { expected } => {
                        report.errors += 1;
                        tracing::error!(
                            library = %library.name,
                            expected,
                            observed = %fingerprint,
                            "library root identity changed during reconciliation"
                        );
                        report.note(
                            "vanished-file cleanup skipped: this node's mounted media root \
                             does not match the library identity recorded by the cluster"
                                .to_owned(),
                        );
                    }
                    ReconcileOutcome::RefusedPrune { requested, limit } => {
                        report.errors += 1;
                        tracing::error!(
                            library = %library.name,
                            requested,
                            limit,
                            "library reconciliation exceeded its prune bound"
                        );
                        report.note(format!(
                            "vanished-file cleanup skipped: {requested} catalog file entries were \
                             not found on disk; removing them from the catalog would exceed the \
                             configured limit of {limit}. No catalog entries were removed. \
                             Verify the library mounts and recording deletion status"
                        ));
                    }
                }
            }
            RootFingerprintStatus::Mismatch { expected } => {
                report.errors += 1;
                tracing::error!(
                    library = %library.name,
                    expected,
                    observed = %fingerprint,
                    "library root identity does not match cluster truth"
                );
                report.note(
                    "vanished-file cleanup skipped: this node's mounted media root does not \
                     match the library identity recorded by the cluster"
                        .to_owned(),
                );
            }
            RootFingerprintStatus::ConfigurationChanged => {
                report.note(CONFIGURATION_CHANGED_NOTE.to_owned());
            }
            RootFingerprintStatus::Unestablished => {
                report.errors += 1;
                tracing::error!(
                    library = %library.name,
                    observed = %fingerprint,
                    "refusing to trust an empty first observation of an existing library"
                );
                report.note(
                    "vanished-file cleanup skipped: this existing library has no recorded root \
                     identity and the scan saw no media; verify the mount and scan again"
                        .to_owned(),
                );
            }
        }
    } else {
        report.note(
            "vanished-file cleanup skipped: some library paths were missing or unreadable, \
             so absent files were kept rather than removed, and so were entries under paths \
             no longer in the library"
                .to_owned(),
        );
    }

    // Reconcile may have removed one physical part from a multipart book.
    // Derive the item duration from the surviving file rows after that change,
    // including for otherwise-unchanged files and repaired probes.
    refresh_audiobook_runtimes(store, library, &placed).await?;

    // Worst first: the folder costing the most files is the one to rename.
    let mut groups: Vec<SkipGroup> = skips.into_values().collect();
    groups.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.folder.cmp(&b.folder)));
    if groups.len() > MAX_SKIP_GROUPS {
        let dropped: usize = groups[MAX_SKIP_GROUPS..].iter().map(|g| g.count).sum();
        let folders = groups.len() - MAX_SKIP_GROUPS;
        groups.truncate(MAX_SKIP_GROUPS);
        // Silent truncation reads as "that was all of them", so say what was
        // dropped and where the rest lives.
        report.note(format!(
            "…and {dropped} more skipped file{} across {folders} more folder{} — \
             see the server log for every one",
            if dropped == 1 { "" } else { "s" },
            if folders == 1 { "" } else { "s" }
        ));
    }
    report.skip_groups = groups;
    report.seal_problems();

    tracing::info!(
        library = %library.name,
        added = report.added,
        unchanged = report.unchanged,
        removed = report.removed_files,
        pruned = report.pruned_items,
        seeded = report.seeded,
        skipped = report.skipped,
        errors = report.errors,
        "scan complete"
    );
    for problem in &report.problems {
        tracing::warn!(library = %library.name, "scan problem: {problem}");
    }
    Ok(report)
}

/// Whether a library root is mounted and readable here: the scanner's own
/// root identity read ([`library_root_fingerprint`], which canonicalises it)
/// succeeds, and the root lists at least one entry. The second half is what
/// tells an unmounted share from a mounted one, since an unmounted NAS leaves
/// an empty mountpoint that canonicalises perfectly well. A job that finds a
/// catalogued file missing asks this before concluding the file is gone.
pub async fn library_root_available(root: &Path) -> bool {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        library_root_fingerprint(std::slice::from_ref(&root)).is_ok()
            && std::fs::read_dir(&root)
                .map(|mut entries| entries.next().is_some())
                .unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

/// What a scan reports when the library's paths were edited while it ran.
const CONFIGURATION_CHANGED_NOTE: &str =
    "the library's paths changed while this scan ran, so it removed nothing from the catalog; \
     the scan queued by that change covers the new paths";

/// Remove `superseded` (see [`superseded_paths`]) in payload-sized batches.
/// `false`, with a note, when the library's paths changed under the scan.
async fn remove_superseded(
    store: &PublicationStore<'_>,
    library: &Library,
    superseded: &[i64],
    report: &mut ScanReport,
) -> Result<bool, StoreError> {
    for batch in superseded.chunks(crate::store::MAX_RECONCILE_FILE_IDS) {
        match store
            .remove_files_outside_library_roots(library.id, &library.paths, batch)
            .await?
        {
            RootRemovalOutcome::Applied {
                deleted_files,
                pruned_items,
            } => {
                report.removed_files += deleted_files as usize;
                report.pruned_items += pruned_items as usize;
            }
            RootRemovalOutcome::ConfigurationChanged => {
                report.note(CONFIGURATION_CHANGED_NOTE.to_owned());
                return Ok(false);
            }
        }
    }
    if !superseded.is_empty() {
        tracing::info!(
            library = %library.name,
            superseded = superseded.len(),
            removed = report.removed_files,
            pruned = report.pruned_items,
            "removed catalog entries a library path change left behind"
        );
    }
    Ok(true)
}

/// Every spelling of `roots` a catalogued path can start with, lexically
/// normalised: each root as configured and as canonicalised here. `None`
/// when there are no roots or a root does not resolve. Blocking.
fn root_spellings(roots: &[PathBuf]) -> Option<Vec<PathBuf>> {
    if roots.is_empty() {
        return None;
    }
    let mut spellings = Vec::with_capacity(roots.len() * 2);
    for root in roots {
        spellings.push(lexical(root));
        spellings.push(lexical(&root.canonicalize().ok()?));
    }
    Some(spellings)
}

/// The ids of `known` rows a configuration change left behind: not walked
/// this time (`seen` holds the walked spellings) and either the same path as
/// a walked file in another spelling, or — when `spellings` is known — under
/// none of the library's roots. Paths compare component by component, so
/// `/media//tv/x` is `/media/tv/x` and `/media/tv2/x` is not under
/// `/media/tv`. Without `spellings` (a root that does not resolve here) no
/// row is judged outside: one a targeted scan recorded under that root's
/// canonical spelling could not be told from one outside every root.
fn superseded_paths(
    known: &[(i64, PathBuf)],
    seen: &HashSet<String>,
    spellings: Option<&[PathBuf]>,
) -> Vec<i64> {
    let walked: HashSet<PathBuf> = seen.iter().map(|path| lexical(Path::new(path))).collect();
    known
        .iter()
        .filter(|(_, path)| {
            if seen.contains(path.to_string_lossy().as_ref()) {
                return false;
            }
            let path = lexical(path);
            walked.contains(&path)
                || spellings.is_some_and(|roots| !roots.iter().any(|root| path.starts_with(root)))
        })
        .map(|(id, _)| *id)
        .collect()
}

/// `path` with repeated separators, `.` components and a trailing separator
/// gone — what [`Path::components`] already treats as equal.
fn lexical(path: &Path) -> PathBuf {
    path.components().collect()
}

/// Stable identity for the configured root set.
///
/// The canonical path prevents two differently configured mount points from
/// being treated as one. It deliberately omits filesystem-local inode/device
/// identity because those values are not stable across voters for the same
/// network export. Replacement storage mounted at the same path is therefore
/// guarded by the independent prune bound.
pub fn library_root_fingerprint(paths: &[std::path::PathBuf]) -> Result<String, StoreError> {
    let mut roots = Vec::with_capacity(paths.len());
    for path in paths {
        let canonical = path.canonicalize().map_err(|error| {
            StoreError::Task(format!(
                "cannot fingerprint library root `{}`: {error}",
                path.display()
            ))
        })?;
        roots.push(canonical.to_string_lossy().into_owned());
    }
    roots.sort();
    let mut digest = Sha256::new();
    for root in roots {
        digest.update((root.len() as u64).to_be_bytes());
        digest.update(root.as_bytes());
    }
    Ok(hex::encode(digest.finalize()))
}

/// What a targeted scan produced.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TargetedScan {
    pub report: ScanReport,
    pub items: Vec<PlacedFile>,
}

/// Why a path could not be scanned. Both variants name the path and, for the
/// common mistake, the roots it should have been under — a path-mapping slip
/// between two applications is the single most likely cause, and an error
/// that just says "invalid path" makes the operator go hunting.
#[derive(Debug)]
pub enum TargetError {
    /// The path is not under any of this library's roots (or escapes them
    /// via a symlink).
    OutsideRoots {
        path: String,
        roots: Vec<String>,
    },
    Store(StoreError),
    /// The durable queue would not admit the request — it is full, or the
    /// same request is being cancelled. Nothing is broken and nothing was
    /// lost: the caller may try again. Distinct from `Store` because that
    /// one surfaces as an internal error, and a retryable refusal reported
    /// as a 500 sends the operator looking for a crash that never happened.
    Refused(String),
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetError::OutsideRoots { path, roots } => write!(
                f,
                "`{path}` is not under any root of this library ({})",
                roots.join(", ")
            ),
            TargetError::Store(e) => write!(f, "{e}"),
            TargetError::Refused(outcome) => {
                write!(f, "the durable queue refused this request: {outcome}")
            }
        }
    }
}

impl From<StoreError> for TargetError {
    fn from(e: StoreError) -> Self {
        TargetError::Store(e)
    }
}

/// Scan exactly one path — a directory subtree or a single file — and record
/// what is there.
///
/// This is the fast path for "something just landed at X". A full scan walks
/// every root, which on a large NAS is minutes of stat calls to discover one
/// new folder; this walks only what the caller names.
///
/// **It never reconciles, and that is not an oversight.** The full scan
/// deletes rows for files it did not see, which is only safe because it saw
/// everything. A targeted scan sees one subtree by construction, so pruning
/// against it would delete the entire rest of the library — the same reasoning
/// as the existing `walk_errors > 0` guard, which already refuses to reconcile
/// after a partial walk. A targeted scan upserts what it finds and touches
/// nothing else, ever.
pub async fn scan_path(
    store: &dyn Store,
    library: &Library,
    target: &Path,
) -> Result<TargetedScan, TargetError> {
    scan_path_with_publication(&PublicationStore::unfenced(store), library, target).await
}

pub async fn scan_path_with_publication(
    store: &PublicationStore<'_>,
    library: &Library,
    target: &Path,
) -> Result<TargetedScan, TargetError> {
    check_scan_cancellation()?;
    let roots: Vec<String> = library
        .paths
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    // Canonicalize before comparing. Without it `/media/movies/../etc` and a
    // symlink pointing outside the library both pass a string prefix test,
    // which would turn "scan this path" into "read any directory on the
    // host" for anyone holding a scan key.
    let target_path = target.to_path_buf();
    let configured_roots = library.paths.clone();
    let canonicalized = tokio::task::spawn_blocking(move || {
        let canonical = target_path.canonicalize()?;
        let is_file = canonical.is_file();
        let roots = configured_roots
            .into_iter()
            .map(|root| root.canonicalize().unwrap_or(root))
            .collect::<Vec<_>>();
        Ok::<_, std::io::Error>((canonical, is_file, roots))
    })
    .await
    .map_err(|error| {
        TargetError::Store(StoreError::Task(format!(
            "target path canonicalization task failed: {error}"
        )))
    })?;
    let (canonical, target_is_file, canonical_roots) = match canonicalized {
        Ok(result) => result,
        Err(_) => {
            return Err(TargetError::OutsideRoots {
                path: target.display().to_string(),
                roots,
            })
        }
    };
    let under_root = canonical_roots.iter().any(|root| {
        // Component-wise, so `/data` does not match `/database`.
        canonical == *root || canonical.starts_with(root)
    });
    if !under_root {
        return Err(TargetError::OutsideRoots {
            path: canonical.display().to_string(),
            roots,
        });
    }

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    let mut report = ScanReport::default();
    let walk_started = Instant::now();
    if target_is_file {
        if wanted_file(library, &canonical) {
            candidates.push(canonical.clone());
        }
    } else {
        let (mut pages, walker) =
            spawn_walker(vec![canonical.clone()], WalkFilter { kind: library.kind });
        while let Some(page) = pages.recv().await {
            for event in page {
                match event {
                    WalkEvent::Candidate(path) => candidates.push(path),
                    WalkEvent::Error { at, error } => {
                        // A walk error is reported but never fatal, and — unlike
                        // the full scan — it changes nothing about reconcile,
                        // because there is no reconcile to change.
                        report.errors += 1;
                        tracing::error!(target: "plurxd::integrate", path = %at, error = %error,
                            "cannot read directory entry during targeted scan");
                        report.note(format!("cannot read `{at}`: {error}"));
                    }
                    WalkEvent::RootNotDirectory(path) => {
                        let at = path.display().to_string();
                        let error = "path is not a directory";
                        report.errors += 1;
                        tracing::error!(target: "plurxd::integrate", path = %at, error,
                            "cannot read directory entry during targeted scan");
                        report.note(format!("cannot read `{at}`: {error}"));
                    }
                }
            }
        }
        walker.await.map_err(|error| {
            TargetError::Store(StoreError::Task(format!(
                "targeted walker task failed: {error}"
            )))
        })?;
    }
    candidates.sort();
    check_scan_cancellation()?;
    observe_scan_phase(&SCAN_PHASE_METRICS.walk, walk_started.elapsed());

    if candidates.is_empty() {
        report.note(format!(
            "no media files found under `{}` — the path exists but holds nothing \
             recognized ({})",
            canonical.display(),
            recognized_extensions(library)
        ));
    }

    let process_started = Instant::now();
    let mut seen: HashSet<String> = HashSet::new();
    let mut items: Vec<PlacedFile> = Vec::new();
    let skips = record_candidates(
        store,
        library,
        candidates,
        None,
        &mut report,
        &mut seen,
        Some(&mut items),
    )
    .await?;
    observe_scan_phase(&SCAN_PHASE_METRICS.process, process_started.elapsed());
    refresh_audiobook_runtimes(store, library, &items).await?;
    report.skip_groups = skips.into_values().collect();
    report.seal_problems();

    tracing::info!(
        target: "plurxd::integrate",
        library = library.id,
        path = %canonical.display(),
        added = report.added,
        updated = report.updated,
        unchanged = report.unchanged,
        skipped = report.skipped,
        errors = report.errors,
        items = items.len(),
        "targeted scan complete"
    );
    Ok(TargetedScan { report, items })
}

/// Whether this file is one the library cares about. Same rule the full scan
/// applies, in one place so the two cannot drift: photos count only in home
/// libraries, and a poster JPEG beside a movie stays as invisible as it is
/// today.
fn wanted_file(library: &Library, path: &Path) -> bool {
    match library.kind {
        LibraryKind::Books => book_kind_for_path(path).is_some(),
        LibraryKind::Home => {
            is_video(path) || (home::is_photo(path) && !home::is_artwork_sidecar(path))
        }
        _ => is_video(path),
    }
}

/// Wall-clock milliseconds. The DVR's rows carry `updated_at_ms`, and the
/// scan is the one place outside the engine that writes to them.
fn scan_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// Records one batch of candidate files into the library.
///
/// Split out of [`scan_library_with_progress`] so the targeted scan can run
/// the *same* per-file logic rather than a second copy of it. Everything
/// delicate lives here — the size+mtime unchanged short-circuit, the
/// probe-failure repair, placement, the home-video NFO seeding — and a
/// divergence between the full scan and the targeted one would show up as
/// "the same file behaves differently depending on who asked", which is the
/// worst kind of bug to chase.
///
/// Fills `seen` with every path it looked at, which the caller needs for
/// reconcile. Deliberately does NOT reconcile: only the caller knows whether
/// it saw the whole library.
async fn record_candidates(
    store: &PublicationStore<'_>,
    library: &Library,
    candidates: Vec<std::path::PathBuf>,
    progress: Option<&ScanProgress>,
    report: &mut ScanReport,
    seen: &mut HashSet<String>,
    placed_out: Option<&mut Vec<PlacedFile>>,
) -> Result<BTreeMap<String, SkipGroup>, StoreError> {
    let mut placed_sink = placed_out;
    let mut reported_duplicate_directories = HashSet::new();
    // Skips are informational, not errors — kept apart so a library full of
    // unparseable filenames can't push real errors past MAX_PROBLEMS.
    // Skips accumulate per folder rather than per file: one mis-named show is
    // one problem, however many episodes it has.
    let mut skips: BTreeMap<String, SkipGroup> = BTreeMap::new();

    for path in candidates {
        check_scan_cancellation()?;
        if let Some(p) = progress {
            p.processed.fetch_add(1, Ordering::Relaxed);
        }
        let path_str = path.to_string_lossy().into_owned();
        seen.insert(path_str.clone());

        let (size, mtime) = match file_stat(&path).await {
            Ok(stat) => stat,
            Err(e) => {
                // The directory walk listed this file, but stat'ing it failed:
                // permissions, a dangling symlink, or the file was moved or
                // deleted between the walk and now. Whatever the cause, it is
                // NOT "this file is gone" — it stays in `seen`, so reconcile
                // leaves its DB record alone.
                report.errors += 1;
                report.unreadable += 1;
                tracing::error!(path = %path_str, error = %e, "cannot stat file; skipping");
                report.note(format!(
                    "cannot read `{path_str}`: {e} — check permissions, or whether it is a \
                     broken symlink or was moved mid-scan; its existing record was kept"
                ));
                continue;
            }
        };

        // Incremental: unchanged file → skip probe entirely.
        let existing = store.get_file_by_path(&path_str).await?;
        if let Some(ref ex) = existing {
            if ex.size == size && ex.mtime == mtime {
                report.unchanged += 1;
                // Unchanged is still an ANSWER. A caller that asks "what did
                // you make of this path" gets the item either way — a file
                // that was already indexed (because a scheduled scan beat the
                // notification, or the same import was retried) is not a file
                // the caller should be told nothing about.
                if let Some(sink) = placed_sink.as_deref_mut() {
                    sink.push(PlacedFile {
                        item_id: ex.item_id,
                        file_id: ex.id,
                        path: path_str.clone(),
                    });
                }
                // An NFO written *after* the file was scanned still seeds: the
                // check is on the item, not on the file's size+mtime. Costs
                // one stat per unseeded home video, and no probe.
                if library.kind == LibraryKind::Home {
                    if let Some(item) = store.get_item(ex.item_id).await? {
                        if item.nfo_seeded_at.is_none() {
                            let mut nfo_notes = Vec::new();
                            if home::seed_from_nfo(store, &item, &path, &mut nfo_notes).await? {
                                report.seeded += 1;
                            }
                            for note in nfo_notes {
                                report.note(note);
                            }
                        }
                    }
                }
                // One exception to "unchanged means untouched": a file whose
                // probe never succeeded. Size and mtime don't move when the
                // reason it failed is fixed — a `chmod` leaves both alone — so
                // nothing else would ever mark it worth another look, and it
                // would sit in the library forever with no codec and no
                // duration. Retried *in place*, against the item it already
                // belongs to: re-running placement here would re-derive the
                // item from the filename and orphan a home video that had been
                // renamed by an NFO or by hand.
                if !ex.probed {
                    match probe_with_outcome(&path).await {
                        Ok(probe) => {
                            store
                                .upsert_file(ex.item_id, &path_str, size, mtime, &probe)
                                .await?;
                            report.repaired += 1;
                            tracing::info!(path = %path_str, "media details recovered on rescan");
                        }
                        Err(e @ ProbeError::Transient { .. }) => {
                            tracing::error!(path = %path_str, error = %e, "probe still did not finish");
                            report.errors += 1;
                            report.degraded += 1;
                            report.note(format!(
                                "still no media details for `{path_str}` because its probe did not finish: {e} — it will be tried again on the next scan"
                            ));
                        }
                        Err(e) => {
                            tracing::error!(path = %path_str, error = %e, "probe still failing");
                            report.errors += 1;
                            report.degraded += 1;
                            report.note(format!(
                                "still no media details for `{path_str}`: {e} — it plays, but \
                                 without codec, duration or track info its playback decisions \
                                 are guesses"
                            ));
                        }
                    }
                }
                continue;
            }
        }
        let is_new = existing.is_none();

        let owner_problem = if let Some(file) = existing.as_ref() {
            existing_owner_problem(store, library, &path, file.item_id).await?
        } else {
            None
        };
        if let Some(problem) = owner_problem.as_ref() {
            report.errors += 1;
            tracing::error!(path = %path_str, item_id = existing.as_ref().map(|file| file.item_id),
                "existing file ownership is inconsistent: {problem}");
            report.note(format!(
                "existing file `{path_str}` kept its current association but ownership is invalid: {problem}"
            ));
        }

        // An existing canonical path owns its item association. Re-deriving
        // placement from mutable titles here can orphan the old episode and
        // cascade away watch state during reconciliation.
        let placed = match existing.as_ref() {
            Some(file) => PlacedItem {
                placed: home::Placed {
                    id: file.item_id,
                    created: false,
                },
                duplicate: None,
            },
            None => match place_item(store, library, &path).await? {
                Placement::Placed(placed) => placed,
                Placement::Skipped(why) => {
                    report.skipped += 1;
                    // Still logged per file: the log is where completeness belongs,
                    // and the report line points at it.
                    tracing::warn!(path = %path_str, "skipped: {why}");
                    let folder = skip_group_folder(library, &path);
                    let group = skips.entry(folder.clone()).or_insert_with(|| SkipGroup {
                        folder,
                        reason: why.to_owned(),
                        ..SkipGroup::default()
                    });
                    group.count += 1;
                    if group.samples.len() < SKIP_SAMPLES {
                        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                            group.samples.push(name.to_owned());
                        }
                    }
                    continue;
                }
            },
        };
        if let Some(duplicate) = placed.duplicate.as_ref() {
            if reported_duplicate_directories.insert(duplicate.directory.clone()) {
                let candidates = duplicate
                    .candidate_ids
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                tracing::warn!(
                    directory = %duplicate.directory,
                    chosen_id = duplicate.chosen_id,
                    candidate_ids = %candidates,
                    "multiple catalogue items own one media directory"
                );
                report.note(format!(
                    "directory `{}` is owned by duplicate catalogue items [{}]; selected item {} deterministically",
                    duplicate.directory, candidates, duplicate.chosen_id
                ));
            }
        }
        let placed = placed.placed;
        let item_id = placed.id;

        // Probe is best-effort — a weird file still records with null media
        // details rather than failing the whole scan.
        let probe = if library.kind == LibraryKind::Books
            && book_kind_for_path(&path) == Some(ItemKind::Book)
        {
            text_book_probe(&path)
        } else {
            match probe_with_outcome(&path).await {
                Ok(p) => p,
                Err(e @ ProbeError::Transient { .. }) => {
                    tracing::error!(path = %path_str, error = %e, "probe did not finish; recording without media detail");
                    report.errors += 1;
                    report.degraded += 1;
                    report.note(format!(
                        "could not finish reading media details for `{path_str}`: {e} — it was added without codec, duration, or track info and will be tried again on the next scan"
                    ));
                    Default::default()
                }
                Err(e) => {
                    tracing::error!(path = %path_str, error = %e, "probe failed; recording without media detail");
                    report.errors += 1;
                    report.degraded += 1;
                    report.note(format!(
                        "could not read media details for `{path_str}`: {e} — it was added without \
                         codec, duration, or track info, so playback decisions for it are guesses"
                    ));
                    Default::default()
                }
            }
        };

        check_scan_cancellation()?;
        let file_id = store
            .upsert_file(item_id, &path_str, size, mtime, &probe)
            .await?;
        if let Some(sink) = placed_sink.as_deref_mut() {
            // The caller of a targeted scan is owed an answer, not a shrug:
            // "which item did my file become" is the whole question Curator
            // asks, and reconstructing it from a path afterwards would race
            // with the next scan.
            sink.push(PlacedFile {
                item_id,
                file_id,
                path: path_str.clone(),
            });
        }
        if owner_problem.is_none() && library.kind == LibraryKind::Home {
            let mut nfo_notes = Vec::new();
            if home::after_record(store, placed, &path, &probe, mtime, &mut nfo_notes).await? {
                report.seeded += 1;
            }
            for note in nfo_notes {
                report.note(note);
            }
        }
        if owner_problem.is_none()
            && library.kind == LibraryKind::Recordings
            && recordings::after_record(store, placed, file_id, &path, &probe, mtime, scan_now_ms())
                .await?
        {
            report.seeded += 1;
        }
        if is_new {
            report.added += 1;
        } else {
            report.updated += 1;
        }
        if let Some(p) = progress {
            p.changed.fetch_add(1, Ordering::Relaxed);
            p.changed_files
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(file_id);
        }
    }

    Ok(skips)
}

async fn refresh_audiobook_runtimes(
    store: &PublicationStore<'_>,
    library: &Library,
    placed: &[PlacedFile],
) -> Result<(), StoreError> {
    if library.kind != LibraryKind::Books {
        return Ok(());
    }
    let item_ids = placed
        .iter()
        .map(|placed| placed.item_id)
        .collect::<std::collections::BTreeSet<_>>();
    for item_id in item_ids {
        check_scan_cancellation()?;
        let Some(item) = store.get_item(item_id).await? else {
            continue;
        };
        if item.library_id != library.id || item.kind != ItemKind::Audiobook {
            continue;
        }
        let durations = store
            .files_for_item(item_id)
            .await?
            .into_iter()
            .filter_map(|file| file.duration_ms.filter(|duration| *duration > 0))
            .collect::<Vec<_>>();
        if durations.is_empty() {
            // Unknown is not zero. A failed or old probe should keep the
            // runtime absent so the next successful scan can supply it.
            continue;
        }
        let duration_ms = durations.into_iter().sum::<i64>();
        if item.runtime_ms == Some(duration_ms) {
            // On hiqlite this is a consensus write, so an unchanged scheduled
            // scan must not rewrite every audiobook on the shelf.
            continue;
        }
        store
            .apply_metadata(
                item_id,
                &MetadataPatch {
                    runtime_ms: Some(duration_ms),
                    ..MetadataPatch::default()
                },
            )
            .await?;
    }
    Ok(())
}

/// The outcome of trying to identify a file. A skip carries the sentence the
/// scan report will print, written where the decision is actually made — a
/// reason reconstructed later by the caller drifts from the code that skipped.
enum Placement {
    Placed(PlacedItem),
    Skipped(&'static str),
}

struct PlacedItem {
    placed: home::Placed,
    duplicate: Option<DuplicateDirectory>,
}

struct DuplicateDirectory {
    directory: String,
    candidate_ids: Vec<i64>,
    chosen_id: i64,
}

fn placed(id: i64, created: bool) -> Placement {
    Placement::Placed(PlacedItem {
        placed: home::Placed { id, created },
        duplicate: None,
    })
}

fn directory_placement(item: Item, directory: String, candidates: &[Item]) -> Placement {
    Placement::Placed(PlacedItem {
        placed: home::Placed {
            id: item.id,
            created: false,
        },
        duplicate: (candidates.len() > 1).then(|| DuplicateDirectory {
            directory,
            candidate_ids: candidates.iter().map(|candidate| candidate.id).collect(),
            chosen_id: item.id,
        }),
    })
}

fn usable_source_directory(library: &Library, directory: Option<&Path>) -> Option<String> {
    let directory = directory?.canonicalize().ok()?;
    let roots = library
        .paths
        .iter()
        .map(|configured| {
            (
                configured,
                configured
                    .canonicalize()
                    .unwrap_or_else(|_| configured.clone()),
            )
        })
        .collect::<Vec<_>>();
    if roots.iter().any(|(_, root)| directory == *root) {
        return None;
    }
    let (configured_root, root) = roots
        .into_iter()
        .filter(|(_, root)| directory.starts_with(root))
        .max_by_key(|(_, root)| root.components().count())?;
    let suffix = directory.strip_prefix(&root).ok()?;
    // Store paths use the configured root spelling. Rebase a targeted scan's
    // canonical path onto that spelling so it can find files recorded through
    // a symlinked mount.
    Some(configured_root.join(suffix).to_string_lossy().into_owned())
}

async fn existing_owner_problem(
    store: &PublicationStore<'_>,
    library: &Library,
    path: &Path,
    item_id: i64,
) -> Result<Option<String>, StoreError> {
    let Some(item) = store.get_item(item_id).await? else {
        return Ok(Some(format!("item {item_id} is missing")));
    };
    if item.library_id != library.id {
        return Ok(Some(format!(
            "item {item_id} belongs to library {}, not {}",
            item.library_id, library.id
        )));
    }
    let allowed = match library.kind {
        LibraryKind::Movies => item.kind == ItemKind::Movie,
        LibraryKind::Books => book_kind_for_path(path).is_some_and(|kind| item.kind == kind),
        LibraryKind::Home => matches!(item.kind, ItemKind::Video | ItemKind::Photo),
        LibraryKind::Recordings => item.kind == ItemKind::Video,
        LibraryKind::Shows => item.kind == ItemKind::Episode,
    };
    if !allowed {
        return Ok(Some(format!(
            "item {item_id} has kind `{}` which cannot own this file in a `{}` library",
            item.kind.as_str(),
            library.kind.as_str()
        )));
    }
    if library.kind != LibraryKind::Shows {
        return Ok(None);
    }
    let Some(season_id) = item.parent_id else {
        return Ok(Some(format!("episode {item_id} has no season parent")));
    };
    let Some(season) = store.get_item(season_id).await? else {
        return Ok(Some(format!(
            "episode {item_id} names missing season {season_id}"
        )));
    };
    if season.library_id != library.id || season.kind != ItemKind::Season {
        return Ok(Some(format!(
            "episode {item_id} parent {season_id} is not a season in library {}",
            library.id
        )));
    }
    let Some(show_id) = season.parent_id else {
        return Ok(Some(format!("season {season_id} has no show parent")));
    };
    let Some(show) = store.get_item(show_id).await? else {
        return Ok(Some(format!(
            "season {season_id} names missing show {show_id}"
        )));
    };
    if show.library_id != library.id || show.kind != ItemKind::Show {
        return Ok(Some(format!(
            "season {season_id} parent {show_id} is not a show in library {}",
            library.id
        )));
    }
    Ok(None)
}

/// Find-or-create the item a file belongs to.
async fn place_item(
    store: &PublicationStore<'_>,
    library: &Library,
    path: &Path,
) -> Result<Placement, StoreError> {
    match library.kind {
        LibraryKind::Movies => {
            let parsed = parse::parse_movie(path);
            if let Some(directory) =
                usable_source_directory(library, parsed.source_directory.as_deref())
            {
                let candidates = store
                    .find_movies_by_directory(library.id, &directory)
                    .await?;
                if let Some(existing) = candidates.first().cloned() {
                    return Ok(directory_placement(existing, directory, &candidates));
                }
            }
            if let Some(existing) = store
                .find_movie(library.id, &parsed.title, parsed.year)
                .await?
            {
                return Ok(placed(existing.id, false));
            }
            let id = store
                .insert_item(&NewItem {
                    library_id: library.id,
                    kind: ItemKind::Movie,
                    parent_id: None,
                    title: parsed.title,
                    year: parsed.year,
                    season_number: None,
                    episode_number: None,
                })
                .await?;
            Ok(placed(id, true))
        }
        LibraryKind::Books => {
            let Some(kind) = book_kind_for_path(path) else {
                return Ok(Placement::Skipped(
                    "it is not a recognized text-book or audiobook format",
                ));
            };
            let parsed = parse::parse_book(path, &library.paths);
            let identity_path = parsed.identity_path.to_string_lossy();
            if let Some(existing) = store
                .find_book(
                    library.id,
                    kind,
                    &parsed.title,
                    parsed.year,
                    Some(&identity_path),
                )
                .await?
            {
                return Ok(placed(existing.id, false));
            }
            let id = store
                .insert_item(&NewItem {
                    library_id: library.id,
                    kind,
                    parent_id: None,
                    title: parsed.title,
                    year: parsed.year,
                    season_number: None,
                    episode_number: None,
                })
                .await?;
            Ok(placed(id, true))
        }
        // Folders are the organization: the directory tree is mirrored as
        // browsable folder items, and the file itself becomes a video or a
        // photo under it.
        LibraryKind::Home => Ok(match home::place(store, library, path).await? {
            Some(home) => placed(home.id, home.created),
            None => Placement::Skipped("it isn't a video or a photo this library can hold"),
        }),
        // One folder per programme, which is exactly what the engine writes,
        // so the same folder mirroring home video uses gives the right shape.
        LibraryKind::Recordings => Ok(match recordings::place(store, library, path).await? {
            Some(home) => placed(home.id, home.created),
            None => Placement::Skipped("it is not under any of this library's roots"),
        }),
        LibraryKind::Shows => {
            // Anime libraries use absolute numbering; regular shows use S/E.
            let parsed = if library.anime {
                parse::parse_anime_episode(path)
            } else {
                parse::parse_episode(path)
            };
            let parsed = match parsed {
                Ok(parsed) => parsed,
                // Each sentence names what was actually inspected. The report
                // prints the whole path beside it, so a vaguer reason reads as
                // a contradiction the moment the path itself shows a marker.
                Err(parse::EpisodeSkip::Extra) => {
                    return Ok(Placement::Skipped(
                        "the file names itself an extra (`sample`, `trailer`, a `-featurette` \
                         suffix) rather than the episode — it was left out instead of attaching \
                         to the episode as a second version, where a 30-second clip can sort \
                         ahead of the real file",
                    ));
                }
                Err(parse::EpisodeSkip::NoMarker) if library.anime => {
                    return Ok(Placement::Skipped(
                        "no episode number in the file name (expected `S01E02` or an absolute \
                         number like `- 137`)",
                    ));
                }
                Err(parse::EpisodeSkip::NoMarker) => {
                    return Ok(Placement::Skipped(
                        "no season/episode marker (expected `S01E02`, `1x02`, or the crammed \
                         `102` form) in the file name or on the folder directly holding it",
                    ));
                }
            };
            let (show, duplicate) = find_or_create_show(store, library, &parsed).await?;
            let season = find_or_create_season(store, library, show.id, parsed.season).await?;
            if let Some(existing) = store.find_episode(season, parsed.episode).await? {
                return Ok(Placement::Placed(PlacedItem {
                    placed: home::Placed {
                        id: existing.id,
                        created: false,
                    },
                    duplicate,
                }));
            }
            let title = parsed
                .episode_title
                .clone()
                .unwrap_or_else(|| format!("Episode {}", parsed.episode));
            let id = store
                .insert_item(&NewItem {
                    library_id: library.id,
                    kind: ItemKind::Episode,
                    parent_id: Some(season),
                    title,
                    year: None,
                    season_number: Some(parsed.season),
                    episode_number: Some(parsed.episode),
                })
                .await?;
            Ok(Placement::Placed(PlacedItem {
                placed: home::Placed { id, created: true },
                duplicate,
            }))
        }
    }
}

async fn find_or_create_show(
    store: &PublicationStore<'_>,
    library: &Library,
    parsed: &parse::ParsedEpisode,
) -> Result<(Item, Option<DuplicateDirectory>), StoreError> {
    if let Some(directory) = usable_source_directory(library, parsed.source_directory.as_deref()) {
        let candidates = store
            .find_shows_by_directory(library.id, &directory, parsed.season)
            .await?;
        if let Some(show) = candidates.first().cloned() {
            let duplicate = (candidates.len() > 1).then(|| DuplicateDirectory {
                directory,
                candidate_ids: candidates.iter().map(|candidate| candidate.id).collect(),
                chosen_id: show.id,
            });
            return Ok((show, duplicate));
        }
    }
    if let Some(show) = store
        .find_show(library.id, &parsed.show_title, parsed.show_year)
        .await?
    {
        return Ok((show, None));
    }
    let id = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Show,
            parent_id: None,
            title: parsed.show_title.clone(),
            year: parsed.show_year,
            season_number: None,
            episode_number: None,
        })
        .await?;
    let show = store
        .get_item(id)
        .await?
        .ok_or_else(|| StoreError::Database("show vanished after insert".to_owned()))?;
    Ok((show, None))
}

async fn find_or_create_season(
    store: &PublicationStore<'_>,
    library: &Library,
    show_id: i64,
    season_number: i32,
) -> Result<i64, StoreError> {
    if let Some(season) = store.find_season(show_id, season_number).await? {
        return Ok(season.id);
    }
    store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Season,
            parent_id: Some(show_id),
            title: format!("Season {season_number}"),
            year: None,
            season_number: Some(season_number),
            episode_number: None,
        })
        .await
}

/// The folder a skipped file should be reported under: the directory directly
/// beneath the library root that contains it — the show, not the season and
/// not the release folder.
///
/// Grouping deeper defeats the point. `Paper Moons/Season 1/` is one row per
/// season, and `Season 1/Paper.Moons.S01E02.DVDRip.XviD-GROUP/` is one
/// row per episode again, which is the list this replaces.
fn skip_group_folder(library: &Library, path: &Path) -> String {
    for root in &library.paths {
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        // A file sitting loose in the root has no show folder of its own; the
        // root IS its group, and joining the first component would name the
        // file rather than a directory.
        return match rel.components().count() {
            0 | 1 => root.display().to_string(),
            _ => rel
                .components()
                .next()
                .map(|c| root.join(c).display().to_string())
                .unwrap_or_else(|| root.display().to_string()),
        };
    }
    // Not under any configured root, which the walk should make impossible.
    // Fall back to the parent rather than losing the file from the report.
    path.parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests;
