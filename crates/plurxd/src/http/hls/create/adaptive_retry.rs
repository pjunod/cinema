//! Same-request placement retry. Store fences coordinate ownership; they never
//! certify native extinction. Unknown outcomes hold the original attempt.
use super::*;
use plurx_core::store::adaptive_resource_attempts::{
    AttemptClaim, AttemptFence, AttemptMutation, AttemptPhase, AttemptRefusal, MutationContext,
    RequestBinding, RequestDisposition, RequestStatus,
};

pub(super) enum AdaptiveOriginFailure {
    NextWorker(ApiError),
    Hold(ApiError),
}
impl From<ApiError> for AdaptiveOriginFailure {
    fn from(error: ApiError) -> Self {
        Self::Hold(error)
    }
}
pub(super) struct AdaptiveOriginContext {
    pub binding: RequestBinding,
    // Cardinality is bounded by the durable three-attempt contract. This is
    // original control payload, not an assertion about Tokio/native peak size.
    attempts: Vec<OriginalAttempt>,
}
struct OriginalAttempt {
    claim: AttemptClaim,
    fence: Option<AttemptFence>,
}
impl AdaptiveOriginContext {
    pub fn new(binding: RequestBinding) -> Self {
        Self {
            binding,
            attempts: Vec::with_capacity(3),
        }
    }
}
fn held(message: &str) -> ApiError {
    ApiError::ServiceUnavailable(message.to_owned())
}

async fn current_status(
    state: &AppState,
    binding: &RequestBinding,
    deadline: tokio::time::Instant,
) -> Result<Option<RequestStatus>, ApiError> {
    let status = tokio::time::timeout_at(
        deadline,
        state
            .store
            .adaptive_request_status(&binding.key, &binding.principal),
    )
    .await
    .map_err(|_| held("adaptive status exceeded the original deadline"))?
    .map_err(|error| session_store_error("reading original adaptive request", error))?;
    if let Some(status) = &status {
        status
            .validate()
            .map_err(|_| held("adaptive request status is invalid"))?;
        if &status.binding != binding {
            return Err(held("adaptive immutable request binding changed"));
        }
    }
    Ok(status)
}

/// None skips an already-used worker without consuming an ordinal. A bound
/// request or unconfirmed predecessor holds, even when another worker is free.
fn next_claim_slot(
    status: Option<&RequestStatus>,
    worker: &str,
) -> Result<Option<(i64, u8)>, ApiError> {
    let Some(status) = status else {
        return Ok(Some((0, 1)));
    };
    if status.disposition != RequestDisposition::Open || status.winner_attempt_id.is_some() {
        return Err(held("adaptive request already selected a winner or ended"));
    }
    if status
        .attempts
        .iter()
        .any(|row| row.key.worker_node_id == worker)
    {
        return Ok(None);
    }
    if status.attempts.len() >= 3
        || status
            .attempts
            .iter()
            .filter(|row| row.phase != AttemptPhase::Closed)
            .count()
            >= 2
    {
        return Err(held("adaptive request reached its durable attempt bound"));
    }
    if status
        .attempts
        .iter()
        .max_by_key(|row| row.key.ordinal)
        .is_some_and(|row| {
            !matches!(
                row.phase,
                AttemptPhase::Cleanup | AttemptPhase::Losing | AttemptPhase::Closed
            )
        })
    {
        return Err(held("adaptive predecessor has no confirmed durable fence"));
    }
    Ok(Some((status.sequence, status.next_ordinal)))
}
fn correct_claim(claim: &mut AttemptClaim, sequence: i64, ordinal: u8) {
    claim.context = MutationContext {
        operation_id: uuid::Uuid::new_v4(),
        expected_request_sequence: sequence,
        expected_attempt_sequence: 0,
        now_ms: unix_ms(),
    };
    claim.key.ordinal = ordinal;
    claim.key.owner_epoch = i64::from(ordinal);
}

