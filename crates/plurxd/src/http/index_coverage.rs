//! How much of this node's video library an exact index can serve, counted
//! per copy identity and split by dynamic-range class.
//!
//! Every file is judged by the rule item detail's badge uses —
//! [`super::browse::vod_index_coverage`], called here rather than restated —
//! so a title this report counts as `partial` is the title whose detail page
//! says `partial`. The answer is node-relative for the same reason the badge
//! is: a cluster artifact counts only when this node's source attestation
//! reaches it.
//!
//! A measurement walks the catalogue and stats every source, so it runs as a
//! background task an admin starts (`POST`), never inside a request deadline,
//! and the page reads its progress and last result (`GET`). One runs at a time
//! per process; the result is process-local and a restart forgets it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use plurx_core::domain::{LibraryKind, MediaFile};
use serde::Serialize;

use super::error::ApiError;
use super::extract::AdminUser;
use crate::availability::{Availability, AvailabilityCache, Observation};
use crate::state::AppState;

/// Files per batched index-status and availability read.
const BATCH: usize = 128;
/// Stats the walk may have blocked at once, in its own cache — never item
/// detail's permits.
const WALK_PROBES: usize = 4;
/// How long one batch waits for its source observations.
const BATCH_OBSERVE: Duration = Duration::from_secs(10);
/// The whole walk's observation wait. A hung mount holds every walk permit;
/// past this the walk stops starting stats and counts the remaining files as
/// unobserved instead of spending ten seconds on each batch.
const TOTAL_OBSERVE: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ClassCoverage {
    pub files: u64,
    pub bytes: u64,
    /// Copy identities a client can ask these files for.
    pub identities: u64,
    /// Of those, the ones with an index this node can serve.
    pub identities_indexed: u64,
    pub indexed: u64,
    pub partial: u64,
    pub pending: u64,
    pub refused: u64,
    pub unsupported: u64,
    /// Files this node stat'ed and did not find. Their cluster artifacts are
    /// not consulted, as item detail does not consult them.
    pub unavailable: u64,
    /// Files whose stat did not answer within the walk's wait, or that the
    /// walk stopped statting once its total wait was spent. Their cluster
    /// artifacts were not consulted either, so some may be counted short.
    pub unobserved: u64,
}

impl ClassCoverage {
    fn add(
        &mut self,
        file: &MediaFile,
        coverage: &super::browse::VodIndexCoverage,
        observation: Availability,
    ) {
        self.files += 1;
        self.bytes = self
            .bytes
            .saturating_add(u64::try_from(file.size).unwrap_or(0));
        self.identities += coverage.identities as u64;
        self.identities_indexed += coverage.present as u64;
        match coverage.status {
            Some("indexed") => self.indexed += 1,
            Some("partial") => self.partial += 1,
            Some("refused") => self.refused += 1,
            Some("unsupported") => self.unsupported += 1,
            _ => self.pending += 1,
        }
        match observation {
            Availability::Available => {}
            Availability::Unavailable => self.unavailable += 1,
            Availability::Unknown => self.unobserved += 1,
        }
    }

    fn absorb(&mut self, other: &Self) {
        self.files += other.files;
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.identities += other.identities;
        self.identities_indexed += other.identities_indexed;
        self.indexed += other.indexed;
        self.partial += other.partial;
        self.pending += other.pending;
        self.refused += other.refused;
        self.unsupported += other.unsupported;
        self.unavailable += other.unavailable;
        self.unobserved += other.unobserved;
    }
}

/// The class a file's copy identities follow from. Dolby Vision is decided
/// exactly as [`crate::fragindex::video_identities`] decides it, then split
/// by profile, because the profile decides how many identities there are:
/// P7 may have a converted one, and none of them is served from the stripped
/// identity alone.
pub(crate) fn coverage_class(file: &MediaFile) -> &'static str {
    if plurx_core::playback::is_dolby_vision(file) {
        return match plurx_core::playback::dolby_vision_profile(file) {
            Some(5) => "dolby_vision_p5",
            Some(7) => "dolby_vision_p7",
            Some(8) => "dolby_vision_p8",
            _ => "dolby_vision_other",
        };
    }
    match file.hdr.as_deref() {
        Some("hdr10" | "hdr10plus") => "hdr10",
        Some("hlg") => "hlg",
        _ => "sdr",
    }
}

