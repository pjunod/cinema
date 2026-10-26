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
    // Pins the original saved catalog, source version and complete carrier.
    // A renewal may update its token, never replace these admission inputs.
    pub(super) payload_digest: [u8; 32],
    pub(super) controller: crate::background_jobs::JobFence,
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
    Closing { _cleanup: FixedPlanningProbeCleanup },
    Closed,
}

pub(super) enum StaticFinite {
    Empty,
    Planned(crate::vodserve::StaticArtifactAdmission),
    /// Core admission consumed the request without returning an original R
    /// selector. Do not repeat the claim or infer a release from this DTO.
    AdmissionUnknown {
        _cache_grant: Option<crate::playback_cache::FiniteCacheReservation>,
    },
    Running(crate::vodserve::StaticArtifactRun),
    Retained(Arc<crate::vodserve::retained::RetainedVodArtifact>),
    PublishedPendingExposure(Arc<crate::vodserve::retained::RetainedVodArtifact>),
    /// A definite logical refusal is sticky. Only original typed S cleanup
    /// may turn this into Closed; a later setting change cannot re-expose it.
    Refused(Arc<crate::vodserve::retained::RetainedVodArtifact>),
    Closing {
        ticket: FiniteCloseTicket,
        cache_grant: Option<crate::playback_cache::FiniteCacheReservation>,
    },
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
        payload_digest: &[u8; 32],
        intent: &EncodedOutputIntent,
        policy_generation: &str,
        file_id: i64,
        budget: u64,
    ) -> bool {
        self.original.same_claim_as(token)
            && self.payload_digest == *payload_digest
            && self.intent == *intent
            && self.policy_generation == policy_generation
            && self.file_id == file_id
            && self.budget == budget
    }
}