pub(super) async fn claim_original(
    state: &AppState,
    context: &mut AdaptiveOriginContext,
    mut claim: AttemptClaim,
    deadline: tokio::time::Instant,
) -> Result<AttemptClaim, AdaptiveOriginFailure> {
    let status = current_status(state, &context.binding, deadline).await?;
    let Some((sequence, ordinal)) = next_claim_slot(status.as_ref(), &claim.key.worker_node_id)?
    else {
        return Err(AdaptiveOriginFailure::NextWorker(held(
            "adaptive worker was already attempted",
        )));
    };
    correct_claim(&mut claim, sequence, ordinal);
    claim
        .validate()
        .map_err(|_| held("adaptive Claim contract is invalid"))?;
    if context.attempts.len() >= 3 {
        return Err(held("adaptive retained attempt bound reached").into());
    }
    context
        .attempts
        .push(OriginalAttempt { claim, fence: None });
    let original = context
        .attempts
        .last_mut()
        .expect("retained original Claim");
    let mut stale_corrected = false;
    let mut unknown_answers = 0;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(held("adaptive Claim remains unknown at original deadline").into());
        }
        let answer = tokio::time::timeout_at(
            deadline,
            state.store.claim_adaptive_attempt(&original.claim),
        )
        .await;
        match answer {
            Ok(Ok(AttemptMutation::Applied(status) | AttemptMutation::Replay(status))) => {
                status
                    .validate()
                    .map_err(|_| held("adaptive Claim status is invalid"))?;
                if status.binding != original.claim.binding
                    || !status.attempts.iter().any(|row| {
                        row.key == original.claim.key && row.phase == AttemptPhase::Provisional
                    })
                {
                    return Err(held("adaptive Claim was changed before Prepare").into());
                }
                return Ok(original.claim.clone());
            }
            Ok(Ok(AttemptMutation::Refused(AttemptRefusal::Bound))) => {
                // A definite refusal did not insert this candidate/ordinal.
                context.attempts.pop();
                return Err(AdaptiveOriginFailure::NextWorker(held(
                    "adaptive candidate is durably bound",
                )));
            }
            Ok(Ok(AttemptMutation::Refused(AttemptRefusal::StaleBinding))) if !stale_corrected => {
                stale_corrected = true;
                let status = current_status(state, &context.binding, deadline).await?;
                let Some((sequence, ordinal)) =
                    next_claim_slot(status.as_ref(), &original.claim.key.worker_node_id)?
                else {
                    context.attempts.pop();
                    return Err(AdaptiveOriginFailure::NextWorker(held(
                        "adaptive worker was already attempted",
                    )));
                };
                // Only a definite noncommit permits a new operation identity.
                correct_claim(&mut original.claim, sequence, ordinal);
                unknown_answers = 0;
            }
            Ok(Ok(AttemptMutation::Refused(_))) => {
                return Err(held("adaptive Claim was refused").into())
            }
            Ok(Err(_)) | Err(_) => {
                unknown_answers += 1;
                if unknown_answers >= 2 {
                    return Err(held("adaptive Claim acknowledgement remains unknown").into());
                }
                // Replay the identical operation, payload, clock and identities.
            }
        }
    }
}

fn fenced(status: &RequestStatus, claim: &AttemptClaim) -> bool {
    status.binding == claim.binding
        && status.attempts.iter().any(|row| {
            row.key == claim.key
                && matches!(
                    row.phase,
                    AttemptPhase::Cleanup | AttemptPhase::Losing | AttemptPhase::Closed
                )
        })
}
fn retain_fence(original: &mut OriginalAttempt, status: &RequestStatus) -> Result<(), ApiError> {
    let row = status
        .attempts
        .iter()
        .find(|row| row.key == original.claim.key)
        .ok_or_else(|| held("adaptive original attempt disappeared before Fence"))?;
    original.fence = Some(AttemptFence {
        context: MutationContext {
            operation_id: uuid::Uuid::new_v4(),
            expected_request_sequence: status.sequence,
            expected_attempt_sequence: row.sequence,
            now_ms: unix_ms(),
        },
        binding: original.claim.binding.clone(),
        key: original.claim.key.clone(),
        cancel_request: false,
    });
    Ok(())
}

