//! Private receiver cleanup. Metadata expiry never constructs these receipts.
use super::*;
use plurx_core::sharing_receiver_retirement::{
    ReceiverPendingOwner, ReceiverPendingRetirementWitness, ReceiverRetirementDisposition,
    ReceiverRetirementOutcome, ReceiverRetirementReason, ReceiverRetirementWitness,
};
use sha2::{Digest, Sha256};
#[path = "shared_receiver_orphans.rs"]
mod orphans;
pub(crate) use orphans::receiver_recovery_loop;

/// How many failed attempts one retirement owner makes before it stops.
/// Bounded work on a detached owner, never on an admission path.
const RETIREMENT_ATTEMPTS: u32 = 6;
/// Bound an actual writer join without inventing a closure or cutting another
/// session on its multiplexed connection when supersession cannot settle.
const RECEIVER_WRITER_JOIN_DEADLINE: Duration = Duration::from_secs(315);
/// The first retry delay; each further failure doubles it up to the cap.
const RETIREMENT_RETRY: Duration = Duration::from_secs(5);
const RETIREMENT_MAX_BACKOFF: Duration = Duration::from_secs(80);

fn retirement_retry_delay(failures: u32) -> Duration {
    let exponent = failures.saturating_sub(1).min(4);
    RETIREMENT_RETRY
        .saturating_mul(1_u32 << exponent)
        .min(RETIREMENT_MAX_BACKOFF)
}

/// Why one retirement step produced no witness or outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RetirementStep {
    /// Transport, deadline or Store commit-unknown. The same exact witness may
    /// still apply, so the owner repeats it within its bounded budget.
    Retry,
    /// The exact immutable witness was refused or cannot be constructed.
    /// Repeating it cannot change the answer.
    Refused,
}
impl RetirementStep {
    /// Only an authenticated, definitive Source answer about this exact End
    /// request is terminal. Dials, deadlines, 503s and unreadable replies may
    /// still reach the same Source End owner, which a repeated End reattaches.
    pub(super) fn from_source_end(error: crate::sharing_client::PeerError) -> Self {
        use crate::sharing_client::PeerError;
        match error {
            PeerError::Authentication | PeerError::ProtocolUnsupported => Self::Refused,
            PeerError::Rejected(status)
                if status.is_client_error() && !matches!(status.as_u16(), 408 | 429) =>
            {
                Self::Refused
            }
            PeerError::Unavailable
            | PeerError::IdentityMismatch
            | PeerError::InvalidResponse
            // A Start refusal is never evidence about this End operation.
            | PeerError::DolbyVisionUnsupported
            | PeerError::Rejected(_) => Self::Retry,
        }
    }
}

/// Why a retirement owner ended without a confirmed retirement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetirementStall {
    /// The exact Start task or body registry could not be joined, so no
    /// retirement witness can exist.
    Unjoined,
    /// An exact immutable witness was definitively refused.
    Refused,
    /// The bounded retry budget ran out on transport or commit-unknown failures.
    Exhausted,
    /// Daemon drain arrived between attempts.
    Shutdown,
}

/// The retry budget of one retirement owner. An attempt in flight is never
/// raced: each is bounded by its own Store or peer deadline, and drain is
/// observed only before the next one.
struct RetirementBudget {
    failures: u32,
    shutdown: tokio_util::sync::CancellationToken,
}
impl RetirementBudget {
    fn new(shutdown: tokio_util::sync::CancellationToken) -> Self {
        Self {
            failures: 0,
            shutdown,
        }
    }
    async fn retry(&mut self) -> Result<(), RetirementStall> {
        self.failures = self.failures.saturating_add(1);
        if self.failures >= RETIREMENT_ATTEMPTS {
            return Err(RetirementStall::Exhausted);
        }
        tokio::select! {
            () = self.shutdown.cancelled() => Err(RetirementStall::Shutdown),
            () = tokio::time::sleep(retirement_retry_delay(self.failures)) => Ok(()),
        }
    }
}

