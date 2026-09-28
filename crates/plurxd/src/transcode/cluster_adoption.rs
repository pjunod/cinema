use super::*;

/// A cluster worker and the process-local replacement gate that must remain
/// held until the ingress has durably accepted or rejected that worker.
pub(crate) struct ClusterSessionStart {
    pub(crate) info: StartInfo,
    pub(crate) replacement: ClusterReplacementGuard,
    pub(crate) created: bool,
}

pub(super) struct SessionCreation {
    pub(super) info: StartInfo,
    pub(super) created: bool,
}

#[derive(Clone, Copy)]
pub(super) struct ClusterServingAdmission {
    pub(super) generation: u64,
    pub(super) deadline: tokio::time::Instant,
}

/// Serializes one player's provisional cluster worker from local creation
/// through the ingress activation verdict and exact predecessor settlement.
/// Cluster make-before-break deliberately does not reap the predecessor
/// before the Store pointer CAS.
pub(crate) struct ClusterReplacementGuard {
    pub(super) registry: Arc<ClusterReplacementGates>,
    pub(super) key: String,
    pub(super) gate: Arc<ReplacementGate>,
    pub(super) permit: Option<tokio::sync::OwnedMutexGuard<()>>,
    pub(super) _predecessor_settlement: Option<SessionSettlementGuard>,
}

impl ClusterReplacementGuard {
    /// Name a session this replacement has brought into existence, so a
    /// reclaim can fence it before taking the key.
    pub(crate) fn publish_fenceable(&self, session_id: &str) {
        if let Ok(mut state) = self.gate.state.lock() {
            if let Some(holder) = state.holder.as_mut() {
                if !holder.fenceable.iter().any(|id| id == session_id) {
                    holder.fenceable.push(session_id.to_owned());
                }
            }
        }
    }

    /// Declare that this hold is no longer a start any viewer is waiting on —
    /// its request has been answered and the guard now lives inside cleanup.
    ///
    /// This is the whole difference between a holder that may be reclaimed
    /// immediately and one that may not. It is a fact the holder knows and a
    /// waiter cannot infer: the measured failure was a cleanup task holding a
    /// key six seconds after its request had already answered the viewer 503,
    /// and from outside that is indistinguishable from a start still building.
    pub(crate) fn mark_abandoned(&self) {
        if let Ok(mut state) = self.gate.state.lock() {
            if let Some(holder) = state.holder.as_mut() {
                holder.abandoned = true;
            }
        }
    }
}

impl Drop for ClusterReplacementGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.gate.state.lock() {
            state.holder = None;
        }
        drop(self.permit.take());
        let Ok(mut entries) = self.registry.entries.lock() else {
            return;
        };
        // Remove only the entry this guard actually held, and only when
        // nothing else still wants it. Holding `entries` is what makes the
        // count meaningful: a waiter upgrades the `Weak` under this same lock,
        // so if one exists the count is already above one.
        //
        // The strong-count test is load-bearing now in a way it was not before
        // the guard started carrying its own `Arc`: with that field alive
        // during `drop`, the old "did the `Weak` fail to upgrade" test could
        // never be true, and the entry was never removed at all.
        if Arc::strong_count(&self.gate) == 1
            && entries
                .get(&self.key)
                .and_then(Weak::upgrade)
                .is_none_or(|gate| Arc::ptr_eq(&gate, &self.gate))
        {
            entries.remove(&self.key);
        }
    }
}

#[derive(Default)]
pub(super) struct ClusterReplacementGates {
    pub(super) entries: std::sync::Mutex<HashMap<String, Weak<ReplacementGate>>>,
}

/// One player's replacement serialization, plus the supersession signal that
/// keeps a newer open from queueing behind an abandoned one.
///
/// The lock alone was the whole gate until 2026-09-21, and a `tokio::sync::
/// Mutex` has no timeout, no poisoning, and no owner the outside world can
/// reach. Every production holder is a detached task — the activation task,
/// the armed handoff, the takeover supervisor, the cleanup spawned by
/// `StartedSessionGuard`'s `Drop` — so the request that began a replacement
/// returns, or is cancelled, while its guard lives on, and nothing aged,
/// expired or force-released the registry. One holder parked on an unbounded
/// await therefore turned its player's key into a three-second refusal for the
/// life of the process: observed on m6 on 2026-09-21, where a start blocked on
/// a 402-second subtitle sidecar extraction refused the viewer's own reopen of
/// the title they were watching.
pub(super) struct ReplacementGate {
    pub(super) lock: Arc<tokio::sync::Mutex<()>>,
    /// Retirement and ownership under ONE lock, deliberately.
    ///
    /// They are the two halves of the same decision, and splitting them opens
    /// a window where a start that won the lock a moment before a reclaim
    /// retired the gate becomes a second live owner of one player. Sharing a
    /// lock makes "judge this hold and retire it" and "claim this hold"
    /// mutually exclusive, so whichever wins renders the other harmless: a
    /// reclaim that loses finds a fresh, healthy holder and refuses; a
    /// claimant that loses sees the retirement and re-enters the registry.
    pub(super) state: std::sync::Mutex<GateState>,
}

#[derive(Default)]
pub(super) struct GateState {
    /// Set once this gate has been reclaimed. A waiter that afterwards wins
    /// its lock is serializing nothing — the player's key is a different gate
    /// now — so it drops the permit and re-enters the registry.
    pub(super) retired: bool,
    /// Present exactly while someone holds the lock.
    pub(super) holder: Option<ReplacementHolder>,
}

