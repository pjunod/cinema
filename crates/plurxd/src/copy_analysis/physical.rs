//! Full-source work is a sequential, exact physical phase of the existing
//! Analysis owner. Metadata/cache hits allocate no Source I/O custody.
use super::*;
use plurx_core::store::analysis_coordination::{
    AnalysisCoordination, AnalysisCoordinationClaim, AnalysisPhysicalPhase,
};
use plurx_core::store::{
    AnalysisRequest, CopyIndexSourceAttestation, FragmentIndexSourceObservation, Store,
};
use tokio_util::sync::CancellationToken;

pub(crate) enum VerifiedFullSource {
    Phase(CopyIndexSourceAttestation),
    /// Full-v1 prefix is mandatory. Sampled v2 observations are never adopted.
    LegacyFull(FragmentIndexSourceObservation),
}
impl VerifiedFullSource {
    pub(super) fn matches(&self, source: &AnalysisCoordinationSource) -> bool {
        match self {
            Self::Phase(value) => {
                value.validate().is_ok()
                    && value.origin_node_id == source.origin_node_id
                    && value.file_id == source.file_id
                    && value.object_version == source.object_version
                    && value.source_size == source.source_size
                    && value.source_mtime == source.source_mtime
            }
            Self::LegacyFull(value) => {
                value.node_id == source.origin_node_id
                    && value.file_id == source.file_id
                    && value.object_version
                        == format!(
                            "{}{}",
                            crate::fragment_index_cluster::FULL_COPY_PREFIX,
                            source.object_version
                        )
                    && value.source_size == source.source_size
                    && value.source_mtime == source.source_mtime
                    && value.observed_at_ms >= 0
                    && valid_copy_digest(&value.source_sha256)
            }
        }
    }
    pub(super) fn sha256(&self) -> &str {
        match self {
            Self::Phase(value) => &value.source_sha256,
            Self::LegacyFull(value) => &value.source_sha256,
        }
    }
}

pub(super) async fn cached_full_source(
    store: &dyn Store,
    source: &AnalysisCoordinationSource,
) -> Result<Option<VerifiedFullSource>, String> {
    if let Some(value) = store
        .copy_index_source_attestation(
            &source.origin_node_id,
            source.file_id,
            &source.object_version,
        )
        .await
        .map_err(|error| error.to_string())?
    {
        let value = VerifiedFullSource::Phase(value);
        if value.matches(source) {
            return Ok(Some(value));
        }
        return Err("full-source receipt differs from the held source".into());
    }
    let full_object = format!(
        "{}{}",
        crate::fragment_index_cluster::FULL_COPY_PREFIX,
        source.object_version
    );
    if let Some(value) = store
        .fragment_index_source(&source.origin_node_id, source.file_id, &full_object)
        .await
        .map_err(|error| error.to_string())?
    {
        let value = VerifiedFullSource::LegacyFull(value);
        if value.matches(source) {
            return Ok(Some(value));
        }
        return Err("legacy full-source receipt differs from the held source".into());
    }
    Ok(None)
}

