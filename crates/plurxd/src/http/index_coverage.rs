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
//! It is an admin read on demand, not a poll: it walks the catalogue, so one
//! runs at a time per process.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::Json;
use plurx_core::domain::{LibraryKind, MediaFile};
use serde::Serialize;

use super::error::ApiError;
use super::extract::AdminUser;
use crate::availability::Availability;
use crate::state::AppState;

/// Files per batched index-status and availability read.
const BATCH: usize = 128;
/// How long one batch waits for its source observations.
const BATCH_OBSERVE: Duration = Duration::from_secs(10);
/// The whole report's observation wait. A hung mount holds every probe
/// permit; past this the remaining batches take whatever is already cached
/// and count the rest as unobserved instead of waiting ten seconds each.
const TOTAL_OBSERVE: Duration = Duration::from_secs(120);

pub(super) static RUNNING: AtomicBool = AtomicBool::new(false);

struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::Release);
    }
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
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
    /// Files whose source this node could not observe within the report's
    /// wait. Their cluster artifacts were not consulted, exactly as item
    /// detail does not consult them, so some may be counted short.
    pub unobserved: u64,
}

impl ClassCoverage {
    fn add(
        &mut self,
        file: &MediaFile,
        coverage: &super::browse::VodIndexCoverage,
        observed: bool,
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
        if !observed {
            self.unobserved += 1;
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
        self.unobserved += other.unobserved;
    }
}

/// The class a file's copy identities follow from. Dolby Vision splits by
/// profile because the profile decides how many identities there are: P7
/// has a converted one, P5 and P8 have a preserved one, none of them is
/// served from the stripped one alone.
pub(crate) fn coverage_class(file: &MediaFile) -> &'static str {
    match plurx_core::playback::dolby_vision_profile(file) {
        Some(5) => "dolby_vision_p5",
        Some(7) => "dolby_vision_p7",
        Some(8) => "dolby_vision_p8",
        Some(_) => "dolby_vision_other",
        None => match file.hdr.as_deref() {
            Some("dolby_vision") => "dolby_vision_other",
            Some("hdr10") => "hdr10",
            Some("hlg") => "hlg",
            _ => "sdr",
        },
    }
}

/// Libraries whose files are video a client plays. Books are excluded: an
/// audiobook's cover art is a video stream nobody indexes.
fn video_library(kind: LibraryKind) -> bool {
    match kind {
        LibraryKind::Movies | LibraryKind::Shows | LibraryKind::Home | LibraryKind::Recordings => {
            true
        }
        LibraryKind::Books => false,
    }
}

pub(crate) struct CoverageReport {
    pub classes: BTreeMap<&'static str, ClassCoverage>,
    pub have_dovi: bool,
    pub convert: bool,
    pub elapsed: Duration,
}

pub(crate) async fn measure(state: &AppState) -> Result<CoverageReport, ApiError> {
    if RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(ApiError::Conflict(
            "an index coverage measurement is already running on this node".to_owned(),
        ));
    }
    let _running = RunningGuard;
    let started = Instant::now();
    let have_dovi = crate::ffmpeg::has_dovi_rpu().await;
    let convert = state.transcode.dv_convert_enabled().await;
    let mut ids = Vec::new();
    for library in state.catalogue.list_libraries().await? {
        if video_library(library.kind) {
            ids.extend(
                state
                    .store
                    .library_file_paths(library.id)
                    .await?
                    .into_iter()
                    .map(|(id, _)| id),
            );
        }
    }
    ids.sort_unstable();
    ids.dedup();

    let mut classes: BTreeMap<&'static str, ClassCoverage> = BTreeMap::new();
    let mut observe_left = TOTAL_OBSERVE;
    for chunk in ids.chunks(BATCH) {
        let mut files = Vec::with_capacity(chunk.len());
        let mut probes = Vec::with_capacity(chunk.len());
        for id in chunk {
            // A file removed since the listing is not part of the library.
            let Some(file) = state.catalogue.get_file(*id).await? else {
                continue;
            };
            if file.video_codec.is_none() {
                continue;
            }
            probes.push(state.catalogue.get_file_probe_json(file.id).await?);
            files.push(file);
        }
        if files.is_empty() {
            continue;
        }
        let paths = files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let waited = Instant::now();
        let observations = state
            .detail_availability
            .observe_many_within(&paths, BATCH_OBSERVE.min(observe_left))
            .await;
        observe_left = observe_left.saturating_sub(waited.elapsed());
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
                observation.state == Availability::Available,
            );
        }
    }
    Ok(CoverageReport {
        classes,
        have_dovi,
        convert,
        elapsed: started.elapsed(),
    })
}

/// GET /api/v1/analysis/coverage — copy-identity index coverage on this node.
pub async fn coverage(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let report = measure(&state).await?;
    let mut total = ClassCoverage::default();
    for class in report.classes.values() {
        total.absorb(class);
    }
    Ok(Json(serde_json::json!({
        "node_id": state.node_id,
        "measured_at_ms": crate::state::clock_ms(),
        "elapsed_ms": u64::try_from(report.elapsed.as_millis()).unwrap_or(u64::MAX),
        "rule": "item detail vod_index_status: indexed only when every copy identity a client can request has a local exact index or a cluster artifact this node's source attestation reaches",
        "dolby_vision_strip_available": report.have_dovi,
        "dolby_vision_convert": report.convert,
        "classes": report.classes,
        "total": total,
    })))
}
