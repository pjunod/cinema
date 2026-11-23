//! Actual registered producer associations retained by private Source viewers.
//! This barrier never releases SQL capacity or certifies downstream HTTP bodies.
use crate::prodrun::ProducerRegistration;
use plurx_core::sharing_source_sessions::SourceDispatchAssignment;
use std::sync::{Arc, Mutex as StdMutex, Weak};
use tokio::sync::Notify;

const SOURCE_OWNER_LIMIT: usize = 8;
const UNSETTLED_GENERATION_LIMIT: usize = 64;

#[derive(Default)]
pub(crate) struct SourceRenditionOwners {
    state: Arc<StdMutex<SourceOwnersState>>,
}
#[derive(Default)]
struct SourceOwnersState {
    telemetry_capable: bool,
    source_observer: Option<PublishedSourceObserver>,
    owners: Vec<Arc<SourceRenditionOwner>>,
    current: Option<ProducerRegistration>,
    pending_dispatches: usize,
    assigned_namespace: bool,
    initial_permit: Option<(Arc<SourceRenditionOwner>, crate::vodencode::EncodePermit)>,
    admissions: Option<crate::admission::Admissions>,
    producer_gate: Option<Arc<crate::transcode::source_actor::SourceProducerAuthority>>,
    retiring_current: bool,
}
struct PublishedSourceObserver {
    epoch: u64,
    observer: crate::source_custody::SourceObserver,
    identity: Arc<()>,
}

/// Owned by the actual SourceInput, never by the cached rendition. Revocation
/// and policy enqueue share the ledger lock, so an accepted policy command is
/// queued before that input can settle its custody.
pub(crate) struct SourceObserverPublication {
    ledger: Weak<StdMutex<SourceOwnersState>>,
    identity: Arc<()>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    epoch: u64,
}
impl SourceObserverPublication {
    /// Publish `observer` for the same rendition and generation epoch as this
    /// publication. A DV window run's later windows read through the run's
    /// input, so that input's observer replaces the unused window input's.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn republish(
        &self,
        observer: crate::source_custody::SourceObserver,
    ) -> Option<(
        SourceObserverPublication,
        impl std::future::Future<Output = Result<(), String>> + Send + 'static,
    )> {
        let ledger = self.ledger.upgrade()?;
        Some(publish_into(&ledger, self.epoch, observer))
    }
}
impl Drop for SourceObserverPublication {
    fn drop(&mut self) {
        let Some(ledger) = self.ledger.upgrade() else {
            return;
        };
        let mut state = ledger.lock().expect("Source rendition owners");
        if state
            .source_observer
            .as_ref()
            .is_some_and(|published| Arc::ptr_eq(&published.identity, &self.identity))
        {
            state.source_observer = None;
        }
    }
}

/// Publish `observer` as the rendition's foreground source observer for
/// `epoch`, unless a newer generation's is already published.
fn publish_into(
    ledger: &Arc<StdMutex<SourceOwnersState>>,
    epoch: u64,
    observer: crate::source_custody::SourceObserver,
) -> (
    SourceObserverPublication,
    impl std::future::Future<Output = Result<(), String>> + Send + 'static,
) {
    let identity = Arc::new(());
    let update = {
        let mut state = ledger.lock().expect("Source rendition owners");
        let update = state
            .telemetry_capable
            .then(|| observer.begin_rolling_policy());
        // An older input finishing its open cannot replace a newer live
        // generation. Identity also fences replacement within one epoch.
        if state
            .source_observer
            .as_ref()
            .is_none_or(|published| published.epoch <= epoch)
        {
            state.source_observer = Some(PublishedSourceObserver {
                epoch,
                observer,
                identity: Arc::clone(&identity),
            });
        }
        update
    };
    (
        SourceObserverPublication {
            ledger: Arc::downgrade(ledger),
            identity,
            epoch,
        },
        async move {
            if let Some(update) = update {
                update.await?;
            }
            Ok(())
        },
    )
}