#[derive(Default)]
pub(super) struct ReceiverBodyRegistry {
    state: Mutex<BodyState>,
    changed: tokio::sync::Notify,
}
#[derive(Default)]
struct BodyState {
    closed: bool,
    active: usize,
    connections: Vec<(
        std::sync::Weak<dyn Send + Sync>,
        std::sync::Weak<ReceiverResourceGuard>,
    )>,
}
pub(super) struct ReceiverResourceGuard(Arc<ReceiverBodyRegistry>);
impl Drop for ReceiverResourceGuard {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().expect("receiver bodies");
        state.active = state
            .active
            .checked_sub(1)
            .expect("owned body registration");
        drop(state);
        self.0.changed.notify_waiters();
    }
}
struct JoinedReceiverBodies(Arc<ReceiverBodyRegistry>);
impl ReceiverBodyRegistry {
    // Every open task, accepted response writer and independent read job must
    // retain a clone of this guard. A response monitor releases its clone only
    // after actual SharingConnectionClosure; Body EOF is insufficient.
    pub(super) fn reserve(
        self: &Arc<Self>,
    ) -> Result<Arc<ReceiverResourceGuard>, ReceiverStartError> {
        let mut state = self.state.lock().expect("receiver bodies");
        if state.closed || state.active >= 32 {
            return Err(ReceiverStartError::Capacity);
        }
        state.active += 1;
        Ok(Arc::new(ReceiverResourceGuard(self.clone())))
    }
    pub(super) fn reserve_connection(
        self: &Arc<Self>,
        connection: &crate::SharingConnectionCancellation,
    ) -> Result<(Arc<ReceiverResourceGuard>, bool), ReceiverStartError> {
        let key = connection.ownership_key();
        let mut state = self.state.lock().expect("receiver bodies");
        if state.closed {
            return Err(ReceiverStartError::Capacity);
        }
        state
            .connections
            .retain(|(key, guard)| key.strong_count() > 0 && guard.strong_count() > 0);
        for (existing, guard) in &state.connections {
            if std::sync::Weak::ptr_eq(existing, &key) {
                if let Some(guard) = guard.upgrade() {
                    return Ok((guard, false));
                }
            }
        }
        if state.active >= 32 {
            return Err(ReceiverStartError::Capacity);
        }
        state.active += 1;
        let guard = Arc::new(ReceiverResourceGuard(self.clone()));
        state.connections.push((key, Arc::downgrade(&guard)));
        Ok((guard, true))
    }
    fn close(&self) {
        self.state.lock().expect("receiver bodies").closed = true;
    }
    async fn join(self: &Arc<Self>) -> JoinedReceiverBodies {
        self.close();
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.state.lock().expect("receiver bodies").active == 0 {
                return JoinedReceiverBodies(self.clone());
            }
            changed.await;
        }
    }
}

// No Serde, public constructor, caller boolean or metadata-only proof.
struct ConfirmedRetirement {
    intent: ReceiverSessionIntent,
    owner: ReceiverSourceOwner,
    binding: Option<ReceiverSourceBinding>,
    disposition: ReceiverRetirementDisposition,
    reason: ReceiverRetirementReason,
    confirmation: String,
    _source: Option<Arc<crate::sharing_client::SourceEndReceipt>>,
}
// An End acknowledgement exists only after the private physical factory and
// exact Store retirement Applied/Replay. It never contains an actor/registry
// Arc, so caching it cannot create an ownership cycle or reopen admission.
pub(super) struct ReceiverEndConfirmation {
    session: Uuid,
    _incarnation: Uuid,
    _confirmation: String,
    _source: Option<Arc<crate::sharing_client::SourceEndReceipt>>,
}
impl ReceiverEndConfirmation {
    pub(super) fn session_id(&self) -> Uuid {
        self.session
    }
}
impl ReceiverRetirementWitness for ConfirmedRetirement {
    fn intent(&self) -> &ReceiverSessionIntent {
        &self.intent
    }
    fn owner(&self) -> &ReceiverSourceOwner {
        &self.owner
    }
    fn binding(&self) -> Option<&ReceiverSourceBinding> {
        self.binding.as_ref()
    }
    fn disposition(&self) -> ReceiverRetirementDisposition {
        self.disposition
    }
    fn reason(&self) -> ReceiverRetirementReason {
        self.reason
    }
    fn confirmation_id(&self) -> &str {
        &self.confirmation
    }
}

