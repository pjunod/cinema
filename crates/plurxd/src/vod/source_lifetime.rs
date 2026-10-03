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
    #[allow(dead_code)] // Source attach remains closed until actor integration.
    pub(super) fn attach(
        &self,
        assignment: &SourceDispatchAssignment,
    ) -> Result<Arc<SourceRenditionOwner>, &'static str> {
        let mut state = self.state.lock().expect("Source rendition owners");
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
        })
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
        {
            // Serialize detachment with the capture snapshot/increment. Without
            // this transition, a spawn could capture just before detachment but
            // increment after an empty barrier had already answered.
            let ledger = self.ledger.upgrade();
            let _transition = ledger
                .as_ref()
                .map(|state| state.lock().expect("Source rendition owners"));
            self.generations
                .lock()
                .expect("Source producer associations")
                .detached = true;
        }
        let used = loop {
            let changed = self.changed.notified();
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