/// No wire constructor. The private deferred-create path associates a closed
/// full assignment with this exact rendition before committing viewer demand.
pub(crate) struct SourceRenditionOwner {
    assignment: SourceDispatchAssignment,
    ledger: Weak<StdMutex<SourceOwnersState>>,
    generations: StdMutex<SourceOwnerGenerations>,
    changed: Notify,
}
#[derive(Default)]
struct SourceOwnerGenerations {
    detached: bool,
    in_flight: usize,
    used: Vec<ProducerRegistration>,
    ingress: Option<plurx_core::sharing_source_sessions::SourceIngressAdmissionPermission>,
}
/// One viewer's producer associations are settled; its actor must separately
/// settle demand, readers, response writers and its exact durable route.
pub(crate) struct SourceProducerAssociationsSettled {
    assignment: SourceDispatchAssignment,
}
impl SourceProducerAssociationsSettled {
    pub(crate) fn matches(&self, assignment: &SourceDispatchAssignment) -> bool {
        self.assignment.same_identity(assignment)
    }
}

impl SourceRenditionOwners {
    pub(crate) fn has_source_telemetry(&self) -> bool {
        self.state
            .lock()
            .expect("Source rendition owners")
            .telemetry_capable
    }
    pub(crate) async fn declare_source_telemetry(&self) -> Result<(), String> {
        let update = {
            let mut state = self.state.lock().expect("Source rendition owners");
            state.telemetry_capable = true;
            state
                .source_observer
                .as_ref()
                .map(|published| published.observer.begin_rolling_policy())
        };
        if let Some(update) = update {
            update.await?;
        }
        Ok(())
    }

    pub(crate) fn publish_source_observer(
        &self,
        epoch: u64,
        observer: crate::source_custody::SourceObserver,
    ) -> (
        SourceObserverPublication,
        impl std::future::Future<Output = Result<(), String>> + Send + 'static,
    ) {
        publish_into(&self.state, epoch, observer)
    }
    pub(crate) fn source_observer(
        &self,
        epoch: u64,
    ) -> Option<crate::source_custody::SourceObserver> {
        self.state
            .lock()
            .expect("Source rendition owners")
            .source_observer
            .as_ref()
            .filter(|published| published.epoch == epoch)
            .map(|published| published.observer.clone())
    }

    pub(crate) fn contains_live_assignment(&self, assignment: &SourceDispatchAssignment) -> bool {
        self.state
            .lock()
            .expect("Source rendition owners")
            .owners
            .iter()
            .any(|owner| {
                owner.assignment.same_identity(assignment)
                    && !owner
                        .generations
                        .lock()
                        .expect("Source producer associations")
                        .detached
            })
    }
    /// Test-only observation of this exact original assignment after all of
    /// its real dispatch/child associations have positively retired. This
    /// cannot detach an owner or manufacture any producer-close receipt.
    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn settled_assignment_for_actor_test(
        &self,
        assignment: &SourceDispatchAssignment,
    ) -> bool {
        let state = self.state.lock().expect("Source rendition owners");
        state.assigned_namespace
            && state.pending_dispatches == 0
            && state.initial_permit.is_none()
            && state
                .current
                .as_ref()
                .is_none_or(|token| token.confirmed_reap().is_some())
            && state
                .owners
                .iter()
                .any(|owner| owner.assignment.same_identity(assignment))
            && state.owners.iter().all(|owner| owner.fully_settled())
    }

