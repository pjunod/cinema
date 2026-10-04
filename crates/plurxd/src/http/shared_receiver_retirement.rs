//! Private receiver cleanup. Metadata expiry never constructs these receipts.
use super::*;
use plurx_core::sharing_receiver_retirement::{
    ReceiverPendingOwner, ReceiverPendingRetirementWitness, ReceiverRetirementDisposition,
    ReceiverRetirementOutcome, ReceiverRetirementReason, ReceiverRetirementWitness,
};
use sha2::{Digest, Sha256};

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
    fn mark_confirmed_retired(&self, witness: &ConfirmedRetirement) {
        let mut owned = self.0.state.lock().expect("receiver owner");
        // A dispatched obligation requires the actual physical Source receipt.
        // Never-dispatched proof is minted only after both independent joins.
        if owned.dispatched.is_some() && witness._source.is_none() {
            return;
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
        let original: serde_json::Value = serde_json::from_str(&self.0.intent.recipe.request_json)
            .map_err(|_| ReceiverStartError::Unresolved)?;
        let playback = original
            .get("playback_id")
            .and_then(serde_json::Value::as_str)
            .ok_or(ReceiverStartError::Unresolved)?
            .to_owned();
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
            owned.dispatch_closed = true;
        }
        self.0.bodies.close();
        self.0.stop.cancel();
        let actor = self.clone();
        let task = tokio::spawn(async move {
            actor.retire_owned(state, reason).await;
        });
        *self
            .0
            .retirement_task
            .lock()
            .expect("receiver retirement task") = Some(task);
    }
    async fn retire_owned(self, state: Arc<AppState>, reason: ReceiverRetirementReason) {
        let Ok(joined) = self.join_start_for_cleanup().await else {
            return;
        };
        let bodies = self.0.bodies.join().await;
        // Sealed resource admissions plus actual last guard release are needed
        // even for a no-send outcome. No elapsed timeout can construct this.
        if !Arc::ptr_eq(&bodies.0, &self.0.bodies) {
            return;
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
                if !planned {
                    self.mark_retired();
                }
                return;
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
            let Ok(mut pending) = self.confirmed_pending(&joined, &bodies, owner) else {
                return;
            };
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
                        return;
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
                    }
                    Err(_) => {} // Preserve the exact witness after commit-unknown.
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
        let mut witness = loop {
            match self
                .confirm_retirement(&state, &joined, &bodies, reason)
                .await
            {
                Ok(witness) => break witness,
                Err(_) => tokio::time::sleep(Duration::from_secs(5)).await,
            }
        };
        loop {
            let outcome = state.store.retire_receiver_session(&witness).await;
            let refused = matches!(&outcome, Ok(ReceiverRetirementOutcome::Refused));
            match outcome {
                Ok(ReceiverRetirementOutcome::Applied | ReceiverRetirementOutcome::Replay) => {
                    self.mark_confirmed_retired(&witness);
                    return;
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
                            self.mark_confirmed_retired(&witness);
                            return;
                        }
                        _ => witness.binding = binding,
                    }
                }
                _ => {}
            }
            if refused {
                // A sweep can advance a terminal lease while physical facts
                // remain unchanged. Repeat exact immutable route checks;
                // commit-unknown never enters this metadata refresh path.
                if let Ok(refreshed) = self
                    .confirm_retirement(&state, &joined, &bodies, reason)
                    .await
                {
                    if refreshed.confirmation == witness.confirmation {
                        witness = refreshed;
                    }
                }
            }
            // Preserve the exact authenticated confirmation and envelope on
            // uncertain commits; do not replace it with an expiry observation.
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
    async fn confirm_retirement(
        &self,
        state: &AppState,
        joined: &JoinedReceiverStart,
        bodies: &JoinedReceiverBodies,
        reason: ReceiverRetirementReason,
    ) -> Result<ConfirmedRetirement, ReceiverStartError> {
        if !Arc::ptr_eq(&joined.0, &self.0) || !Arc::ptr_eq(&bodies.0, &self.0.bodies) {
            return Err(ReceiverStartError::Unresolved);
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
                return Err(ReceiverStartError::Unresolved);
            }
            (
                owned
                    .planned_activation
                    .clone()
                    .ok_or(ReceiverStartError::Unresolved)?,
                owned.owner.clone(),
                owned.source.as_ref().map(|a| a.binding.clone()),
            )
        };
        let route = state
            .store
            .media_session_route_by_incarnation(&planned.incarnation_id)
            .await
            .map_err(|_| ReceiverStartError::Unresolved)?
            .ok_or(ReceiverStartError::Unresolved)?;
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
            return Err(ReceiverStartError::Unresolved);
        }
        if owner.as_ref().is_some_and(|o| {
            o.owner_epoch != route.owner_epoch
                || o.owner_node_id != route.owner_node_id
                || o.incarnation_id.to_string() != route.incarnation_id
                || o.session_id.to_string() != route.session_id
        }) {
            return Err(ReceiverStartError::Unresolved);
        }
        let owner = ReceiverSourceOwner {
            incarnation_id: Uuid::parse_str(&route.incarnation_id)
                .map_err(|_| ReceiverStartError::Unresolved)?,
            session_id: Uuid::parse_str(&route.session_id)
                .map_err(|_| ReceiverStartError::Unresolved)?,
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
                    .map_err(|_| ReceiverStartError::Unresolved)?,
            )
        } else {
            if binding.is_some() {
                return Err(ReceiverStartError::Unresolved);
            }
            let identity = serde_json::to_vec(&serde_json::json!({
                "recipe":self.0.intent.recipe,"incarnation":owner.incarnation_id,
                "session":owner.session_id,"node":owner.owner_node_id,"epoch":owner.owner_epoch,
                "request":owner.request_id,
            }))
            .map_err(|_| ReceiverStartError::Unresolved)?;
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
