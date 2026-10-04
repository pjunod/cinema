//! Actual registered producer associations retained by private Source viewers.
//! This barrier never releases SQL capacity or certifies downstream HTTP bodies.
use super::*;
use crate::prodrun::ProducerRegistration;
use plurx_core::sharing_source_sessions::SourceDispatchAssignment;

const SOURCE_OWNER_LIMIT: usize = 8;
const UNSETTLED_GENERATION_LIMIT: usize = 64;

#[derive(Default)]
pub(super) struct SourceRenditionOwners {
    state: Arc<StdMutex<SourceOwnersState>>,
}
#[derive(Default)]
struct SourceOwnersState {
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
}
/// One viewer's producer associations are settled; its actor must separately
/// settle demand, readers, response writers and its exact durable route.
#[allow(dead_code)] // Private Source actor handoff is the next integration.
pub(crate) struct SourceProducerAssociationsSettled {
    assignment: SourceDispatchAssignment,
}
#[allow(dead_code)]
impl SourceProducerAssociationsSettled {
    pub(crate) fn matches(&self, assignment: &SourceDispatchAssignment) -> bool {
        self.assignment.same_identity(assignment)
    }
}

impl SourceRenditionOwners {
    pub(super) fn contains_live_assignment(&self, assignment: &SourceDispatchAssignment) -> bool {
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
    #[allow(dead_code)] // Source attach remains closed until actor integration.
    pub(super) fn attach(
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
    pub(super) fn begin_generation(&self) -> Result<SourceGenerationDispatch, &'static str> {
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
    pub(super) fn arm_initial_permit(
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
        state.initial_permit = Some((Arc::clone(owner), permit));
        state.admissions = Some(admissions.clone());
        state.producer_gate = Some(Arc::clone(producer_gate));
        Ok(())
    }

    pub(super) fn copy_admissions(&self) -> Option<crate::admission::Admissions> {
        self.state
            .lock()
            .expect("Source rendition owners")
            .admissions
            .clone()
    }

    pub(super) fn take_initial_permit(&self) -> Option<crate::vodencode::EncodePermit> {
        self.state
            .lock()
            .expect("Source rendition owners")
            .initial_permit
            .take()
            .map(|(_, permit)| permit)
    }

    pub(super) fn terminal_registration(&self) -> Option<ProducerRegistration> {
        let state = self.state.lock().expect("Source rendition owners");
        state
            .retiring_current
            .then(|| state.current.clone())
            .flatten()
    }

    /// Called only with the token returned by actual job-owned producer attach.
    pub(super) fn registered(
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
    #[allow(dead_code)] // Private owned actor invokes after demand detachment.
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
pub(super) struct SourceGenerationDispatch {
    ledger: Arc<StdMutex<SourceOwnersState>>,
    owners: Vec<Arc<SourceRenditionOwner>>,
    producer_gate: Option<Arc<crate::transcode::source_actor::SourceProducerAuthority>>,
    assigned_namespace: bool,
}
impl SourceGenerationDispatch {
    pub(super) async fn authorize_before_spawn(
        &self,
    ) -> Result<Option<crate::transcode::source_actor::SourceGenerationAuthority>, String> {
        if !self.assigned_namespace {
            return Ok(None);
        }
        let gate = self
            .producer_gate
            .as_ref()
            .ok_or_else(|| "Source producer gate is unavailable".to_owned())?;
        let assignments: Vec<_> = self
            .owners
            .iter()
            .map(|owner| owner.assignment.clone())
            .collect();
        gate.authorize_generation(&assignments).await.map(Some)
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