    pub(crate) fn attach(
        &self,
        assignment: &SourceDispatchAssignment,
    ) -> Result<Arc<SourceRenditionOwner>, &'static str> {
        let mut state = self.state.lock().expect("Source rendition owners");
        state.assigned_namespace = true;
        if state.retiring_current {
            if state.pending_dispatches == 0
                && state
                    .current
                    .as_ref()
                    .is_none_or(|token| token.confirmed_reap().is_some())
            {
                state.retiring_current = false;
            } else {
                return Err("Source producer retirement is unresolved");
            }
        }
        state.owners.retain(|owner| !owner.fully_settled());
        if let Some(existing) = state
            .owners
            .iter()
            .find(|owner| owner.assignment.same_identity(assignment))
        {
            if existing
                .generations
                .lock()
                .expect("Source producer associations")
                .detached
            {
                return Err("Source viewer retirement is unresolved");
            }
            return Ok(Arc::clone(existing));
        }
        if state.pending_dispatches > 0 {
            return Err("Source producer dispatch is unresolved");
        }
        if state.owners.len() >= SOURCE_OWNER_LIMIT {
            return Err("Source rendition owner capacity");
        }
        let mut generations = SourceOwnerGenerations::default();
        if let Some(current) = state
            .current
            .as_ref()
            .filter(|token| token.confirmed_reap().is_none())
        {
            generations.used.push(current.clone());
        }
        let owner = Arc::new(SourceRenditionOwner {
            assignment: assignment.clone(),
            ledger: Arc::downgrade(&self.state),
            generations: StdMutex::new(generations),
            changed: Notify::new(),
        });
        state.owners.push(Arc::clone(&owner));
        Ok(owner)
    }

    /// Capture associations before any async spawn preparation. Retirement
    /// cannot declare no producer while this actual dispatch is unresolved.
    pub(crate) fn begin_generation(&self) -> Result<SourceGenerationDispatch, &'static str> {
        let mut state = self.state.lock().expect("Source rendition owners");
        let owners: Vec<_> = state
            .owners
            .iter()
            .filter(|owner| {
                !owner
                    .generations
                    .lock()
                    .expect("Source producer associations")
                    .detached
            })
            .cloned()
            .collect();
        if state.assigned_namespace && owners.is_empty() {
            return Err("Source producer has no owned viewer demand");
        }
        for owner in &owners {
            let mut generations = owner
                .generations
                .lock()
                .expect("Source producer associations");
            generations
                .used
                .retain(|token| token.confirmed_reap().is_none());
            if generations.used.len() + generations.in_flight >= UNSETTLED_GENERATION_LIMIT {
                return Err("Source producer settlement capacity is unavailable");
            }
        }
        for owner in &owners {
            owner
                .generations
                .lock()
                .expect("Source producer associations")
                .in_flight += 1;
        }
        state.pending_dispatches += 1;
        Ok(SourceGenerationDispatch {
            owners,
            ledger: Arc::clone(&self.state),
            producer_gate: state.producer_gate.clone(),
            assigned_namespace: state.assigned_namespace,
        })
    }

    /// The actor transfers its actual first-start reservation before committing
    /// reader demand. A different pending owner cannot replace that reservation.
    pub(crate) fn arm_initial_permit(
        &self,
        owner: &Arc<SourceRenditionOwner>,
        permit: crate::vodencode::EncodePermit,
        admissions: &crate::admission::Admissions,
        producer_gate: &Arc<crate::transcode::source_actor::SourceProducerAuthority>,
    ) -> Result<(), crate::vodencode::EncodePermit> {
        let mut state = self.state.lock().expect("Source rendition owners");
        if state.initial_permit.is_some()
            || !state
                .owners
                .iter()
                .any(|current| Arc::ptr_eq(current, owner))
            || owner
                .generations
                .lock()
                .expect("Source producer associations")
                .detached
        {
            return Err(permit);
        }
        let Some(ingress) = producer_gate.retained_ingress(&owner.assignment) else {
            return Err(permit);
        };
        // Each actual owner's initially issued permission survives ordinary
        // connection gaps independently. A retired predecessor cannot supply
        // or invalidate a successor's producer authority.
        owner
            .generations
            .lock()
            .expect("Source producer associations")
            .ingress = Some(ingress);
        state.initial_permit = Some((Arc::clone(owner), permit));
        state.admissions = Some(admissions.clone());
        state.producer_gate = Some(Arc::clone(producer_gate));
        Ok(())
    }

    pub(crate) fn copy_admissions(&self) -> Option<crate::admission::Admissions> {
        self.state
            .lock()
            .expect("Source rendition owners")
            .admissions
            .clone()
    }

    /// A sequential preparing/primary operation retains the SAME admitted
    /// reservation through its registered reaper. Viewer detachment may drop
    /// the parent reservation only after this independent physical clone exists.
    pub(crate) fn retained_initial_permit(&self) -> Option<crate::vodencode::EncodePermit> {
        self.state
            .lock()
            .expect("Source rendition owners")
            .initial_permit
            .as_ref()
            .map(|(_, permit)| permit.clone())
    }
    pub(crate) fn take_initial_permit(&self) -> Option<crate::vodencode::EncodePermit> {
        self.state
            .lock()
            .expect("Source rendition owners")
            .initial_permit
            .take()
            .map(|(_, permit)| permit)
    }

    pub(crate) fn terminal_registration(&self) -> Option<ProducerRegistration> {
        let state = self.state.lock().expect("Source rendition owners");
        state
            .retiring_current
            .then(|| state.current.clone())
            .flatten()
    }

    /// Called only with the token returned by actual job-owned producer attach.
    pub(crate) fn registered(
        &self,
        dispatch: &SourceGenerationDispatch,
        token: &ProducerRegistration,
    ) {
        let mut state = self.state.lock().expect("Source rendition owners");
        for owner in &dispatch.owners {
            let mut generations = owner
                .generations
                .lock()
                .expect("Source producer associations");
            generations
                .used
                .retain(|old| old.confirmed_reap().is_none());
            if !generations
                .used
                .iter()
                .any(|old| old.same_generation(token))
            {
                generations.used.push(token.clone());
            }
        }
        state.current = Some(token.clone());
    }
}
impl SourceRenditionOwner {
    pub(crate) fn begin_detach(&self) {
        let ledger = self.ledger.upgrade();
        let mut transition = ledger
            .as_ref()
            .map(|state| state.lock().expect("Source rendition owners"));
        self.generations
            .lock()
            .expect("Source producer associations")
            .detached = true;
        if let Some(state) = transition.as_mut() {
            if state
                .initial_permit
                .as_ref()
                .is_some_and(|(owner, _)| std::ptr::eq(owner.as_ref(), self))
            {
                state.initial_permit.take();
            }
            if state.owners.iter().all(|owner| {
                owner
                    .generations
                    .lock()
                    .expect("Source producer associations")
                    .detached
            }) {
                state.retiring_current = true;
            }
        }
    }

