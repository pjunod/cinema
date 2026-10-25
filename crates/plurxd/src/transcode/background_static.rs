//! One original selected static-background owner, independent of a job pass.
//! The fixed slot is installed before the first admitted P/native await. A
//! dropped waiter releases its task guard, never its P/R/S or Store mutation.

use super::*;
use plurx_core::adaptive_resources::runtime::finite_vod::{
    FiniteApplicationLease, FiniteCloseTicket,
};
use plurx_core::adaptive_resources::runtime::planning_probe::{
    FixedPlanningProbeCleanup, FixedPlanningProbeOwner, FixedProbeMetadataLease,
};
use plurx_core::store::background_jobs::{EncodedOutputIntent, JobToken};
use std::sync::atomic::AtomicBool;

#[derive(Default)]
pub(super) struct StaticArtifactRuns {
    // The existing BACKGROUND_HEAVY_LIMIT is one selected producer. The slot
    // itself has no growing heap; its Arc entry is included in the original
    // P application projection before allocation/native entry.
    slot: std::sync::Mutex<Option<Arc<StaticArtifactEntry>>>,
}

pub(super) struct StaticArtifactEntry {
    pub(super) original: JobToken,
    pub(super) intent: EncodedOutputIntent,
    pub(super) policy_generation: String,
    pub(super) file_id: i64,
    pub(super) budget: u64,
    pub(super) planning: Mutex<StaticPlanning>,
    pub(super) report: Mutex<Option<FixedProbeMetadataLease>>,
    pub(super) finite: Mutex<StaticFinite>,
    pub(super) active: AtomicBool,
    // Last: the original P/M application payload stays charged through all
    // fields above and any aliases held by the original coordinator.
    pub(super) planning_lease: std::sync::Mutex<Option<FiniteApplicationLease>>,
}

pub(super) enum StaticPlanning {
    Empty,
    Original(FixedPlanningProbeOwner),
    Closing(FixedPlanningProbeCleanup),
    Closed,
}

pub(super) enum StaticFinite {
    Empty,
    Planned(crate::vodserve::StaticArtifactAdmission),
    /// Core admission consumed the request without returning an original R
    /// selector. Do not repeat the claim or infer a release from this DTO.
    AdmissionUnknown,
    Running(crate::vodserve::StaticArtifactRun),
    Retained(Arc<crate::vodserve::retained::RetainedVodArtifact>),
    PublishedPendingExposure(Arc<crate::vodserve::retained::RetainedVodArtifact>),
    Closing(FiniteCloseTicket),
    Exposed,
    Closed,
}

pub(super) struct StaticRunGuard(Arc<StaticArtifactEntry>);

impl Drop for StaticRunGuard {
    fn drop(&mut self) {
        self.0.active.store(false, Release);
    }
}

impl StaticArtifactEntry {
    pub(super) fn begin(self: &Arc<Self>) -> Option<StaticRunGuard> {
        self.active
            .compare_exchange(false, true, AcqRel, Acquire)
            .ok()
            .map(|_| StaticRunGuard(Arc::clone(self)))
    }

    pub(super) fn matches(
        &self,
        token: &JobToken,
        intent: &EncodedOutputIntent,
        policy_generation: &str,
        file_id: i64,
        budget: u64,
    ) -> bool {
        self.original.same_claim_as(token)
            && self.intent == *intent
            && self.policy_generation == policy_generation
            && self.file_id == file_id
            && self.budget == budget
    }
}