pub(super) async fn fence_original(
    state: &AppState,
    context: &mut AdaptiveOriginContext,
    claim: &AttemptClaim,
    deadline: tokio::time::Instant,
) -> Result<(), ApiError> {
    let original = context
        .attempts
        .iter_mut()
        .find(|row| row.claim.key == claim.key)
        .ok_or_else(|| held("adaptive original Claim selector is absent"))?;
    let status = current_status(state, &context.binding, deadline)
        .await?
        .ok_or_else(|| held("adaptive original row disappeared before Fence"))?;
    if status.winner_attempt_id.is_some() {
        return Err(held("adaptive Winner cannot retry"));
    }
    if fenced(&status, claim) {
        return Ok(());
    }
    if tokio::time::Instant::now() >= deadline {
        return Err(held("adaptive Fence deadline expired"));
    }
    if original.fence.is_none() {
        retain_fence(original, &status)?;
    }
    let fence = original.fence.as_ref().expect("retained original Fence");
    // First let the original worker retain and drive its own native cleanup.
    // Transport uncertainty never establishes that the worker reserved nothing.
    if claim.key.worker_node_id == state.node_id {
        if let Ok(receiver) = state.transcode.begin_adaptive_cancel(fence.clone()) {
            let _ = tokio::time::timeout_at(deadline, receiver).await;
        }
    } else {
        let cancel = crate::media_sessions::AdaptiveAttemptCancelV1 {
            version: crate::media_sessions::ADAPTIVE_ATTEMPT_VERSION,
            fence: fence.clone(),
        };
        let _ = state
            .media_sessions
            .adaptive_attempt_peer_request(
                &claim.key.worker_node_id,
                crate::media_sessions::ADAPTIVE_ATTEMPT_CANCEL_PATH,
                &cancel,
                deadline,
            )
            .await;
    }
    let mut rebuilt = false;
    let mut applications = 0;
    loop {
        let status = current_status(state, &context.binding, deadline)
            .await?
            .ok_or_else(|| held("adaptive row absent while confirming Fence"))?;
        if status.winner_attempt_id.is_some() {
            return Err(held("adaptive Winner cannot retry"));
        }
        if fenced(&status, claim) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline || applications >= 2 {
            return Err(held("adaptive Fence acknowledgement remains unknown"));
        }
        applications += 1;
        let fence = original.fence.as_ref().expect("same retained Fence");
        let answer =
            tokio::time::timeout_at(deadline, state.store.fence_adaptive_attempt(fence)).await;
        match answer {
            Ok(Ok(AttemptMutation::Applied(status) | AttemptMutation::Replay(status))) => {
                status
                    .validate()
                    .map_err(|_| held("adaptive Fence status is invalid"))?;
                if status.winner_attempt_id.is_some() || !fenced(&status, claim) {
                    return Err(held(
                        "adaptive Fence did not confirm the original losing attempt",
                    ));
                }
                // A fresh read, rather than this acknowledgement alone, gates
                // the next worker. It also observes a concurrent winner.
            }
            Ok(Ok(AttemptMutation::Refused(AttemptRefusal::StaleBinding))) if !rebuilt => {
                let status = current_status(state, &context.binding, deadline)
                    .await?
                    .ok_or_else(|| held("adaptive stale Fence row disappeared"))?;
                if status.winner_attempt_id.is_some() {
                    return Err(held("adaptive Winner cannot retry"));
                }
                if fenced(&status, claim) {
                    return Ok(());
                }
                rebuilt = true;
                retain_fence(original, &status)?;
                applications = 0;
            }
            Ok(Ok(AttemptMutation::Refused(_))) => return Err(held("adaptive Fence was refused")),
            Ok(Err(_)) | Err(_) => {} // Same operation only; no new owner or Close.
        }
    }
}

#[cfg(test)]
#[path = "adaptive_retry/tests.rs"]
mod tests;