    pub(crate) async fn wait_dispatches(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self
                .generations
                .lock()
                .expect("Source producer associations")
                .in_flight
                == 0
            {
                return;
            }
            changed.await;
        }
    }
    /// Woken whenever one of this owner's dispatches settles. Registration
    /// records the token before its dispatch is dropped, so a producer that
    /// [`Self::registered_readiness`] can observe has always woken this.
    pub(crate) fn dispatch_settled(&self) -> tokio::sync::futures::Notified<'_> {
        self.changed.notified()
    }
    /// A registered producer for this immutable rendition is physical evidence.
    /// The worker still requires current file/floor authority before publishing.
    pub(crate) fn registered_readiness(&self) -> Option<ProducerRegistration> {
        let ledger = self.ledger.upgrade()?;
        let state = ledger.lock().expect("Source rendition owners");
        let generations = self
            .generations
            .lock()
            .expect("Source producer associations");
        if generations.detached {
            return None;
        }
        let token = state.current.as_ref()?;
        (token.confirmed_reap().is_some()
            || generations
                .used
                .iter()
                .any(|used| used.same_generation(token)))
        .then(|| token.clone())
    }
    fn fully_settled(&self) -> bool {
        let generations = self
            .generations
            .lock()
            .expect("Source producer associations");
        generations.detached
            && generations.in_flight == 0
            && generations
                .used
                .iter()
                .all(|token| token.confirmed_reap().is_some())
    }
    /// Detach under the reader/lifecycle transition before this is called.
    /// Other viewers may keep the rendition's producer running; that keeps
    /// this viewer's exact physical obligation retained until actual reap.
    pub(crate) async fn detach_and_wait(&self) -> SourceProducerAssociationsSettled {
        // Serialize detachment with capture before waiting on actual tokens.
        self.begin_detach();
        self.wait_dispatches().await;
        let used = loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let ready = {
                let generations = self
                    .generations
                    .lock()
                    .expect("Source producer associations");
                (generations.in_flight == 0).then(|| generations.used.clone())
            };
            if let Some(used) = ready {
                break used;
            }
            changed.await;
        };
        for token in used {
            let _ = token.wait_confirmed_reap().await;
        }
        SourceProducerAssociationsSettled {
            assignment: self.assignment.clone(),
        }
    }
}