impl StaticArtifactRuns {
    pub(super) fn reserve(
        &self,
        token: JobToken,
        intent: EncodedOutputIntent,
        policy_generation: String,
        file_id: i64,
        budget: u64,
    ) -> Result<Arc<StaticArtifactEntry>, &'static str> {
        let mut slot = self
            .slot
            .lock()
            .map_err(|_| "static owner registry is uncertain")?;
        if let Some(existing) = slot.as_ref() {
            return existing
                .matches(&token, &intent, &policy_generation, file_id, budget)
                .then(|| Arc::clone(existing))
                .ok_or("another static claim retains the slot");
        }
        let entry = Arc::new(StaticArtifactEntry {
            original: token,
            intent,
            policy_generation,
            file_id,
            budget,
            planning: Mutex::new(StaticPlanning::Empty),
            report: Mutex::new(None),
            finite: Mutex::new(StaticFinite::Empty),
            active: AtomicBool::new(false),
            planning_lease: std::sync::Mutex::new(None),
        });
        *slot = Some(Arc::clone(&entry));
        Ok(entry)
    }

    pub(super) fn current(&self) -> Option<Arc<StaticArtifactEntry>> {
        self.slot.lock().ok()?.as_ref().map(Arc::clone)
    }

    pub(super) fn remove(&self, entry: &Arc<StaticArtifactEntry>) {
        if let Ok(mut slot) = self.slot.lock() {
            if slot
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, entry))
            {
                *slot = None;
            }
        }
    }
}

/// Rebuild only the selected software-candidate binding from the immutable
/// Store snapshot. The legacy quality-catalog resolver may open a source and
/// capture an executable even for an Encode candidate; it is not called in
/// this admitted path. The later fixed-P recipe digest and full intent checks
/// reject any current plan that differs from this saved candidate.
pub(super) fn restore_selected_candidate(
    request: &SessionRequest,
    catalog_value: &serde_json::Value,
    snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
    expected: (plurx_core::playback::candidate::CandidateId, [u8; 32]),
    node: &str,
) -> Result<Box<CandidateExecutionContext>, &'static str> {
    let catalog: crate::media_sessions::CandidateCatalogContext =
        serde_json::from_value(catalog_value.clone())
            .map_err(|_| "static candidate catalog is malformed")?;
    let selected = &catalog.candidate;
    if catalog.binding != crate::media_pool::PlanningBinding::from_snapshot(snapshot)
        || selected.id != expected.0
        || selected.recipe_digest != expected.1
        || !selected.identity_matches()
        || selected.route != plurx_core::playback::candidate::CandidateRoute::Encode
        || selected.grade != OutputGrade::Sdr
        || selected.processed_hdr10
        || !selected.normalized_geometry
        || !matches!(request.kind, SessionKind::Transcode { height }
            if height == i64::from(selected.target_height))
        || request.hdr10
        || request.file_id != snapshot.file.id
    {
        return Err("static candidate differs from its selected Store snapshot");
    }
    let query = crate::media_pool::QualityCatalogRequest {
        file_id: request.file_id,
        source_size: snapshot.file.size,
        source_mtime: snapshot.file.mtime,
        caps: catalog.caps.clone(),
        audio_index: request.audio_index,
        audio_offset_ms: request.audio_offset_ms,
        audio_claim: request.audio_claim.clone(),
        audio_delivery: request.audio_delivery.clone(),
        subtitle_burn: request.subtitle_burn,
        presentation: request.presentation,
        copy_contract: None,
    };
    query
        .validate()
        .map_err(|_| "static candidate capability query changed")?;
    let mut context = TranscodeManager::candidate_context(selected);
    context.canonical_caps = Some(catalog.caps);
    context.planning_binding = Some(catalog.binding);
    context.planning_snapshot = Some(Arc::new(snapshot.clone()));
    context.owner_node_id = Some(node.to_owned());
    Ok(Box::new(context))
}