/// Libraries whose files are video a client plays. Books are excluded (an
/// audiobook's cover art is a video stream nobody indexes), and so are the
/// photos a Home library holds, by the scanner's own test.
fn counted(kind: LibraryKind, file: &MediaFile) -> bool {
    match kind {
        LibraryKind::Movies | LibraryKind::Shows | LibraryKind::Recordings => true,
        LibraryKind::Home => !plurx_core::scan::home::is_photo(&file.path),
        LibraryKind::Books => false,
    }
}

#[derive(Debug)]
pub(crate) struct CoverageReport {
    pub classes: BTreeMap<&'static str, ClassCoverage>,
    pub have_dovi: bool,
    pub convert: bool,
    pub elapsed: Duration,
}

/// What a running measurement has done so far.
#[derive(Debug, Default)]
pub(crate) struct Progress {
    pub(crate) started_at_ms: AtomicU64,
    pub(crate) files_total: AtomicU64,
    pub(crate) files_done: AtomicU64,
}

pub(crate) async fn measure(
    state: &AppState,
    progress: &Progress,
) -> Result<CoverageReport, plurx_core::error::StoreError> {
    let started = Instant::now();
    let have_dovi = crate::ffmpeg::has_dovi_rpu().await;
    let convert = state.transcode.dv_convert_enabled().await;
    let mut ids = Vec::new();
    for library in state.catalogue.list_libraries().await? {
        if library.kind == LibraryKind::Books {
            continue;
        }
        ids.extend(
            state
                .store
                .library_file_paths(library.id)
                .await?
                .into_iter()
                .map(|(id, _)| (id, library.kind)),
        );
    }
    ids.sort_unstable_by_key(|(id, _)| *id);
    ids.dedup_by_key(|(id, _)| *id);
    progress
        .files_total
        .store(ids.len() as u64, Ordering::Relaxed);

    let availability = AvailabilityCache::for_walk(WALK_PROBES, BATCH);
    let mut classes: BTreeMap<&'static str, ClassCoverage> = BTreeMap::new();
    let mut observe_left = TOTAL_OBSERVE;
    for chunk in ids.chunks(BATCH) {
        let mut files = Vec::with_capacity(chunk.len());
        let mut probes = Vec::with_capacity(chunk.len());
        for (id, kind) in chunk {
            // A file removed since the listing is not part of the library.
            let Some(file) = state.catalogue.get_file(*id).await? else {
                continue;
            };
            if file.video_codec.is_none() || !counted(*kind, &file) {
                continue;
            }
            probes.push(state.catalogue.get_file_probe_json(file.id).await?);
            files.push(file);
        }
        if !files.is_empty() {
            let observations = if observe_left.is_zero() {
                vec![
                    Observation {
                        state: Availability::Unknown,
                        observed_at_ms: None,
                        stat: None,
                    };
                    files.len()
                ]
            } else {
                let paths = files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect::<Vec<_>>();
                let waited = Instant::now();
                let observed = availability
                    .observe_many_within(&paths, BATCH_OBSERVE.min(observe_left))
                    .await;
                observe_left = observe_left.saturating_sub(waited.elapsed());
                observed
            };
            let (coverage, _) = super::browse::vod_index_coverage(
                state,
                &files,
                &probes,
                &observations,
                have_dovi,
                convert,
            )
            .await?;
            for ((file, observation), coverage) in files.iter().zip(&observations).zip(&coverage) {
                classes.entry(coverage_class(file)).or_default().add(
                    file,
                    coverage,
                    observation.state,
                );
            }
        }
        progress
            .files_done
            .fetch_add(chunk.len() as u64, Ordering::Relaxed);
    }
    Ok(CoverageReport {
        classes,
        have_dovi,
        convert,
        elapsed: started.elapsed(),
    })
}