/// Owned by the actual spawn path until its registration or no-child failure.
pub(crate) struct SourceGenerationDispatch {
    ledger: Arc<StdMutex<SourceOwnersState>>,
    owners: Vec<Arc<SourceRenditionOwner>>,
    producer_gate: Option<Arc<crate::transcode::source_actor::SourceProducerAuthority>>,
    assigned_namespace: bool,
}
impl SourceGenerationDispatch {
    /// Only the first private growing range uses preparing authority. The
    /// association barrier still spans the actual child and all output drains.
    pub(crate) async fn authorize_preparing_before_spawn(
        &self,
    ) -> Result<crate::transcode::source_actor::SourceGenerationAuthority, String> {
        if !self.assigned_namespace {
            return Err("Source bootstrap is not an assigned namespace".into());
        }
        let gate = self
            .producer_gate
            .as_ref()
            .ok_or_else(|| "Source producer gate is unavailable".to_owned())?;
        let permissions: Vec<_> = self
            .owners
            .iter()
            .filter_map(|owner| {
                owner
                    .generations
                    .lock()
                    .expect("Source producer associations")
                    .ingress
                    .clone()
            })
            .collect();
        gate.authorize_preparing_generation(&permissions).await
    }

    pub(crate) async fn authorize_before_spawn(
        &self,
    ) -> Result<Option<crate::transcode::source_actor::SourceGenerationAuthority>, String> {
        if !self.assigned_namespace {
            return Ok(None);
        }
        let gate = self
            .producer_gate
            .as_ref()
            .ok_or_else(|| "Source producer gate is unavailable".to_owned())?;
        let permissions: Vec<_> = self
            .owners
            .iter()
            .filter_map(|owner| {
                owner
                    .generations
                    .lock()
                    .expect("Source producer associations")
                    .ingress
                    .clone()
            })
            .collect();
        // A newly attached owner has no authority until its actual actor arms
        // admission. Existing admitted owners continue during that transition.
        gate.authorize_generation(&permissions).await.map(Some)
    }
}
impl Drop for SourceGenerationDispatch {
    fn drop(&mut self) {
        let mut state = self.ledger.lock().expect("Source rendition owners");
        state.pending_dispatches -= 1;
        for owner in &self.owners {
            owner
                .generations
                .lock()
                .expect("Source producer associations")
                .in_flight -= 1;
            owner.changed.notify_waiters();
        }
    }
}

#[cfg(test)]
mod observer_publication_tests {
    use super::*;
    use crate::source_custody::SourceObserver;

    #[tokio::test]
    async fn retired_observer_reuse_defers_telemetry_to_next_live_input() {
        let owners = SourceRenditionOwners::default();
        let (old, old_owner) = SourceObserver::policy_fixture();
        let (publication, update) = owners.publish_source_observer(7, old);
        update.await.expect("no telemetry yet");
        drop(publication);
        drop(old_owner); // The original source owner has actually ended.
        assert!(owners.source_observer(7).is_none());
        owners
            .declare_source_telemetry()
            .await
            .expect("cached rendition remains reusable");
        assert!(owners.has_source_telemetry());

        let (next, mut next_owner) = SourceObserver::policy_fixture();
        let (publication, update) = owners.publish_source_observer(8, next.clone());
        // Publication enqueues pending policy even though its request was
        // prepared before telemetry arrived; no projection future is polled.
        let reply = next_owner.projection().await;
        assert!(!next.rolling_policy());
        reply.send(Ok(())).expect("policy waiter");
        update
            .await
            .expect("next live owner accepts rolling policy");
        assert!(next.rolling_policy());
        drop(publication);
        assert!(owners.source_observer(8).is_none());
    }