/// One begun and activated physical phase with its byte budget. The owner
/// reads only through `budget` and must call [`ActivePhase::finish`] after
/// every reader and child it started has joined.
pub(super) struct ActivePhase {
    pub(super) phase: AnalysisPhysicalPhase,
    pub(super) budget: Arc<crate::background_jobs::SourceReadBudget>,
    pub(super) cancel: CancellationToken,
}
impl ActivePhase {
    /// Returns None when the physical source domain is occupied: the store's
    /// capacity clause is the idle test. The coordinator retries through its
    /// existing work notifications; no lease expiry frees a reader and no
    /// independent poll/watchdog is installed here.
    pub(super) async fn begin(
        store: &Arc<dyn Store>,
        source: &AnalysisCoordinationSource,
        claim: AnalysisCoordinationClaim,
        request: &AnalysisRequest,
        lost: &CancellationToken,
    ) -> Result<Option<Self>, String> {
        let role = store
            .analysis_coordination(&claim.request_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("copy coordinator is absent")?;
        if &role.source != source {
            return Err("copy source generation differs".into());
        }
        if role.active_custody_id.is_some() {
            return Ok(None);
        }
        // Retain the complete exact attempt before dispatch. An error is not a
        // negative acknowledgement of this phase's atomic reservation.
        let expected_sequence = role.phase_sequence;
        if expected_sequence < 0 {
            return Err("copy phase sequence is invalid".into());
        }
        let attempted_owner = claim.physical_owner();
        let attempted = AnalysisPhysicalPhase {
            claim,
            source: source.clone(),
            phase_sequence: expected_sequence
                .checked_add(1)
                .ok_or("copy phase sequence exhausted")?,
            custody: plurx_core::store::background_jobs_resources::BackgroundSourceCustodyToken {
                custody_id: uuid::Uuid::new_v4().to_string(),
                owner: attempted_owner,
            },
        };
        let began_at_ms = crate::state::clock_ms();
        let phase = match store
            .begin_analysis_source_phase(
                attempted.claim.clone(),
                source.clone(),
                expected_sequence,
                attempted.custody.custody_id.clone(),
                began_at_ms,
            )
            .await
        {
            Ok(Some(phase)) => phase,
            Ok(None) => return Ok(None),
            Err(error) => {
                // No activation/Input was exposed. Resolve and retire only this
                // UUID/sequence, retaining it across unavailable authoritative reads.
                resolve_unstarted_phase(store, &attempted, request, began_at_ms).await;
                return Err(error.to_string());
            }
        };
        if let Err(error) =
            crate::source_custody::activate_background_with_journal(store, phase.custody.clone())
                .await
        {
            // This operation has not created a budget or exposed an Input. The
            // activation helper resolves its own exact activation-id abort before
            // returning Err; durable release still refuses an armed Input row.
            crate::background_jobs::settle_no_reader_source_custody(store, phase.custody.clone())
                .await;
            finish_exact_phase(store, &phase).await;
            return Err(error);
        }
        let cancel = lost.child_token();
        let budget = crate::background_jobs::SourceReadBudget::from_analysis_phase(
            Arc::clone(store),
            phase.clone(),
            cancel.clone(),
        );
        Ok(Some(Self {
            phase,
            budget,
            cancel,
        }))
    }

    /// Call only after every read, Input bridge and child of this phase joined.
    pub(super) async fn finish(self, store: &Arc<dyn Store>) {
        self.cancel.cancel();
        self.budget.invalidate_grant();
        crate::background_jobs::settle_joined_source_custody(store, self.phase.custody.clone())
            .await;
        finish_exact_phase(store, &self.phase).await;
    }
}

/// Returns None when the physical source domain is occupied.
pub(super) async fn attest_full_source(
    store: Arc<dyn Store>,
    file: Arc<plurx_core::domain::MediaFile>,
    source: AnalysisCoordinationSource,
    claim: AnalysisCoordinationClaim,
    request: AnalysisRequest,
    checkpoint_root: Option<PathBuf>,
    lost: CancellationToken,
) -> Result<Option<VerifiedFullSource>, String> {
    if claim.node_id != source.origin_node_id
        || request.request_id != claim.request_id
        || request.fence != claim.fence
        || request.owner_node_id != claim.node_id
        || file.id != source.file_id
        || file.size != source.source_size
        || file.mtime != source.source_mtime
    {
        return Err("copy attestation owner/source differs".into());
    }
    if let Some(value) = cached_full_source(store.as_ref(), &source).await? {
        return Ok(Some(value));
    }
    let Some(active) = ActivePhase::begin(&store, &source, claim, &request, &lost).await? else {
        return Ok(None);
    };
    let phase = &active.phase;
    // This await is cooperative, not a select that drops an in-flight file read.
    // Cancellation stops the next allowance; every already-issued read joins.
    let result = crate::background_jobs::budgeted(Arc::clone(&active.budget), async {
        let attested = crate::fragment_index_cluster::attest_coordination_source_full_persistent(
            &source.origin_node_id,
            &file,
            &|_| {},
            checkpoint_root.as_deref(),
            store.as_ref(),
            &request,
        )
        .await?;
        let expected = format!(
            "{}{}",
            crate::fragment_index_cluster::FULL_COPY_PREFIX,
            source.object_version
        );
        if attested.observation.object_version != expected
            || attested.observation.source_size != source.source_size
            || attested.observation.source_mtime != source.source_mtime
        {
            return Err("source moved during complete-byte attestation".into());
        }
        let receipt = CopyIndexSourceAttestation {
            origin_node_id: source.origin_node_id.clone(),
            file_id: source.file_id,
            object_version: source.object_version.clone(),
            source_size: source.source_size,
            source_mtime: source.source_mtime,
            source_sha256: attested.observation.source_sha256.clone(),
            regime_revision: 1,
            completed_at_ms: crate::state::clock_ms(),
            analysis_request_id: phase.claim.request_id.clone(),
            analysis_fence: phase.claim.fence,
            phase_sequence: phase.phase_sequence,
            custody_id: phase.custody.custody_id.clone(),
        };
        // Drop the held descriptor before reporting this physical phase joined.
        drop(attested);
        if !store
            .record_copy_index_source_attestation(
                phase.clone(),
                receipt.clone(),
                crate::state::clock_ms(),
            )
            .await
            .map_err(|e| e.to_string())?
        {
            return Err("copy attestation phase lost ownership".into());
        }
        Ok(VerifiedFullSource::Phase(receipt))
    })
    .await;
    active.finish(&store).await;
    result.map(Some)
}

fn exact_phase_role(role: &AnalysisCoordination, phase: &AnalysisPhysicalPhase) -> bool {
    role.request_id == phase.claim.request_id
        && role.source == phase.source
        && role.phase_sequence == phase.phase_sequence
        && role.active_custody_id.as_deref() == Some(phase.custody.custody_id.as_str())
        && role.physical_node_id.as_deref() == Some(phase.claim.node_id.as_str())
        && role.physical_fence == Some(phase.claim.fence)
}

/// A missing exact receipt alone cannot close an unresolved begin: the role's
/// sequence must also fence out a delayed reservation with the original input.
fn begin_is_fenced_out(
    role: Option<&AnalysisCoordination>,
    phase: &AnalysisPhysicalPhase,
    expected_sequence: i64,
) -> bool {
    role.is_none_or(|role| {
        role.active_custody_id.as_deref() != Some(phase.custody.custody_id.as_str())
            && (role.request_id != phase.claim.request_id
                || role.source != phase.source
                || role.phase_sequence > expected_sequence)
    })
}

/// Only persisted state/fence transitions close this claim. Lease expiry,
/// cancellation and removed-owner flags can reverse and are not closure proof.
fn old_claim_is_closed(original: &AnalysisRequest, current: Option<&AnalysisRequest>) -> bool {
    current.is_none_or(|current| {
        current.request_id == original.request_id
            && (current.fence > original.fence
                || (current.fence == original.fence && current.state != "running"))
    })
}

async fn close_unstarted_claim(
    store: &Arc<dyn Store>,
    request: &AnalysisRequest,
    began_at_ms: i64,
) {
    // Retry's CAS requires a live lease relative to its supplied operation
    // clock, and retry_at must be strictly later. Retain the original clock:
    // closing metadata is not permission to activate or read any Source Input.
    let retry_at_ms = began_at_ms.saturating_add(1).max(crate::state::clock_ms());
    loop {
        match store
            .retry_analysis_request(
                request,
                "source_phase_ack_unknown",
                began_at_ms,
                retry_at_ms,
                false,
            )
            .await
        {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, request_id=%request.request_id, "copy unstarted claim close acknowledgement remains pending")
            }
        }
        match store.analysis_request(&request.request_id).await {
            Ok(current) if old_claim_is_closed(request, current.as_ref()) => return,
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, request_id=%request.request_id, "copy unstarted claim closure lookup remains pending")
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

async fn resolve_unstarted_phase(
    store: &Arc<dyn Store>,
    attempted: &AnalysisPhysicalPhase,
    request: &AnalysisRequest,
    began_at_ms: i64,
) {
    // Close acknowledgement MUST precede the authoritative custody observation:
    // a delayed original begin could have committed just before this closure.
    close_unstarted_claim(store, request, began_at_ms).await;
    loop {
        let role = store
            .analysis_coordination(&attempted.claim.request_id)
            .await;
        let custody = store
            .resolve_background_source_custody(attempted.custody.clone())
            .await;
        match (role, custody) {
            (Ok(role), Ok(custody))
                if role
                    .as_ref()
                    .is_some_and(|role| exact_phase_role(role, attempted)) =>
            {
                let mut phase = attempted.clone();
                if let Some(custody) = custody {
                    phase.custody = custody;
                }
                // No activation/Input has been exposed; durable DELETE still
                // requires NoReader. Owner closure does not settle an Input.
                crate::background_jobs::settle_no_reader_source_custody(
                    store,
                    phase.custody.clone(),
                )
                .await;
                finish_exact_phase(store, &phase).await;
                return;
            }
            (Ok(role), Ok(None))
                if role.as_ref().is_none_or(|role| {
                    role.active_custody_id.as_deref() != Some(attempted.custody.custody_id.as_str())
                }) =>
            {
                return
            }
            (Err(error), _) | (_, Err(error)) => {
                tracing::warn!(%error, custody_id=%attempted.custody.custody_id, "copy no-reader phase lookup remains pending")
            }
            _ => {
                tracing::warn!(custody_id=%attempted.custody.custody_id, "copy no-reader phase identity remains unresolved")
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// This runs only after joined/no-reader settlement. A false deletion or a
/// lost finish acknowledgement never releases the operation's pending state.
async fn finish_exact_phase(store: &Arc<dyn Store>, phase: &AnalysisPhysicalPhase) {
    loop {
        match store.finish_analysis_source_phase(phase.clone()).await {
            Ok(true) => return,
            Ok(false) => {
                let custody = store
                    .resolve_background_source_custody(phase.custody.clone())
                    .await;
                let role = store.analysis_coordination(&phase.claim.request_id).await;
                if matches!(custody, Ok(None))
                    && role.as_ref().is_ok_and(|role| {
                        begin_is_fenced_out(role.as_ref(), phase, phase.phase_sequence - 1)
                    })
                {
                    return;
                }
                tracing::warn!(custody_id=%phase.custody.custody_id, "copy phase cleanup acknowledgement remains pending");
            }
            Err(error) => {
                tracing::warn!(%error, custody_id=%phase.custody.custody_id, "copy phase finish remains pending")
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

#[cfg(test)]
mod acknowledgement_tests {
    use super::*;

    fn phase_and_role() -> (AnalysisPhysicalPhase, AnalysisCoordination) {
        let source = AnalysisCoordinationSource {
            file_id: 7,
            source_size: 100,
            source_mtime: 11,
            origin_node_id: "source-node".into(),
            object_version: "held-object".into(),
            video_identity: "ab".repeat(32),
            contract_version: 1,
            requested_generation: "normal".into(),
        };
        let claim = AnalysisCoordinationClaim {
            request_id: "opaque-request".into(),
            node_id: "source-node".into(),
            fence: 3,
        };
        let phase = AnalysisPhysicalPhase {
            custody: plurx_core::store::background_jobs_resources::BackgroundSourceCustodyToken {
                custody_id: "00000000-0000-4000-8000-000000000007".into(),
                owner: claim.physical_owner(),
            },
            claim,
            source,
            phase_sequence: 5,
        };
        let role = AnalysisCoordination {
            request_id: phase.claim.request_id.clone(),
            source: phase.source.clone(),
            phase_sequence: 5,
            active_custody_id: Some(phase.custody.custody_id.clone()),
            physical_node_id: Some(phase.claim.node_id.clone()),
            physical_fence: Some(3),
        };
        (phase, role)
    }

    #[test]
    fn ambiguous_begin_requires_exact_phase_and_never_treats_empty_initial_role_as_closed() {
        let (phase, mut role) = phase_and_role();
        assert!(exact_phase_role(&role, &phase));
        assert!(!begin_is_fenced_out(Some(&role), &phase, 4));
        role.physical_fence = Some(4);
        assert!(
            !exact_phase_role(&role, &phase),
            "successor fence cannot settle this phase"
        );
        role.physical_fence = Some(3);
        role.physical_node_id = Some("other-node".into());
        assert!(!exact_phase_role(&role, &phase));
        role.physical_node_id = Some(phase.claim.node_id.clone());
        role.source.object_version = "replacement-object".into();
        assert!(!exact_phase_role(&role, &phase));
        assert!(
            !begin_is_fenced_out(Some(&role), &phase, 4),
            "our UUID must remain pending even on inconsistent source metadata"
        );
        role.source = phase.source.clone();
        role.active_custody_id = None;
        role.physical_node_id = None;
        role.physical_fence = None;
        role.phase_sequence = 4;
        assert!(
            !begin_is_fenced_out(Some(&role), &phase, 4),
            "lost write can still reserve this empty initial sequence"
        );
        role.phase_sequence = 5;
        assert!(
            begin_is_fenced_out(Some(&role), &phase, 4),
            "completed sequence fences delayed identical begin"
        );
        role.phase_sequence = 6;
        role.active_custody_id = Some("00000000-0000-4000-8000-000000000008".into());
        assert!(
            begin_is_fenced_out(Some(&role), &phase, 4),
            "newer phase is isolated from old cleanup"
        );
        assert!(begin_is_fenced_out(None, &phase, 4));
    }
}

#[cfg(test)]
mod closure_tests {
    use super::*;
    fn running_request() -> AnalysisRequest {
        AnalysisRequest {
            request_id: "request-a".into(),
            file_id: 7,
            source_size: 100,
            source_mtime: 11,
            component: "fragment_index".into(),
            pipeline_version: "copy-v3".into(),
            video_identity: "ab".repeat(32),
            requested_generation: "normal".into(),
            expected_predecessor_generation: String::new(),
            priority: "normal".into(),
            trigger: "playback".into(),
            force_rebuild: false,
            target_node_id: "source-node".into(),
            state: "running".into(),
            owner_node_id: "source-node".into(),
            fence: 3,
            lease_expires_ms: 10_000,
            attempts: 1,
            not_before_ms: 0,
            result_cache_key: String::new(),
            last_error_code: String::new(),
            cancel_requested: false,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }
    #[test]
    fn unknown_close_ack_requires_persisted_state_or_monotonic_fence_not_reversible_flags() {
        let original = running_request();
        let mut current = original.clone();
        current.lease_expires_ms = 0;
        current.cancel_requested = true;
        assert!(
            !old_claim_is_closed(&original, Some(&current)),
            "expiry and cancellation cannot close delayed begin"
        );
        current.cancel_requested = false;
        current.lease_expires_ms = 50_000;
        assert!(
            !old_claim_is_closed(&original, Some(&current)),
            "pending heartbeat may renew same running fence"
        );
        current.state = "queued".into();
        current.owner_node_id.clear();
        assert!(
            old_claim_is_closed(&original, Some(&current)),
            "lost successful close ACK resolved by durable queued state"
        );
        current.state = "running".into();
        current.owner_node_id = "source-node".into();
        current.fence += 1;
        assert!(
            old_claim_is_closed(&original, Some(&current)),
            "new claim cannot revive the previous fence"
        );
        current.fence = 2;
        assert!(
            !old_claim_is_closed(&original, Some(&current)),
            "older observation proves no closure"
        );
    }
}