// Constructed only after the owned Start and all accepted bodies/jobs joined,
// dispatch was sealed, and this actual attempt never sent Source Start.
struct ConfirmedPending {
    intent: ReceiverSessionIntent,
    request: String,
    playback: String,
    owner: ReceiverPendingOwner,
    confirmation: String,
}
impl ReceiverPendingRetirementWitness for ConfirmedPending {
    fn intent(&self) -> &ReceiverSessionIntent {
        &self.intent
    }
    fn request_id(&self) -> &str {
        &self.request
    }
    fn playback_id(&self) -> &str {
        &self.playback
    }
    fn owner(&self) -> &ReceiverPendingOwner {
        &self.owner
    }
    fn confirmation_id(&self) -> &str {
        &self.confirmation
    }
}

impl ReceiverStartActor {
    fn mark_confirmed_retired(&self, witness: &ConfirmedRetirement) -> Result<(), RetirementStall> {
        let mut owned = self.0.state.lock().expect("receiver owner");
        // A dispatched obligation requires the actual physical Source receipt.
        // Never-dispatched proof is minted only after both independent joins.
        if owned.dispatched.is_some() && witness._source.is_none() {
            return Err(RetirementStall::Refused);
        }
        owned.end_confirmation = Some(Arc::new(ReceiverEndConfirmation {
            session: witness.owner.session_id,
            _incarnation: witness.owner.incarnation_id,
            _confirmation: witness.confirmation.clone(),
            _source: witness._source.clone(),
        }));
        owned.retired = true;
        drop(owned);
        self.0.changed.notify_waiters();
        Ok(())
    }
    fn mark_retired(&self) {
        self.0.state.lock().expect("receiver owner").retired = true;
        self.0.changed.notify_waiters();
    }
    fn confirmed_pending(
        &self,
        joined: &JoinedReceiverStart,
        bodies: &JoinedReceiverBodies,
        owner: ReceiverPendingOwner,
    ) -> Result<ConfirmedPending, ReceiverStartError> {
        if !Arc::ptr_eq(&joined.0, &self.0) || !Arc::ptr_eq(&bodies.0, &self.0.bodies) {
            return Err(ReceiverStartError::Unresolved);
        }
        {
            let state = self.0.state.lock().expect("receiver owner");
            if !state.dispatch_closed
                || state.dispatched.is_some()
                || state.received.is_some()
                || state.source.is_some()
                || state.confirmed_source_end.is_some()
            {
                return Err(ReceiverStartError::Unresolved);
            }
        }
        // The playback the pending route was claimed under: B's own
        // per-session identity, never the viewer's.
        let playback = crate::sharing::receiver_playback_id(&self.0.intent.recipe);
        let node = match &owner {
            ReceiverPendingOwner::Unassigned => None,
            ReceiverPendingOwner::Assigned(node) => Some(node.as_str()),
        };
        let identity = serde_json::to_vec(&serde_json::json!({
            "intent":self.0.intent.recipe,"request":self.0.request_id,"playback":playback,
            "node":node,
        }))
        .map_err(|_| ReceiverStartError::Unresolved)?;
        let mut digest = Sha256::new();
        digest.update(b"plurx.receiver.joined-never-dispatched-pending.v1\0");
        digest.update(identity);
        Ok(ConfirmedPending {
            intent: self.0.intent.clone(),
            request: self.0.request_id.clone(),
            playback,
            owner,
            confirmation: format!("{:x}", digest.finalize()),
        })
    }
    pub(crate) fn begin_retirement(&self, state: Arc<AppState>, reason: ReceiverRetirementReason) {
        {
            let mut owned = self.0.state.lock().expect("receiver owner");
            if owned.retirement_started {
                return;
            }
            owned.retirement_started = true;
            owned.retirement_reason = Some(reason);
            owned.dispatch_closed = true;
        }
        // An uncommitted prepared successor has no viewer once the session it
        // was prepared for retires; it is withdrawn through its own owner.
        let withdrawn = self.0.take_uncommitted_successor();
        self.0.bodies.close();
        self.0.stop.cancel();
        let actor = self.clone();
        let owner_state = state.clone();
        let task = tokio::spawn(async move {
            actor.retire_owned(owner_state, reason).await;
        });
        *self
            .0
            .retirement_task
            .lock()
            .expect("receiver retirement task") = Some(task);
        if let Some(successor) = withdrawn {
            ReceiverStartActor(successor)
                .begin_retirement(state, ReceiverRetirementReason::Replaced);
        }
    }
    async fn retire_owned(self, state: Arc<AppState>, reason: ReceiverRetirementReason) {
        let mut budget = RetirementBudget::new(state.shutdown.clone());
        let Err(stall) = self.retire_exact(&state, reason, &mut budget).await else {
            return;
        };
        // Every exit ends this owner and releases its registry slot. Nothing
        // here renews the route: the published owner has already stopped, so
        // its lease lapses, and the durable route and binding keep the exact
        // lineage that only an exact retirement may remove. That obligation is
        // the durable row, not this slot; holding the slot would let stuck
        // settlements refuse all shared playback. The pruned tombstone carries
        // no End receipt, so an exact retry stays unresolved and End is 503.
        tracing::warn!(
            target: "plurxd::sharing",
            import = %self.0.intent.scope.import_id,
            incarnation = %self.0.intent.recipe.source_request_id,
            stall = ?stall,
            failed_attempts = budget.failures,
            "shared playback retirement ended without a confirmed retirement; the durable route keeps its exact lineage"
        );
        self.mark_retired();
    }
    async fn retire_exact(
        &self,
        state: &AppState,
        reason: ReceiverRetirementReason,
        budget: &mut RetirementBudget,
    ) -> Result<(), RetirementStall> {
        let joined = self
            .join_start_for_cleanup()
            .await
            .map_err(|_| RetirementStall::Unjoined)?;
        let bodies = tokio::time::timeout(RECEIVER_WRITER_JOIN_DEADLINE, self.0.bodies.join())
            .await
            .map_err(|_| RetirementStall::Unjoined)?;
        // Sealed resource admissions plus actual last guard release are needed
        // even for a no-send outcome. No elapsed timeout can construct this.
        if !Arc::ptr_eq(&bodies.0, &self.0.bodies) {
            return Err(RetirementStall::Unjoined);
        }
        let (claim, never_sent, planned) = {
            let owned = self.0.state.lock().expect("receiver owner");
            (
                owned.claim.clone(),
                owned.dispatched.is_none(),
                owned.planned_activation.is_some(),
            )
        };
        if never_sent {
            // These outcomes never acquired a row or physical producer for this
            // process-local actor. Release only its joined inert registry slot.
            if matches!(
                claim,
                ReceiverClaimStage::NotAttempted | ReceiverClaimStage::NotAcquired
            ) {
                if planned {
                    return Err(RetirementStall::Refused);
                }
                self.mark_retired();
                return Ok(());
            }
            let attempted_node = match &claim {
                ReceiverClaimStage::Assigning(node) | ReceiverClaimStage::Assigned(node) => {
                    Some(node.clone())
                }
                _ => None,
            };
            let owner = attempted_node
                .as_ref()
                .map_or(ReceiverPendingOwner::Unassigned, |node| {
                    ReceiverPendingOwner::Assigned(node.clone())
                });
            let mut pending = self
                .confirmed_pending(&joined, &bodies, owner)
                .map_err(|_| RetirementStall::Refused)?;
            let mut unassigned = if attempted_node.is_some() {
                self.confirmed_pending(&joined, &bodies, ReceiverPendingOwner::Unassigned)
                    .ok()
            } else {
                None
            };
            loop {
                match state.store.retire_pending_receiver_request(&pending).await {
                    Ok(ReceiverRetirementOutcome::Applied | ReceiverRetirementOutcome::Replay) => {
                        self.mark_retired();
                        return Ok(());
                    }
                    Ok(ReceiverRetirementOutcome::Refused) => {
                        if let Some(fallback) = unassigned.take() {
                            // Change the expected NULL/attempted owner only on
                            // Refused; an uncertain commit keeps this exact
                            // witness for every subsequent retry.
                            pending = fallback;
                            continue;
                        }
                        // Refused proves this exact transaction did not apply.
                        // A planned activation may instead have committed an
                        // owned route; only the full route witness can retire it.
                        if planned {
                            break;
                        }
                        // Nothing was planned, so no later state can make the
                        // identical transaction apply. Resending it is not cleanup.
                        return Err(RetirementStall::Refused);
                    }
                    Err(_) => {} // Preserve the exact witness after commit-unknown.
                }
                budget.retry().await?;
            }
        }
        let mut witness = loop {
            match self
                .confirm_retirement(state, &joined, &bodies, reason)
                .await
            {
                Ok(witness) => break witness,
                Err(RetirementStep::Refused) => return Err(RetirementStall::Refused),
                Err(RetirementStep::Retry) => budget.retry().await?,
            }
        };
        loop {
            let outcome = state.store.retire_receiver_session(&witness).await;
            let refused = matches!(&outcome, Ok(ReceiverRetirementOutcome::Refused));
            match outcome {
                Ok(ReceiverRetirementOutcome::Applied | ReceiverRetirementOutcome::Replay) => {
                    return self.mark_confirmed_retired(&witness);
                }
                Ok(ReceiverRetirementOutcome::Refused) if witness.binding.is_some() => {
                    // Only Refused permits trying the legitimate pending case.
                    // Its transaction proves all binding columns are NULL;
                    // no Store error or absence is interpreted as pending.
                    let binding = witness.binding.take();
                    match state.store.retire_receiver_session(&witness).await {
                        Ok(
                            ReceiverRetirementOutcome::Applied | ReceiverRetirementOutcome::Replay,
                        ) => {
                            return self.mark_confirmed_retired(&witness);
                        }
                        _ => witness.binding = binding,
                    }
                }
                _ => {}
            }
            if refused {
                // A sweep can advance a terminal lease while physical facts
                // remain unchanged. Repeat exact immutable route checks; a
                // definitive refusal of the route itself ends the owner, and
                // commit-unknown never enters this metadata refresh path.
                match self
                    .confirm_retirement(state, &joined, &bodies, reason)
                    .await
                {
                    Ok(refreshed) if refreshed.confirmation == witness.confirmation => {
                        witness = refreshed;
                    }
                    Ok(_) | Err(RetirementStep::Refused) => {
                        return Err(RetirementStall::Refused);
                    }
                    Err(RetirementStep::Retry) => {}
                }
            }
            // Preserve the exact authenticated confirmation and envelope on
            // uncertain commits; do not replace it with an expiry observation.
            budget.retry().await?;
        }
    }
    async fn confirm_retirement(
        &self,
        state: &AppState,
        joined: &JoinedReceiverStart,
        bodies: &JoinedReceiverBodies,
        reason: ReceiverRetirementReason,
    ) -> Result<ConfirmedRetirement, RetirementStep> {
        // Only a Store read or the Source dial may be repeated; every other
        // failure is a refusal of the exact immutable route and receipt.
        if !Arc::ptr_eq(&joined.0, &self.0) || !Arc::ptr_eq(&bodies.0, &self.0.bodies) {
            return Err(RetirementStep::Refused);
        }
        let dispatched = self
            .0
            .state
            .lock()
            .expect("receiver owner")
            .dispatched
            .is_some();
        let source = if dispatched {
            Some(self.request_source_end(state, joined).await?)
        } else {
            None
        };
        let (planned, owner, binding) = {
            let owned = self.0.state.lock().expect("receiver owner");
            if !owned.dispatch_closed {
                return Err(RetirementStep::Refused);
            }
            (
                owned
                    .planned_activation
                    .clone()
                    .ok_or(RetirementStep::Refused)?,
                owned.owner.clone(),
                owned.source.as_ref().map(|a| a.binding.clone()),
            )
        };
        let route = state
            .store
            .media_session_route_by_incarnation(&planned.incarnation_id)
            .await
            .map_err(|_| RetirementStep::Retry)?
            .ok_or(RetirementStep::Refused)?;
        if route.incarnation_id != planned.incarnation_id
            || route.session_id != planned.session_id
            || route.principal != planned.principal
            || route.playback_id != planned.playback_id
            || route.recovery_epoch != planned.recovery_epoch
            || route.owner_node_id != planned.owner_node_id
            || route.owner_epoch <= 0
            || route.recipe_json != planned.recipe_json
            || route.request_fingerprint != planned.request_fingerprint
            || route.media_origin_ms != planned.media_origin_ms
            || !matches!(route.state.as_str(), "active" | "ended")
        {
            return Err(RetirementStep::Refused);
        }
        if owner.as_ref().is_some_and(|o| {
            o.owner_epoch != route.owner_epoch
                || o.owner_node_id != route.owner_node_id
                || o.incarnation_id.to_string() != route.incarnation_id
                || o.session_id.to_string() != route.session_id
        }) {
            return Err(RetirementStep::Refused);
        }
        let owner = ReceiverSourceOwner {
            incarnation_id: Uuid::parse_str(&route.incarnation_id)
                .map_err(|_| RetirementStep::Refused)?,
            session_id: Uuid::parse_str(&route.session_id).map_err(|_| RetirementStep::Refused)?,
            owner_node_id: route.owner_node_id,
            owner_epoch: route.owner_epoch,
            request_id: self.0.request_id.clone(),
            lease_expires_at_ms: route.lease_expires_at_ms,
            now_ms: clock_ms(),
        };
        let (disposition, confirmation) = if let Some(receipt) = &source {
            (
                ReceiverRetirementDisposition::SourceSettled,
                receipt
                    .retirement_confirmation(&self.0.peer_session)
                    .map_err(|_| RetirementStep::Refused)?,
            )
        } else {
            if binding.is_some() {
                return Err(RetirementStep::Refused);
            }
            let identity = serde_json::to_vec(&serde_json::json!({
                "recipe":self.0.intent.recipe,"incarnation":owner.incarnation_id,
                "session":owner.session_id,"node":owner.owner_node_id,"epoch":owner.owner_epoch,
                "request":owner.request_id,
            }))
            .map_err(|_| RetirementStep::Refused)?;
            let mut digest = Sha256::new();
            digest.update(b"plurx.receiver.joined-never-dispatched.v1\0");
            digest.update(identity);
            (
                ReceiverRetirementDisposition::NeverDispatched,
                format!("{:x}", digest.finalize()),
            )
        };
        Ok(ConfirmedRetirement {
            intent: self.0.intent.clone(),
            owner,
            binding,
            disposition,
            reason,
            confirmation,
            _source: source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receiver_retirement_backoff_doubles_to_its_cap() {
        let delays: Vec<Duration> = (1..RETIREMENT_ATTEMPTS)
            .map(retirement_retry_delay)
            .collect();
        assert_eq!(delays, [5, 10, 20, 40, 80].map(Duration::from_secs));
        assert_eq!(retirement_retry_delay(u32::MAX), RETIREMENT_MAX_BACKOFF);
    }
    #[tokio::test(start_paused = true)]
    async fn receiver_retirement_budget_is_bounded_and_observes_drain() {
        let mut budget = RetirementBudget::new(tokio_util::sync::CancellationToken::new());
        let started = tokio::time::Instant::now();
        for _ in 1..RETIREMENT_ATTEMPTS {
            budget.retry().await.expect("within the bounded budget");
        }
        assert_eq!(budget.retry().await, Err(RetirementStall::Exhausted));
        assert_eq!(budget.failures, RETIREMENT_ATTEMPTS);
        assert_eq!(started.elapsed(), Duration::from_secs(155));
        let drain = tokio_util::sync::CancellationToken::new();
        let mut budget = RetirementBudget::new(drain.clone());
        drain.cancel();
        let started = tokio::time::Instant::now();
        assert_eq!(budget.retry().await, Err(RetirementStall::Shutdown));
        assert_eq!(started.elapsed(), Duration::ZERO);
    }
    #[test]
    fn receiver_retirement_treats_only_definitive_source_end_refusals_as_terminal() {
        use crate::sharing_client::PeerError;
        use axum::http::StatusCode;
        for refused in [
            PeerError::Authentication,
            PeerError::ProtocolUnsupported,
            PeerError::Rejected(StatusCode::CONFLICT),
            PeerError::Rejected(StatusCode::UNPROCESSABLE_ENTITY),
        ] {
            assert_eq!(
                RetirementStep::from_source_end(refused),
                RetirementStep::Refused
            );
        }
        for retried in [
            PeerError::Unavailable,
            PeerError::IdentityMismatch,
            PeerError::InvalidResponse,
            PeerError::Rejected(StatusCode::SERVICE_UNAVAILABLE),
            PeerError::Rejected(StatusCode::TOO_MANY_REQUESTS),
            PeerError::Rejected(StatusCode::REQUEST_TIMEOUT),
        ] {
            assert_eq!(
                RetirementStep::from_source_end(retried),
                RetirementStep::Retry
            );
        }
    }
    #[tokio::test]
    async fn receiver_resource_retirement_seals_admission_and_waits_actual_owned_job() {
        let registry = Arc::new(ReceiverBodyRegistry::default());
        let response = registry.reserve().expect("resource admission");
        let job_guard = response.clone();
        let (release, waiting) = tokio::sync::oneshot::channel();
        let job = tokio::spawn(async move {
            waiting.await.expect("owned job released");
            drop(job_guard);
        });
        let closed = registry.clone();
        let cleanup = tokio::spawn(async move { closed.join().await });
        registry.close();
        assert!(registry.reserve().is_err());
        drop(response);
        tokio::task::yield_now().await;
        assert!(
            !cleanup.is_finished(),
            "response Drop cannot stand in for the owned read job"
        );
        release.send(()).expect("release job");
        job.await.expect("actual job join");
        let joined = cleanup.await.expect("cleanup owner");
        assert!(Arc::ptr_eq(&joined.0, &registry));
        assert_eq!(registry.state.lock().expect("registry").active, 0);
    }
    #[tokio::test]
    async fn receiver_connection_registration_deduplicates_streams_and_keeps_join_pending() {
        let registry = Arc::new(ReceiverBodyRegistry::default());
        let connection = crate::SharingConnectionCancellation::new();
        let (first, fresh) = registry
            .reserve_connection(&connection)
            .expect("connection");
        assert!(fresh);
        let (second, fresh) = registry
            .reserve_connection(&connection.clone())
            .expect("same connection");
        assert!(!fresh);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(registry.state.lock().expect("registry").active, 1);
        let other = crate::SharingConnectionCancellation::new();
        let (third, fresh) = registry
            .reserve_connection(&other)
            .expect("other connection");
        assert!(fresh);
        assert_eq!(registry.state.lock().expect("registry").active, 2);
        registry.close();
        assert!(registry.reserve_connection(&connection).is_err());
        drop(first);
        drop(third);
        assert_eq!(registry.state.lock().expect("registry").active, 1);
        let owner = registry.clone();
        let joining = tokio::spawn(async move { owner.join().await });
        tokio::task::yield_now().await;
        assert!(
            !joining.is_finished(),
            "body EOF cannot release retained writer guard"
        );
        drop(second);
        let joined = joining.await.expect("actual ownership join");
        assert!(Arc::ptr_eq(&joined.0, &registry));
    }
    #[tokio::test]
    async fn receiver_handover_drains_busy_transports_without_cutting_successor_streams() {
        use super::super::tests::registered;
        let state = Arc::new(crate::http::source_actor_test_state());
        let registry = ReceiverStartRegistry::default();
        let superseded = ReceiverStartActor(registered(&registry, "superseded", "player", 1));
        // `idle` carried an answer whose writer has been released (the commit
        // answer, once its handoff runs); `busy` still has a writer on it.
        let idle = crate::SharingConnectionCancellation::new();
        let busy = crate::SharingConnectionCancellation::new();
        drop(
            superseded
                .retain_accepted_connection(state.clone(), &idle)
                .expect("idle writer"),
        );
        let _busy_writer = superseded
            .retain_accepted_connection(state.clone(), &busy)
            .expect("busy writer");
        superseded.begin_retirement(state.clone(), ReceiverRetirementReason::Superseded);
        let bodies = superseded.0.bodies.clone();
        let revoked = ReceiverStartActor(registered(&registry, "revoked", "other", 2));
        let quiet = crate::SharingConnectionCancellation::new();
        drop(
            revoked
                .retain_accepted_connection(state.clone(), &quiet)
                .expect("quiet writer"),
        );
        revoked.begin_retirement(state, ReceiverRetirementReason::Revoked);
        tokio::time::timeout(Duration::from_secs(5), async {
            busy.drain_token().cancelled().await;
            assert!(
                !busy.0.is_cancelled(),
                "predecessor drain preserves accepted successor streams"
            );
            // Revocation still cuts every transport it was retained on.
            quiet.0.cancelled().await;
            loop {
                let changed = bodies.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if bodies.state.lock().expect("bodies").active == 1 {
                    return;
                }
                changed.await;
            }
        })
        .await
        .expect("busy transport drains, revoked transport cuts, and idle custody releases");
        assert!(
            !idle.0.is_cancelled(),
            "a hand-over never cuts a transport it has no writer on"
        );
    }
    #[tokio::test]
    async fn receiver_resource_retirement_remains_bounded_and_closes_empty_admissions() {
        let registry = Arc::new(ReceiverBodyRegistry::default());
        let mut guards = Vec::new();
        for _ in 0..32 {
            guards.push(registry.reserve().expect("bounded resource"));
        }
        assert!(registry.reserve().is_err());
        drop(guards.pop());
        guards.push(registry.reserve().expect("actual released slot"));
        registry.close();
        assert!(registry.reserve().is_err());
        drop(guards);
        let joined = registry.join().await;
        assert!(Arc::ptr_eq(&joined.0, &registry));
    }
}
