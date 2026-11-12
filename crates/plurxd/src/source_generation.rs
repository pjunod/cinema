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
    source_observer: Option<(u64, crate::source_custody::SourceObserver)>,
    owners: Vec<Arc<SourceRenditionOwner>>,
    current: Option<ProducerRegistration>,
    pending_dispatches: usize,
    assigned_namespace: bool,
    initial_permit: Option<(Arc<SourceRenditionOwner>, crate::vodencode::EncodePermit)>,
    admissions: Option<crate::admission::Admissions>,
    producer_gate: Option<Arc<crate::transcode::source_actor::SourceProducerAuthority>>,
    retiring_current: bool,
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
        let observer = {
            let mut state = self.state.lock().expect("Source rendition owners");
            state.telemetry_capable = true;
            state
                .source_observer
                .as_ref()
                .map(|(_, observer)| observer.clone())
        };
        if let Some(observer) = observer {
            observer.declare_rolling_policy().await?;
        }
        Ok(())
    }

    pub(crate) fn set_source_observer(
        &self,
        epoch: u64,
        observer: crate::source_custody::SourceObserver,
    ) {
        self.state
            .lock()
            .expect("Source rendition owners")
            .source_observer = Some((epoch, observer));
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
            .filter(|(current, _)| *current == epoch)
            .map(|(_, observer)| observer.clone())
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

    /// Whether a Source viewer's assignment has been attached to this
    /// rendition, read under the ledger lock.
    pub(crate) fn is_source_assigned(&self) -> bool {
        self.state
            .lock()
            .expect("Source rendition owners")
            .assigned_namespace
    }

    /// Record a local playback's generation. A local owner registers only on
    /// the ledger its dispatch was captured from, and only while no Source
    /// viewer is attached to it. That is decided here, under the ledger
    /// lock, not from what the dispatch saw when it was captured: an
    /// assignment attached in between refuses the registration, and the
    /// caller retires the child it just attached.
    pub(crate) fn registered_local(
        &self,
        dispatch: &SourceGenerationDispatch,
        token: &ProducerRegistration,
    ) -> Result<(), &'static str> {
        let mut state = self.state.lock().expect("Source rendition owners");
        if !Arc::ptr_eq(&dispatch.ledger, &self.state)
            || state.assigned_namespace
            || !dispatch.owners.is_empty()
        {
            return Err("a local generation cannot register on a Source rendition");
        }
        state.current = Some(token.clone());
        Ok(())
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

/// A local playback's generation and a Source viewer never share a ledger.
/// The half that needs a Source viewer takes a real assignment, which only a
/// Source session yields; the Source actor fixture calls it with its own.
#[cfg(test)]
pub(crate) mod local_registration_tests {
    use super::*;

    #[test]
    fn a_local_generation_registers_only_on_its_own_unassigned_ledger() {
        let owners = SourceRenditionOwners::default();
        let token = crate::prodrun::unattached_registration();
        let dispatch = owners.begin_generation().expect("a local dispatch");
        assert!(!owners.is_source_assigned());
        owners
            .registered_local(&dispatch, &token)
            .expect("an unassigned ledger records a local generation");
        // Another rendition's ledger does not take this dispatch.
        let other = SourceRenditionOwners::default();
        assert!(other.registered_local(&dispatch, &token).is_err());
    }

    /// The branch `registered_local` exists for: an assignment attached
    /// after a local dispatch was captured. The dispatch still says what it
    /// saw; the ledger, asked under its lock, refuses.
    pub(crate) fn a_source_viewer_attached_after_capture_refuses_the_local_registration(
        assignment: &SourceDispatchAssignment,
    ) {
        let owners = SourceRenditionOwners::default();
        let token = crate::prodrun::unattached_registration();
        let dispatch = owners
            .begin_generation()
            .expect("a local dispatch captured before any Source viewer");
        assert!(!dispatch.assigned_namespace);
        // The attach itself waits for the unresolved dispatch, but the
        // rendition is a Source one from this moment.
        assert!(owners.attach(assignment).is_err());
        assert!(owners.is_source_assigned());
        assert!(
            owners.registered_local(&dispatch, &token).is_err(),
            "the snapshot the dispatch carries must not decide"
        );
        drop(dispatch);
        // With the viewer attached, a dispatch captured now is refused too.
        owners
            .attach(assignment)
            .expect("the Source viewer attaches");
        let dispatch = owners.begin_generation().expect("a Source dispatch");
        assert!(owners.registered_local(&dispatch, &token).is_err());
        assert!(owners.terminal_registration().is_none());
    }
}