    #[tokio::test]
    async fn retiring_old_input_cannot_unpublish_replacement_or_newer_epoch() {
        let owners = SourceRenditionOwners::default();
        let (old, _old_owner) = SourceObserver::policy_fixture();
        let (old_publication, update) = owners.publish_source_observer(10, old);
        update.await.expect("old publication");
        let (replacement, _replacement_owner) = SourceObserver::policy_fixture();
        let (replacement_publication, update) = owners.publish_source_observer(10, replacement);
        update.await.expect("same-epoch replacement");
        drop(old_publication);
        assert!(owners.source_observer(10).is_some());

        let (next, _next_owner) = SourceObserver::policy_fixture();
        let (next_publication, update) = owners.publish_source_observer(11, next);
        update.await.expect("next epoch");
        let (late, _late_owner) = SourceObserver::policy_fixture();
        let (late_publication, update) = owners.publish_source_observer(10, late);
        update
            .await
            .expect("late old input cannot replace current publication");
        assert!(owners.source_observer(10).is_none());
        drop(replacement_publication);
        drop(late_publication);
        assert!(owners.source_observer(11).is_some());
        drop(next_publication);
        assert!(owners.source_observer(11).is_none());
    }

    #[tokio::test]
    async fn telemetry_accepted_before_retirement_keeps_its_authority_reply() {
        let owners = Arc::new(SourceRenditionOwners::default());
        let (observer, mut owner) = SourceObserver::policy_fixture();
        let (publication, update) = owners.publish_source_observer(3, observer.clone());
        update.await.expect("publication");
        let declaration = tokio::spawn({
            let owners = Arc::clone(&owners);
            async move { owners.declare_source_telemetry().await }
        });
        // Reaching the real command receiver proves the declaration has
        // passed the fence. Retirement must not discard its authority result.
        let reply = owner.projection().await;
        drop(publication);
        assert!(owners.source_observer(3).is_none());
        owners
            .declare_source_telemetry()
            .await
            .expect("later attach stays pending");
        reply.send(Ok(())).expect("accepted declaration");
        declaration
            .await
            .expect("declaration task")
            .expect("authority accepted");
        assert!(observer.rolling_policy());
    }

    #[tokio::test]
    async fn live_owner_authority_failure_is_preserved() {
        let owners = Arc::new(SourceRenditionOwners::default());
        let (observer, mut owner) = SourceObserver::policy_fixture();
        let (_publication, update) = owners.publish_source_observer(5, observer.clone());
        update.await.expect("publication");
        let declaration = tokio::spawn({
            let owners = Arc::clone(&owners);
            async move { owners.declare_source_telemetry().await }
        });
        owner
            .projection()
            .await
            .send(Err("source authority refused policy".into()))
            .expect("live waiter");
        assert_eq!(
            declaration.await.expect("task"),
            Err("source authority refused policy".into())
        );
        assert!(!observer.has_authority());
        assert!(!observer.rolling_policy());
        assert!(owners.source_observer(5).is_some());
    }

    #[tokio::test]
    async fn closed_live_owner_is_an_error_until_publication_retires() {
        let owners = SourceRenditionOwners::default();
        let (observer, owner) = SourceObserver::policy_fixture();
        let (publication, update) = owners.publish_source_observer(6, observer);
        update.await.expect("publication");
        drop(owner);
        assert_eq!(
            owners.declare_source_telemetry().await,
            Err("original-source owner ended".into())
        );
        drop(publication);
        owners
            .declare_source_telemetry()
            .await
            .expect("retired input has no live projection");
    }
}