/// What a waiter is allowed to know about the hold it is queued behind.
pub(super) struct ReplacementHolder {
    /// When the key was taken. The only clock a reclaim consults, and it is
    /// compared against [`CLUSTER_REPLACEMENT_HOLD_CEILING`], never against
    /// the cooperative wait.
    pub(super) since: tokio::time::Instant,
    /// Set by the holder itself once its request has been answered and the
    /// guard lives on inside cleanup. See `ClusterReplacementGuard::
    /// mark_abandoned`.
    pub(super) abandoned: bool,
    /// Sessions this hold has brought into existence, fenced before the key
    /// moves.
    pub(super) fenceable: Vec<String>,
}

impl ReplacementGate {
    pub(super) fn new() -> Self {
        Self {
            lock: Arc::new(tokio::sync::Mutex::new(())),
            state: std::sync::Mutex::new(GateState::default()),
        }
    }

    /// Claim a gate whose lock this caller has just won. `Ok(false)` means the
    /// gate was retired first and this permit serializes nothing.
    pub(super) fn claim(&self) -> Result<bool, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "cluster replacement gate state was poisoned".to_owned())?;
        if state.retired {
            return Ok(false);
        }
        state.holder = Some(ReplacementHolder {
            since: tokio::time::Instant::now(),
            abandoned: false,
            fenceable: Vec::new(),
        });
        Ok(true)
    }
}

pub(super) struct SessionReleaseGate {
    pub(super) transition: Arc<tokio::sync::Mutex<()>>,
    pub(super) released: AtomicBool,
}

pub(crate) struct SessionAdoptionToken {
    pub(super) gate: Arc<SessionReleaseGate>,
    pub(super) registry: Arc<SessionReleaseGates>,
    pub(super) session_id: String,
}

/// Move-only cleanup ownership that follows a rolling worker across the one
/// registry mutation which changes its public capability. The adoption future
/// owns this value, so cancellation drops the cleanup owner at whichever exact
/// identity the registry currently contains. Implementations must update only
/// process-local identity; durable publication is still owned by the caller.
pub(crate) trait SessionAdoptionOwner: Send + Sized + 'static {
    fn adopted_session_id(&mut self, durable_session_id: &str);
}

impl SessionAdoptionOwner for () {
    fn adopted_session_id(&mut self, _durable_session_id: &str) {}
}

impl Drop for SessionAdoptionToken {
    fn drop(&mut self) {
        // The final token is the only owner that can prove an unretained weak
        // entry is dead. Remove only the exact gate generation: a completed
        // release may have retained it, or a later operation may already have
        // installed a replacement for this never-reused capability key.
        // Take the registry lock before testing the count: every new token
        // upgrades the Weak while holding this lock, so it cannot race this
        // last-owner proof and lose the registry generation beneath itself.
        let mut registry = self
            .registry
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if Arc::strong_count(&self.gate) != 1 {
            return;
        }
        let exact = registry.entries.get(&self.session_id).is_some_and(|entry| {
            entry.active_release.is_none()
                && entry.retain_until.is_none()
                && Weak::ptr_eq(&entry.gate, &Arc::downgrade(&self.gate))
        });
        if exact {
            registry.entries.remove(&self.session_id);
        }
    }
}

pub(super) struct SessionReleaseGateEntry {
    pub(super) gate: Weak<SessionReleaseGate>,
    pub(super) active_release: Option<Arc<SessionReleaseGate>>,
    pub(super) retain_until: Option<Instant>,
}

#[derive(Default)]
pub(super) struct SessionReleaseGates {
    pub(super) registry: std::sync::Mutex<SessionReleaseGateRegistry>,
}

#[derive(Default)]
pub(super) struct SessionReleaseGateRegistry {
    pub(super) entries: HashMap<String, SessionReleaseGateEntry>,
    pub(super) retained: VecDeque<(String, Instant)>,
}

/// Outlive the complete admitted media envelope and replicated lease work. A
/// completed terminal projection therefore cannot be forgotten while a
/// pre-terminal route decision, body, or commit-unknown renewal can still
/// retain its exact generation.
pub(super) const SESSION_RELEASE_GATE_RETENTION: Duration =
    crate::media_sessions::TERMINAL_PROJECTION_SAFETY_WINDOW;
const MAX_RETAINED_SESSION_RELEASE_GATES: usize = 4_096;
/// Bound distinct capability generations that are only being inspected or
/// prepared. Active release owners and retained terminal generations have
/// their own independent bounds, so an attacker cannot exhaust adoption
/// memory with random UUID GETs while legitimate terminalization still gets
/// an admission path.
pub(super) const MAX_IN_FLIGHT_SESSION_ADOPTION_GATES: usize = 4_096;

pub(super) fn prune_session_release_gates(registry: &mut SessionReleaseGateRegistry) {
    let now = Instant::now();
    while registry.retained.front().is_some_and(|(_, deadline)| {
        *deadline <= now || registry.retained.len() > MAX_RETAINED_SESSION_RELEASE_GATES
    }) {
        let Some((session_id, deadline)) = registry.retained.pop_front() else {
            break;
        };
        let remove = registry.entries.get_mut(&session_id).is_some_and(|entry| {
            if entry.retain_until != Some(deadline) {
                return false;
            }
            entry.active_release = None;
            entry.retain_until = None;
            entry.gate.strong_count() == 0
        });
        if remove {
            registry.entries.remove(&session_id);
        }
    }
    // An authorization may outlive the retained FIFO entry. Once its final
    // Arc drops there is no adoption-token Drop hook to remove the dead Weak;
    // sweep every fully unretained dead generation here so repeated valid
    // sessions cannot grow the map outside either hard admission bound.
    registry.entries.retain(|_, entry| {
        entry.active_release.is_some()
            || entry.retain_until.is_some()
            || entry.gate.strong_count() > 0
    });
}