fn planning_application_bytes(
    request: &SessionRequest,
    snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
    entry: &StaticArtifactEntry,
    frame_bytes: usize,
) -> Option<usize> {
    let token = &entry.original;
    let intent = &entry.intent;
    let mut bytes = std::mem::size_of::<StaticArtifactEntry>()
        .checked_add(2 * std::mem::size_of::<usize>())? // Arc entry header
        .checked_add(frame_bytes)?
        .checked_add(request.selected_finite_requested_heap_bytes()?)?
        .checked_add(super::adaptive_attempts::planning_snapshot_requested_heap_bytes(snapshot)?)?
        .checked_add(super::adaptive_attempts::PLANNING_TO_R_HANDOFF_BYTES)?;
    for capacity in [
        token.job_id.capacity(),
        token.node_id.capacity(),
        token.boot_id.capacity(),
        token.claim_id.capacity(),
        intent.target_node_id.capacity(),
        intent.plan_digest.capacity(),
        intent.executable_digest.capacity(),
        intent.engine_digest.capacity(),
        intent.subtitle_digest.as_ref().map_or(0, String::capacity),
        entry.policy_generation.capacity(),
    ] {
        bytes = bytes.checked_add(capacity)?;
    }
    bytes = bytes
        .checked_add(intent.audio_claim.requested_heap_bytes()?)?
        .checked_add(intent.audio_delivery.requested_heap_bytes()?)?;
    Some(bytes)
}

impl TranscodeManager {
    /// Existing manager reap cadence, never a new timer. The core book keeps
    /// every original P/R job and durable mutation when this bounded pass
    /// times out; the fixed entry remains until its R selector closes.
    pub(super) async fn reconcile_static_artifact_run(&self) {
        let Some(entry) = self.static_artifact_runs.current() else {
            return;
        };
        let Some(_active) = entry.begin() else { return };
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut finite = entry.finite.lock().await;
        match &mut *finite {
            StaticFinite::Planned(_) => {
                *finite = StaticFinite::Closed;
            }
            StaticFinite::Running(run) => {
                let _ = run.request_stop();
                if let Ok(ticket) = run.prepare_cancel(deadline).await {
                    let old = std::mem::replace(&mut *finite, StaticFinite::Closing(ticket));
                    drop(old); // rich R owner/lease precede compact final settlement
                }
            }
            StaticFinite::Closing(ticket) => {
                if ticket.finish_retirement(deadline).await.is_ok() {
                    *finite = StaticFinite::Closed;
                }
            }
            StaticFinite::Retained(artifact) => {
                if matches!(
                    self.vod
                        .reconcile_static_artifact_store(artifact, &entry.original, &entry.intent,)
                        .await,
                    Ok(Some(true))
                ) {
                    let retained = Arc::clone(artifact);
                    *finite = StaticFinite::PublishedPendingExposure(retained);
                }
            }
            StaticFinite::PublishedPendingExposure(artifact) => {
                if matches!(
                    self.vod
                        .expose_published_static_artifact(
                            Arc::clone(artifact),
                            &entry.intent,
                            entry.file_id,
                            &entry.policy_generation,
                            entry.budget,
                        )
                        .await,
                    Ok(true)
                ) {
                    *finite = StaticFinite::Exposed;
                }
            }
            StaticFinite::Empty
            | StaticFinite::AdmissionUnknown
            | StaticFinite::Exposed
            | StaticFinite::Closed => {}
        }
        if matches!(&*finite, StaticFinite::Exposed) {
            drop(finite);
            self.static_artifact_runs.remove(&entry);
            return;
        }
        if !matches!(&*finite, StaticFinite::Empty | StaticFinite::Closed) {
            return;
        }
        drop(finite);
        *entry.report.lock().await = None;
        let mut planning = entry.planning.lock().await;
        if let StaticPlanning::Original(_) = &*planning {
            let old = std::mem::replace(&mut *planning, StaticPlanning::Closed);
            if let StaticPlanning::Original(owner) = old {
                *planning = StaticPlanning::Closing(owner.into_cleanup());
            }
        }
        // The original core book, not this selector, retains native P jobs
        // and its cleanup flag. Removing the rich entry drops its last M
        // application lease after all of its own payload fields.
        drop(planning);
        self.static_artifact_runs.remove(&entry);
    }