fn report_value(state: &AppState, report: &CoverageReport) -> serde_json::Value {
    let mut total = ClassCoverage::default();
    for class in report.classes.values() {
        total.absorb(class);
    }
    serde_json::json!({
        "node_id": state.node_id,
        "measured_at_ms": crate::state::clock_ms(),
        "elapsed_ms": u64::try_from(report.elapsed.as_millis()).unwrap_or(u64::MAX),
        "rule": "item detail vod_index_status: indexed only when every copy identity a client can request has a local exact index or a cluster artifact this node's source attestation reaches",
        "dolby_vision_strip_available": report.have_dovi,
        "dolby_vision_convert": report.convert,
        "classes": report.classes,
        "total": total,
    })
}

/// This process's measurement: the one running, if any, and the last result.
#[derive(Default)]
pub(crate) struct CoverageRuns {
    inner: Mutex<Runs>,
}

#[derive(Default)]
struct Runs {
    running: Option<Arc<Progress>>,
    last: Option<serde_json::Value>,
}

/// Clears `running` however the task ends — finished, failed or panicked.
struct RunningGuard(Arc<CoverageRuns>);

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .running = None;
    }
}

impl CoverageRuns {
    /// Claims the single measurement slot, or `None` while one is running.
    fn begin(&self) -> Option<Arc<Progress>> {
        let mut runs = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if runs.running.is_some() {
            return None;
        }
        let progress = Arc::new(Progress::default());
        progress
            .started_at_ms
            .store(crate::state::clock_ms().max(0) as u64, Ordering::Relaxed);
        runs.running = Some(Arc::clone(&progress));
        Some(progress)
    }

    fn finish(&self, last: serde_json::Value) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last = Some(last);
    }

    fn snapshot(&self) -> serde_json::Value {
        let runs = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        serde_json::json!({
            "running": runs.running.as_ref().map(|progress| serde_json::json!({
                "started_at_ms": progress.started_at_ms.load(Ordering::Relaxed),
                "files_total": progress.files_total.load(Ordering::Relaxed),
                "files_done": progress.files_done.load(Ordering::Relaxed),
            })),
            "last": runs.last,
        })
    }
}

/// Starts a measurement unless one is running; either way the answer is the
/// same snapshot `GET` returns.
pub(crate) fn start_measurement(state: &AppState) -> bool {
    let runs = Arc::clone(&state.index_coverage);
    let Some(progress) = runs.begin() else {
        return false;
    };
    let state = state.clone();
    tokio::spawn(async move {
        let guard = RunningGuard(Arc::clone(&runs));
        let last = match measure(&state, &progress).await {
            Ok(report) => report_value(&state, &report),
            Err(error) => serde_json::json!({
                "node_id": state.node_id,
                "measured_at_ms": crate::state::clock_ms(),
                "error": error.to_string(),
            }),
        };
        runs.finish(last);
        drop(guard);
    });
    true
}

/// GET /api/v1/analysis/coverage — the running measurement and the last
/// result on this node.
pub async fn status(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(state.index_coverage.snapshot()))
}

/// POST /api/v1/analysis/coverage — start a measurement on this node. 202
/// either way; `started` says whether this call started it.
pub async fn start(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let started = start_measurement(&state);
    let mut body = state.index_coverage.snapshot();
    body["started"] = serde_json::Value::Bool(started);
    Ok((StatusCode::ACCEPTED, Json(body)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_measurement_runs_at_a_time_and_the_slot_frees_however_it_ends() {
        let runs = Arc::new(CoverageRuns::default());
        let first = runs.begin().expect("an idle slot starts");
        assert!(runs.begin().is_none(), "a second start joins the first");
        assert!(runs.snapshot()["running"].is_object());
        first.files_total.store(3, Ordering::Relaxed);
        assert_eq!(runs.snapshot()["running"]["files_total"], 3);
        // A panicking task unwinds through the guard.
        let guarded = Arc::clone(&runs);
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = RunningGuard(guarded);
            panic!("measurement failed");
        }));
        assert!(unwound.is_err());
        assert!(runs.snapshot()["running"].is_null());
        assert!(runs.begin().is_some(), "the slot is free again");
    }
}