impl StaticArtifactRuns {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn reserve(
        &self,
        token: JobToken,
        payload_digest: [u8; 32],
        controller: crate::background_jobs::JobFence,
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
            if !existing.controller.same_controller_as(&controller) {
                return Err("another original static controller retains the slot");
            }
            return existing
                .matches(
                    &token,
                    &payload_digest,
                    &intent,
                    &policy_generation,
                    file_id,
                    budget,
                )
                .then(|| Arc::clone(existing))
                .ok_or("another static claim retains the slot");
        }
        let entry = Arc::new(StaticArtifactEntry {
            original: token,
            payload_digest,
            controller,
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

/// Bind the entire saved carrier without allocating a second JSON buffer.
/// serde_json's pinned object map emits stable key order; this digest is only
/// an equality fence for the original DTO, never native closure authority.
fn static_payload_digest(value: &serde_json::Value) -> Result<[u8; 32], &'static str> {
    use sha2::Digest as _;
    struct Writer(sha2::Sha256);
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer(sha2::Sha256::new());
    serde_json::to_writer(&mut writer, value).map_err(|_| "static_payload_unserializable")?;
    Ok(writer.0.finalize().into())
}

/// Rebuild only the selected software-candidate binding from the immutable
/// Store snapshot. The legacy quality-catalog resolver may open a source and
/// capture an executable even for an Encode candidate; it is not called in
/// this admitted path. The later fixed-P recipe digest and full intent checks
/// reject any current plan that differs from this saved candidate.
pub(super) fn restore_selected_candidate(
    request: &SessionRequest,
    catalog: crate::media_sessions::CandidateCatalogContext,
    snapshot: Arc<plurx_core::store::PlaybackPlanningSnapshot>,
    expected: (plurx_core::playback::candidate::CandidateId, [u8; 32]),
    node: &str,
) -> Result<Box<CandidateExecutionContext>, &'static str> {
    let selected = &catalog.candidate;
    if catalog.binding != crate::media_pool::PlanningBinding::from_snapshot(&snapshot)
        || selected.id != expected.0
        || selected.recipe_digest != expected.1
        || !selected.identity_matches()
        || selected.route != plurx_core::playback::candidate::CandidateRoute::Encode
        || selected.grade != OutputGrade::Sdr
        || selected.processed_dv
        || !selected.decoder_compatible
        || selected
            .planned_codec
            .as_deref()
            .is_some_and(|codec| codec != "h264")
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
        caps: catalog.caps,
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
    context.canonical_caps = Some(query.caps);
    context.planning_binding = Some(catalog.binding);
    context.planning_snapshot = Some(snapshot);
    context.owner_node_id = Some(node.to_owned());
    Ok(Box::new(context))
}

/// Configured roots grant only a lexical source scope. Actual no-follow
/// descriptor acquisition remains inside the already admitted P operation.
fn static_source_in_roots(path: &std::path::Path, roots: &[std::path::PathBuf]) -> bool {
    use std::path::Component;
    path.is_absolute()
        && roots.iter().any(|root| {
            root.is_absolute()
                && root
                    .components()
                    .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
                && path.strip_prefix(root).is_ok_and(|relative| {
                    !relative.as_os_str().is_empty()
                        && relative
                            .components()
                            .all(|component| matches!(component, Component::Normal(_)))
                })
        })
}

#[allow(clippy::too_many_arguments)]
fn planning_application_bytes(
    request: &SessionRequest,
    snapshot: &Arc<plurx_core::store::PlaybackPlanningSnapshot>,
    entry: &StaticArtifactEntry,
    job: &plurx_core::store::background_jobs::BackgroundJob,
    catalog_value: Option<&serde_json::Value>,
    retained_intent: &EncodedOutputIntent,
    retained_policy: &String,
    retained_source: &String,
    frame_bytes: usize,
) -> Option<usize> {
    let plurx_core::store::background_jobs::BackgroundJob {
        id: _,
        payload_version: _,
        payload: _,
        dedupe_key: _,
        priority: _,
        state: _,
        token: _,
        fence: _,
        revision: _,
        failed_attempts: _,
        attempt_limit: _,
        retry_deadline_ms: _,
        attempt_errors: _,
        index_diagnostic_json: _,
        yield_count: _,
        abandoned_count: _,
        not_before_ms: _,
        checkpoint: _,
        result_ref: _,
        last_error_code: _,
        created_at_ms: _,
        updated_at_ms: _,
    } = job;
    let token = &entry.original;
    let intent = &entry.intent;
    let mut bytes = std::mem::size_of::<StaticArtifactEntry>()
        .checked_add(2 * std::mem::size_of::<usize>())? // Arc entry header
        .checked_add(frame_bytes)?
        .checked_add(std::mem::size_of::<SessionRequest>())?
        .checked_add(request.selected_finite_requested_heap_bytes()?)?
        .checked_add(super::adaptive_attempts::PLANNING_TO_R_HANDOFF_BYTES)?;
    if request.candidate_context.as_ref().is_none_or(|context| {
        context
            .planning_snapshot
            .as_ref()
            .is_none_or(|held| !Arc::ptr_eq(held, snapshot))
    }) {
        bytes = bytes
            .checked_add(
                std::mem::size_of::<plurx_core::store::PlaybackPlanningSnapshot>()
                    + 2 * std::mem::size_of::<usize>(),
            )?
            .checked_add(
                super::adaptive_attempts::planning_snapshot_requested_heap_bytes(snapshot)?,
            )?;
    }
    // The claimed job and its separately decoded catalog stay live in the
    // caller while this original coordinator awaits native work.
    bytes = bytes
        .checked_add(crate::background_jobs::BackgroundJobController::frame_bytes())?
        .checked_add(crate::media_sessions::selected_finite_json_heap_bytes(
            &job.payload,
        )?)?
        .checked_add(retained_policy.capacity())?
        .checked_add(retained_source.capacity())?;
    for value in [
        &retained_intent.target_node_id,
        &retained_intent.plan_digest,
        &retained_intent.executable_digest,
        &retained_intent.engine_digest,
    ] {
        bytes = bytes.checked_add(value.capacity())?;
    }
    bytes = bytes
        .checked_add(
            retained_intent
                .subtitle_digest
                .as_ref()
                .map_or(0, String::capacity),
        )?
        .checked_add(retained_intent.audio_claim.requested_heap_bytes()?)?
        .checked_add(retained_intent.audio_delivery.requested_heap_bytes()?)?;
    if let Some(value) = catalog_value {
        bytes = bytes.checked_add(crate::media_sessions::selected_finite_json_heap_bytes(
            value,
        )?)?;
    }
    if let Some(value) = &job.checkpoint {
        bytes = bytes.checked_add(crate::media_sessions::selected_finite_json_heap_bytes(
            value,
        )?)?;
    }
    for capacity in [
        job.id.capacity(),
        job.dedupe_key.capacity(),
        job.attempt_errors.capacity(),
        job.index_diagnostic_json.capacity(),
        job.result_ref.as_ref().map_or(0, String::capacity),
        job.last_error_code.as_ref().map_or(0, String::capacity),
    ] {
        bytes = bytes.checked_add(capacity)?;
    }
    if let Some(token) = &job.token {
        for value in [
            &token.job_id,
            &token.node_id,
            &token.boot_id,
            &token.claim_id,
        ] {
            bytes = bytes.checked_add(value.capacity())?;
        }
    }
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
    /// Pure shared branch selection. Once selected, every refusal remains in
    /// the same typed owner; it cannot trigger a fresh legacy preparation.
    pub(crate) fn uses_selected_static_intent(
        &self,
        file: &plurx_core::domain::MediaFile,
        intent: &EncodedOutputIntent,
    ) -> bool {
        self.resource_runtime.is_some()
            && intent.copy_for_burn.is_none()
            && intent.subtitle_burn.is_none()
            && !intent.hdr10_requested
            && intent.grade == OutputGrade::Sdr
            && file.dolby_vision.is_empty()
    }

    pub(crate) fn selected_planning_snapshot_heap_bytes(
        snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
    ) -> Option<usize> {
        let plurx_core::store::PlaybackPlanningSnapshot {
            file: _,
            probe_json: _,
            settings: _,
            capacity: _,
            generation: _,
            item_title: _,
        } = snapshot;
        super::adaptive_attempts::planning_snapshot_requested_heap_bytes(snapshot)
    }

    async fn static_source_configured(
        &self,
        file: &plurx_core::domain::MediaFile,
    ) -> Result<bool, String> {
        let Some(item) = self
            .store
            .get_item(file.item_id)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        let Some(library) = self
            .store
            .get_library(item.library_id)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        Ok(static_source_in_roots(&file.path, &library.paths))
    }

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
                    let cache_grant = run.take_cache_grant();
                    let old = std::mem::replace(
                        &mut *finite,
                        StaticFinite::Closing {
                            ticket,
                            cache_grant,
                        },
                    );
                    drop(old); // rich R owner/lease precede compact final settlement
                }
            }
            StaticFinite::Closing {
                ticket,
                cache_grant,
            } => {
                if ticket.finish_retirement(deadline).await.is_ok() {
                    if let Some(grant) = cache_grant.take() {
                        grant.known_closed();
                    }
                    *finite = StaticFinite::Closed;
                }
            }
            StaticFinite::Retained(artifact) => {
                match self
                    .vod
                    .reconcile_static_artifact_store(artifact, &entry.original, &entry.intent)
                    .await
                {
                    Ok(Some(true)) => {
                        *finite = StaticFinite::PublishedPendingExposure(Arc::clone(artifact))
                    }
                    Ok(Some(false)) => *finite = StaticFinite::Refused(Arc::clone(artifact)),
                    Ok(None) | Err(_) => {}
                }
            }
            StaticFinite::PublishedPendingExposure(artifact) => {
                match self
                    .vod
                    .expose_published_static_artifact(
                        Arc::clone(artifact),
                        &entry.intent,
                        entry.file_id,
                        &entry.policy_generation,
                        entry.budget,
                    )
                    .await
                {
                    Ok(crate::vodserve::StaticArtifactExposure::Exposed) => {
                        *finite = StaticFinite::Exposed
                    }
                    Ok(crate::vodserve::StaticArtifactExposure::Obsolete) => {
                        *finite = StaticFinite::Refused(Arc::clone(artifact))
                    }
                    Ok(crate::vodserve::StaticArtifactExposure::Retry) | Err(_) => {}
                }
            }
            StaticFinite::Refused(artifact) => {
                if let Ok(ticket) = self
                    .vod
                    .prepare_static_artifact_eviction(artifact, deadline)
                    .await
                {
                    let old = std::mem::replace(
                        &mut *finite,
                        StaticFinite::Closing {
                            ticket,
                            cache_grant: None,
                        },
                    );
                    drop(old); // original rich S aliases must precede its final refund
                }
            }
            StaticFinite::Empty
            | StaticFinite::AdmissionUnknown { .. }
            | StaticFinite::Exposed
            | StaticFinite::Closed => {}
        }
        if !matches!(
            &*finite,
            StaticFinite::Empty | StaticFinite::Closed | StaticFinite::Exposed
        ) {
            return;
        }
        drop(finite);
        *entry.report.lock().await = None;
        let mut planning = entry.planning.lock().await;
        if let StaticPlanning::Original(_) = &*planning {
            let old = std::mem::replace(&mut *planning, StaticPlanning::Closed);
            if let StaticPlanning::Original(owner) = old {
                *planning = StaticPlanning::Closing {
                    _cleanup: owner.into_cleanup(),
                };
            }
        }
        drop(planning);
        // Rich job/control payload destruction is a separate fact from the
        // original heartbeat join and logical Store Close. A cancelled waiter
        // leaves its original handle in this retained controller; no missing
        // fence, timeout or pruned row can authorize M's final drop.
        if !entry.controller.outer_payloads_dropped() {
            return;
        }
        let receipt = match tokio::time::timeout_at(
            deadline.into(),
            entry.controller.finish_original_controller(),
        )
        .await
        {
            Ok(receipt) => receipt,
            Err(_) => return,
        };
        if !receipt.closes_original(&entry.original) {
            return;
        }
        // The original core book still requires positive native P closure.
        // The entry's own fields/controller disappear before its M lease.
        self.static_artifact_runs.remove(&entry);
    }

    /// Selected complete software output. The fixed registry entry is visible
    /// before any P native await, and every later phase resumes that same
    /// original P/R/S owner after a dropped waiter.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn produce_selected_static_job(
        &self,
        request: &mut SessionRequest,
        candidate_catalog: Option<&serde_json::Value>,
        file: &plurx_core::domain::MediaFile,
        job: &plurx_core::store::background_jobs::BackgroundJob,
        fence: &crate::background_jobs::JobFence,
        intent: &EncodedOutputIntent,
        policy_generation: &String,
        source_object_version: &String,
        scratch_bytes: i64,
        deadline: Instant,
        frame_bytes: usize,
    ) -> Result<bool, crate::background_jobs::PreparationError> {
        use crate::background_jobs::PreparationError;
        let token = job
            .token
            .as_ref()
            .ok_or(PreparationError::Fail("static_claim_token_missing"))?;
        if fence.snapshot_for_claim(token).await.is_none() {
            return Err(PreparationError::Stop("ownership_lost"));
        }
        let budget = u64::try_from(scratch_bytes)
            .map_err(|_| PreparationError::Fail("encoded_cap_invalid"))?;
        let entry = self
            .static_artifact_runs
            .reserve(
                token.clone(),
                static_payload_digest(&job.payload).map_err(PreparationError::Fail)?,
                fence.clone(),
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
            if !self
                .static_source_configured(file)
                .await
                .map_err(PreparationError::Retry)?
            {
                return Err(PreparationError::Stop(
                    "static_source_outside_configured_roots",
                ));
            }
            let snapshot = Arc::new(
                self.store
                    .playback_planning_snapshot(file.id, &super::QUALITY_PLANNING_KEYS)
                    .await
                    .map_err(|error| PreparationError::Retry(error.to_string()))?
                    .ok_or(PreparationError::Stop("source_changed"))?,
            );
            if snapshot.file.id != file.id
                || snapshot.file.size != file.size
                || snapshot.file.mtime != file.mtime
            {
                return Err(PreparationError::Stop("source_changed"));
            }
            if let Some(expected) = intent.candidate_id.zip(intent.candidate_digest) {
                let value =
                    candidate_catalog.ok_or(PreparationError::Stop("static_candidate_missing"))?;
                let catalog = serde::Deserialize::deserialize(value)
                    .map_err(|_| PreparationError::Stop("static_candidate_malformed"))?;
                request.candidate_context = Some(
                    restore_selected_candidate(
                        request,
                        catalog,
                        Arc::clone(&snapshot),
                        expected,
                        &intent.target_node_id,
                    )
                    .map_err(|_| PreparationError::Stop("static_candidate_changed"))?,
                );
            } else if candidate_catalog.is_some() {
                return Err(PreparationError::Stop("static_candidate_unexpected"));
            }
            let controller_bytes = entry
                .controller
                .controller_requested_heap_bytes()
                .await
                .ok_or(PreparationError::Fail(
                    "static_controller_capacity_unavailable",
                ))?;
            let app_bytes = planning_application_bytes(
                request,
                &snapshot,
                &entry,
                job,
                candidate_catalog,
                intent,
                policy_generation,
                source_object_version,
                frame_bytes,
            )
            .and_then(|bytes| bytes.checked_add(controller_bytes))
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
            if fence.snapshot_for_claim(&entry.original).await.is_none() {
                return Err(PreparationError::Stop("ownership_lost"));
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
            let compatibility_report = report.clone();
            let encoding = self
                .resolve_adaptive_encoding_from_probe(request, file, &snapshot, report)
                .await
                .map_err(PreparationError::Retry)?;
            if request.candidate_context.as_ref().is_some_and(|context| {
                context.canonical_caps.as_ref().is_none_or(|caps| {
                    !super::manager_candidates::encoded_client_compatible(
                        caps,
                        &encoding.plan,
                        compatibility_report.facts(),
                    )
                })
            }) {
                return Err(PreparationError::Stop("static_candidate_decoder_changed"));
            }
            if encoding.source_object_version.as_str() != source_object_version.as_str()
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
                measured_candidate: request.candidate_context.as_ref().map(|context| {
                    crate::vodserve::RetainedCandidateBinding {
                        kind: request.kind,
                        normalized_geometry: context.normalized_geometry,
                        profile: context.profile,
                        candidate_id: context.candidate_id,
                        recipe_digest: context.recipe_digest,
                        file_id: request.file_id,
                        audio_index: request.audio_index,
                        audio_offset_ms: request.audio_offset_ms,
                        subtitle_burn: request.subtitle_burn,
                        grade: context.grade,
                        route: plurx_core::playback::candidate::CandidateRoute::Encode,
                    }
                }),
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
                    crate::vodserve::StaticArtifactPolicy::from_snapshot(&snapshot)
                        .ok_or(PreparationError::Stop("static_policy_snapshot_overbound"))?,
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
                    *planning = StaticPlanning::Closing {
                        _cleanup: owner.into_cleanup(),
                    };
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
                        let (message, definitely_unreserved, cache_grant) = error.into_parts();
                        // Only core's exact pre-reservation provenance proves
                        // no original R escaped. Generic admission/rollback
                        // errors remain unknown; never replay this request.
                        *finite = if definitely_unreserved {
                            StaticFinite::Closed
                        } else {
                            StaticFinite::AdmissionUnknown {
                                _cache_grant: cache_grant,
                            }
                        };
                        return Err(PreparationError::Retry(message));
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
                    *entry.finite.lock().await = StaticFinite::Refused(Arc::clone(&artifact));
                    return Err(PreparationError::Retry(
                        "original static publication has a nonpositive verdict".into(),
                    ));
                }
                None => {
                    if !self
                        .vod
                        .commit_static_artifact_store(&artifact, fence, &entry.intent)
                        .await
                        .map_err(PreparationError::Retry)?
                    {
                        *entry.finite.lock().await = StaticFinite::Refused(Arc::clone(&artifact));
                        return Ok(false);
                    }
                }
            }
            *entry.finite.lock().await =
                StaticFinite::PublishedPendingExposure(Arc::clone(&artifact));
        }
        match self
            .vod
            .expose_published_static_artifact(
                Arc::clone(&artifact),
                &entry.intent,
                entry.file_id,
                &entry.policy_generation,
                entry.budget,
            )
            .await
            .map_err(PreparationError::Retry)?
        {
            crate::vodserve::StaticArtifactExposure::Exposed => {
                *entry.finite.lock().await = StaticFinite::Exposed
            }
            crate::vodserve::StaticArtifactExposure::Obsolete => {
                *entry.finite.lock().await = StaticFinite::Refused(artifact);
                return Ok(false);
            }
            crate::vodserve::StaticArtifactExposure::Retry => return Ok(false),
        }
        // Store publication is logical Close, not destruction of the outer
        // worker's job payload or a join of its original heartbeat. Existing
        // reconciliation removes this entry only after all three facts hold.
        Ok(true)
    }
}

#[cfg(test)]
#[path = "background_static/tests.rs"]
#[allow(
    clippy::unwrap_used,
    reason = "Fixture setup and expected-refusal assertions deliberately fail the test on the opposite result; production is checked separately."
)]
mod tests;