    /// Selected complete software output. The fixed registry entry is visible
    /// before any P native await, and every later phase resumes that same
    /// original P/R/S owner after a dropped waiter.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn produce_selected_static_job(
        &self,
        request: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
        job: &plurx_core::store::background_jobs::BackgroundJob,
        fence: &crate::background_jobs::JobFence,
        intent: &EncodedOutputIntent,
        policy_generation: &str,
        source_object_version: &str,
        scratch_bytes: i64,
        deadline: Instant,
        frame_bytes: usize,
    ) -> Result<bool, crate::background_jobs::PreparationError> {
        use crate::background_jobs::PreparationError;
        let token = job
            .token
            .as_ref()
            .ok_or(PreparationError::Fail("static_claim_token_missing"))?;
        let budget = u64::try_from(scratch_bytes)
            .map_err(|_| PreparationError::Fail("encoded_cap_invalid"))?;
        let entry = self
            .static_artifact_runs
            .reserve(
                token.clone(),
                intent.clone(),
                policy_generation.to_owned(),
                file.id,
                budget,
            )
            .map_err(PreparationError::Yield)?;
        let _active = entry
            .begin()
            .ok_or(PreparationError::Yield("static_owner_busy"))?;
        let runtime = self
            .resource_runtime
            .as_ref()
            .ok_or(PreparationError::Fail("static_runtime_unavailable"))?;

        if matches!(&*entry.finite.lock().await, StaticFinite::Empty) {
            let snapshot = self
                .store
                .playback_planning_snapshot(file.id, &super::QUALITY_PLANNING_KEYS)
                .await
                .map_err(|error| PreparationError::Retry(error.to_string()))?
                .ok_or(PreparationError::Stop("source_changed"))?;
            if snapshot.file.id != file.id
                || snapshot.file.size != file.size
                || snapshot.file.mtime != file.mtime
            {
                return Err(PreparationError::Stop("source_changed"));
            }
            let app_bytes = planning_application_bytes(request, &snapshot, &entry, frame_bytes)
                .ok_or(PreparationError::Fail("static_planning_capacity_overflow"))?;
            runtime
                .refresh_admission_capacity(deadline)
                .await
                .map_err(|error| {
                    PreparationError::Retry(format!("observing static P capacity: {error:?}"))
                })?;
            let mut planning = entry.planning.lock().await;
            if matches!(&*planning, StaticPlanning::Empty) {
                let probe = super::adaptive_attempts::fixed_planning_request(
                    self,
                    &snapshot,
                    source_object_version,
                    app_bytes,
                )
                .map_err(PreparationError::Retry)?;
                let owner = runtime
                    .prepare_fixed_planning_probe(probe, deadline)
                    .map_err(|error| PreparationError::Retry(error.to_string()))?;
                *planning = StaticPlanning::Original(owner);
            }
            let StaticPlanning::Original(owner) = &mut *planning else {
                return Err(PreparationError::Yield("static_planning_closing"));
            };
            {
                let mut lease = entry
                    .planning_lease
                    .lock()
                    .map_err(|_| PreparationError::Yield("static_planning_lease_uncertain"))?;
                if lease.is_none() {
                    *lease = Some(
                        owner
                            .take_application_lease(app_bytes)
                            .map_err(|error| PreparationError::Retry(error.to_string()))?,
                    );
                }
            }
            owner
                .prepare_inputs(deadline)
                .await
                .map_err(|error| PreparationError::Retry(error.to_string()))?;
            let report = if let Some(report) = entry.report.lock().await.as_ref().cloned() {
                report
            } else {
                let report = owner
                    .collect_and_refine(deadline)
                    .await
                    .map_err(|error| PreparationError::Retry(error.to_string()))?;
                *entry.report.lock().await = Some(report.clone());
                report
            };
            if report
                .application_limit_bytes()
                .and_then(|n| usize::try_from(n).ok())
                .is_none_or(|limit| limit < app_bytes)
                || report.source_object_version() != source_object_version
            {
                return Err(PreparationError::Stop("static_source_changed"));
            }
            let settings = self
                .vod_settings(request)
                .await
                .map_err(PreparationError::Retry)?
                .ok_or(PreparationError::Fail("vod_policy_unavailable"))?;
            let encoding = self
                .resolve_adaptive_encoding_from_probe(request, file, &snapshot, report)
                .await
                .map_err(PreparationError::Retry)?;
            if encoding.source_object_version != source_object_version
                || self
                    .encoded_output_intent(request, &encoding, &intent.target_node_id)
                    .map_err(PreparationError::Retry)?
                    != *intent
            {
                return Err(PreparationError::Stop("encoded_delivery_changed"));
            }
            let mut recipe = crate::vodserve::VodRecipeRequest {
                root_admission: None,
                soundtrack: None,
                companion: None,
                request,
                encoding: Some(encoding),
                retained_capture: crate::vodserve::RetainedOutputCapture::New,
                measured_candidate: None,
            };
            let prepared = self
                .vod
                .resolve_adaptive_recipe(&mut recipe, file, &settings)
                .await
                .map_err(PreparationError::Retry)?;
            let planned = self
                .vod
                .plan_static_artifact(
                    prepared,
                    std::mem::size_of::<StaticArtifactEntry>(),
                    super::adaptive_attempts::PLANNING_TO_R_HANDOFF_BYTES,
                    deadline,
                )
                .await
                .map_err(PreparationError::Retry)?;
            *entry.finite.lock().await = StaticFinite::Planned(planned);
        }

        {
            let mut planning = entry.planning.lock().await;
            if let StaticPlanning::Original(owner) = &mut *planning {
                owner
                    .finish_native(deadline)
                    .await
                    .map_err(|error| PreparationError::Retry(error.to_string()))?;
                let old = std::mem::replace(&mut *planning, StaticPlanning::Closed);
                if let StaticPlanning::Original(owner) = old {
                    *planning = StaticPlanning::Closing(owner.into_cleanup());
                }
            }
        }
        {
            let mut finite = entry.finite.lock().await;
            if matches!(&*finite, StaticFinite::Planned(_)) {
                let StaticFinite::Planned(planned) =
                    std::mem::replace(&mut *finite, StaticFinite::Empty)
                else {
                    unreachable!()
                };
                match self.vod.admit_static_artifact(planned, deadline) {
                    Ok(run) => *finite = StaticFinite::Running(run),
                    Err(error) => {
                        *finite = StaticFinite::AdmissionUnknown;
                        return Err(PreparationError::Retry(error));
                    }
                }
                *entry.report.lock().await = None;
            }
            if let StaticFinite::Running(run) = &mut *finite {
                let artifact = Arc::new(
                    run.complete(deadline)
                        .await
                        .map_err(|error| PreparationError::Retry(error.to_string()))?,
                );
                *finite = StaticFinite::Retained(artifact);
            }
        }
        let artifact = match &*entry.finite.lock().await {
            StaticFinite::Retained(artifact) | StaticFinite::PublishedPendingExposure(artifact) => {
                Arc::clone(artifact)
            }
            StaticFinite::Exposed => return Ok(true),
            _ => return Err(PreparationError::Yield("static_owner_not_publishable")),
        };
        if matches!(&*entry.finite.lock().await, StaticFinite::Retained(_)) {
            match self
                .vod
                .reconcile_static_artifact_store(&artifact, &entry.original, &entry.intent)
                .await
                .map_err(PreparationError::Retry)?
            {
                Some(true) => {}
                Some(false) => {
                    return Err(PreparationError::Retry(
                        "original static publication has a nonpositive verdict".into(),
                    ))
                }
                None => {
                    if !self
                        .vod
                        .commit_static_artifact_store(&artifact, fence, &entry.intent)
                        .await
                        .map_err(PreparationError::Retry)?
                    {
                        return Ok(false);
                    }
                }
            }
            *entry.finite.lock().await =
                StaticFinite::PublishedPendingExposure(Arc::clone(&artifact));
        }
        if !self
            .vod
            .expose_published_static_artifact(
                artifact,
                &entry.intent,
                entry.file_id,
                &entry.policy_generation,
                entry.budget,
            )
            .await
            .map_err(PreparationError::Retry)?
        {
            return Ok(false);
        }
        *entry.finite.lock().await = StaticFinite::Exposed;
        self.static_artifact_runs.remove(&entry);
        Ok(true)
    }
}
