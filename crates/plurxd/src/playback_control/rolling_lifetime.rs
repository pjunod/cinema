//! Original actor-task custody independent of command-sender lifetime.
//! A retirement selector owns no sender, so it can outlive the last rich
//! session without keeping the actor's receiver open or creating a self-wait.
use super::*;

#[cfg(target_os = "linux")]
pub(super) type ActorApplication =
    plurx_core::adaptive_resources::runtime::finite_vod::FiniteApplicationLease;
#[cfg(not(target_os = "linux"))]
pub(super) struct ActorApplication;

/// An application allocation in the already admitted role's finite client
/// partition. It grants no physical entry, closure, or durable refund.
#[derive(Clone)]
pub(crate) struct ActorPayloadAllocation {
    #[cfg(target_os = "linux")]
    slot: Arc<plurx_core::adaptive_resources::runtime::finite_vod::FiniteClientSlot>,
}
impl std::fmt::Debug for ActorPayloadAllocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ActorPayloadAllocation")
    }
}
impl PartialEq for ActorPayloadAllocation {
    fn eq(&self, _: &Self) -> bool {
        // Allocation identity is not playback state. Payload comparisons stay
        // semantic; deep copies still require their own reservation below.
        true
    }
}
impl Eq for ActorPayloadAllocation {}
#[cfg(target_os = "linux")]
impl ActorPayloadAllocation {
    pub(crate) fn reserve_related(&self, bytes: usize) -> Result<Self, ()> {
        Ok(Self {
            slot: self.slot.reserve_related(bytes).map_err(|_| ())?,
        })
    }

    pub(crate) fn validate_actual(&self, bytes: usize) -> Result<(), ()> {
        self.slot.validate_actual(bytes).map_err(|_| ())
    }
    pub(crate) fn into_slot(
        self,
    ) -> Arc<plurx_core::adaptive_resources::runtime::finite_vod::FiniteClientSlot> {
        self.slot
    }
}

#[cfg(target_os = "linux")]
pub(super) struct ActorReplyBudget {
    client: plurx_core::adaptive_resources::runtime::finite_vod::FiniteClientStateLease,
    // Last: the actual immutable budget-owner allocation is destroyed before
    // its partition lease. ResponseBudget owns the original close barrier.
    _metadata: plurx_core::adaptive_resources::runtime::finite_vod::FiniteMetadataLease,
}
#[cfg(target_os = "linux")]
impl ActorReplyBudget {
    pub(super) fn owner_bytes() -> Option<usize> {
        super::finite_memory::arc_requested_bytes::<Self>()
    }
    pub(super) fn reserve(&self, bytes: usize) -> Result<ActorPayloadAllocation, ()> {
        Ok(ActorPayloadAllocation {
            slot: self.client.reserve(bytes).map_err(|_| ())?,
        })
    }
}

/// Original admitted partition and the reservation made BEFORE constructing
/// the actor's two initial ControlState observation-token allocations.
#[cfg(target_os = "linux")]
pub(crate) struct RollingActorAccounting {
    pub(super) budget: Arc<ActorReplyBudget>,
    pub(super) initial_state: ActorPayloadAllocation,
}
#[cfg(target_os = "linux")]
impl RollingActorAccounting {
    pub(crate) fn owner_bytes() -> Option<usize> {
        ActorReplyBudget::owner_bytes()
    }
    pub(crate) fn new(
        metadata: plurx_core::adaptive_resources::runtime::finite_vod::FiniteMetadataLease,
        client: plurx_core::adaptive_resources::runtime::finite_vod::FiniteClientStateLease,
    ) -> Result<Self, ()> {
        let bytes = ControlState::initial_finite_heap_bytes().ok_or(())?;
        let initial_state = ActorPayloadAllocation {
            slot: client.reserve(bytes).map_err(|_| ())?,
        };
        Ok(Self {
            budget: Arc::new(ActorReplyBudget {
                client,
                _metadata: metadata,
            }),
            initial_state,
        })
    }
}

/// One private shared owner of the SAME move-only application lease. Neither
/// this owner nor a payload carrier contains a command Sender or native token.
pub(super) struct ActorApplicationOwner {
    _application: ActorApplication,
}
impl ActorApplicationOwner {
    pub(super) fn retain(application: ActorApplication) -> Arc<Self> {
        Arc::new(Self {
            _application: application,
        })
    }
}

/// Lifetime safety for byte/cardinality components that remain Unknown.
/// The original selector is retained when present; the application owner also
/// survives a lost selector while the original run frame is still draining.
/// No serialization/deserialization or public constructor can mint authority.
#[derive(Clone, Default)]
pub(crate) struct ActorPayloadLifetime {
    _original: Option<Arc<OriginalRollingActorTask>>,
    _application: Option<Arc<ActorApplicationOwner>>,
}
impl ActorPayloadLifetime {
    pub(super) fn from_original(original: &Arc<OriginalRollingActorTask>) -> Self {
        Self {
            _original: Some(Arc::clone(original)),
            _application: original._application.clone(),
        }
    }
    pub(super) fn from_weak(
        original: &Weak<OriginalRollingActorTask>,
        application: &Weak<ActorApplicationOwner>,
    ) -> Self {
        Self {
            _original: original.upgrade(),
            _application: application.upgrade(),
        }
    }
}
impl std::fmt::Debug for ActorPayloadLifetime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ActorPayloadLifetime")
    }
}
impl PartialEq for ActorPayloadLifetime {
    fn eq(&self, _: &Self) -> bool {
        true
    } // Private accounting is not playback state.
}
impl Eq for ActorPayloadLifetime {}

pub(super) struct OriginalRollingActorTask {
    #[cfg_attr(
        all(not(target_os = "linux"), not(test)),
        expect(
            dead_code,
            reason = "retain original actor task and conservative join state on every platform; production native retirement selectors are Linux-only"
        )
    )]
    task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    #[cfg_attr(
        all(not(target_os = "linux"), not(test)),
        expect(
            dead_code,
            reason = "retain original actor task and conservative join state on every platform; production native retirement selectors are Linux-only"
        )
    )]
    registered: bool,
    #[cfg_attr(
        all(not(target_os = "linux"), not(test)),
        expect(
            dead_code,
            reason = "retain original actor task and conservative join state on every platform; production native retirement selectors are Linux-only"
        )
    )]
    failed: AtomicBool,
    // Last: queued actor payloads and its completed JoinHandle allocation
    // remain within the original application's charged lifetime until the
    // original retirement selector itself is destroyed after join.
    _application: Option<Arc<ActorApplicationOwner>>,
}

#[cfg(any(target_os = "linux", test))]
pub(crate) struct RollingControlActorJoin {
    original: Arc<OriginalRollingActorTask>,
}

impl OriginalRollingActorTask {
    pub(super) fn registered(task: tokio::task::JoinHandle<()>) -> Arc<Self> {
        Self::registered_with_application(task, None)
    }

    pub(super) fn registered_with_application(
        task: tokio::task::JoinHandle<()>,
        application: Option<Arc<ActorApplicationOwner>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            task: tokio::sync::Mutex::new(Some(task)),
            registered: true,
            failed: AtomicBool::new(false),
            _application: application,
        })
    }

    #[cfg(test)]
    pub(super) fn unavailable() -> Arc<Self> {
        Arc::new(Self {
            task: tokio::sync::Mutex::new(None),
            registered: false,
            failed: AtomicBool::new(true),
            _application: None,
        })
    }

    #[cfg(any(target_os = "linux", test))]
    pub(super) fn select(original: &Arc<Self>) -> RollingControlActorJoin {
        RollingControlActorJoin {
            original: Arc::clone(original),
        }
    }
}

#[cfg(any(target_os = "linux", test))]
impl RollingControlActorJoin {
    /// Join only the original registered actor. Timeout/cancellation leaves
    /// its handle in place; panic remains a permanent uncertain result. This
    /// is application-task completion, never native settlement authority.
    pub(crate) async fn join(&mut self, deadline: Instant) -> Result<(), ()> {
        if !self.original.registered || self.original.failed.load(Ordering::Acquire) {
            return Err(());
        }
        let mut task = tokio::time::timeout_at(deadline.into(), self.original.task.lock())
            .await
            .map_err(|_| ())?;
        if self.original.failed.load(Ordering::Acquire) {
            return Err(());
        }
        if let Some(original) = task.as_mut() {
            match tokio::time::timeout_at(deadline.into(), original).await {
                Err(_) => return Err(()),
                Ok(Err(_)) => {
                    self.original.failed.store(true, Ordering::Release);
                    task.take();
                    return Err(());
                }
                Ok(Ok(())) => {
                    task.take();
                }
            }
        }
        Ok(())
    }
}

pub(super) struct OriginalRollingActorInput {
    actor: Option<RollingControlActor>,
    receiver: Option<tokio::sync::mpsc::Receiver<RollingControlEnvelope>>,
    exit: Option<RollingActorExitFence>,
    // Last: input/actor/receiver destruction precedes application release.
    _application: Option<Arc<ActorApplicationOwner>>,
}

impl OriginalRollingActorInput {
    pub(super) fn new(
        actor: RollingControlActor,
        receiver: tokio::sync::mpsc::Receiver<RollingControlEnvelope>,
        application: Option<Arc<ActorApplicationOwner>>,
    ) -> Self {
        let exit = RollingActorExitFence {
            retired: Arc::clone(&actor.retired_fence),
            producer_transition: Arc::clone(&actor.producer_transition),
            flow_sync: Arc::clone(&actor.flow_sync),
            producer_events: Arc::clone(&actor.producer_events),
            hooks: Arc::clone(&actor.hooks),
        };
        Self {
            actor: Some(actor),
            receiver: Some(receiver),
            exit: Some(exit),
            _application: application,
        }
    }
}

impl Drop for OriginalRollingActorInput {
    fn drop(&mut self) {
        // Also fence an actor aborted before its start latch first polls.
        // Receiver closure must never become visible before retirement.
        drop(self.exit.take());
        drop(self.actor.take());
        drop(self.receiver.take());
    }
}

/// The exact future used by spawn. Constructing it with absent inputs below
/// measures the same compiler layout without allocating an actor or channel.
pub(super) async fn run_original_actor(
    start: Option<tokio::sync::oneshot::Receiver<Weak<OriginalRollingActorTask>>>,
    input: Option<OriginalRollingActorInput>,
) {
    let (Some(start), Some(mut input)) = (start, input) else {
        return;
    };
    if let Ok(original) = start.await {
        let mut actor = input.actor.take().expect("original registered actor");
        actor.original_owner = original;
        let receiver = input.receiver.take().expect("original registered receiver");
        let exit = input.exit.take().expect("original actor exit fence");
        actor.run(receiver, exit).await;
    }
    // Explicitly keep the run frame's SAME lease owner across actor.run;
    // losing all selectors cannot release it while receiver draining runs.
    drop(input);
}

/// Only the task's fixed requested layout. Actor payloads, mailbox blocks,
/// command heaps, Tokio task allocation and runtime baseline are separate.
#[cfg(target_os = "linux")]
pub(super) fn original_actor_frame_bytes() -> Option<usize> {
    std::mem::size_of_val(&run_original_actor(None, None))
        .checked_add(super::finite_memory::arc_requested_bytes::<
            OriginalRollingActorTask,
        >()?)?
        // Exact new private Arc requested layout, charged before creation.
        .checked_add(super::finite_memory::arc_requested_bytes::<
            ActorApplicationOwner,
        >()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpolled_actor_retirement_precedes_receiver_destruction() {
        let retired = Arc::new(AtomicBool::new(false));
        let actor =
            RollingControlActor::new(Instant::now(), "unpolled-actor-test", Arc::clone(&retired));
        let (sender, receiver) = tokio::sync::mpsc::channel(ROLLING_ACTOR_MAILBOX_CAPACITY);
        let input = OriginalRollingActorInput::new(actor, receiver, None);
        drop(run_original_actor(None, Some(input)));
        assert!(retired.load(Ordering::Acquire));
        assert!(sender.is_closed());
    }

    #[tokio::test]
    async fn actor_join_waits_for_last_command_owner_without_retaining_a_sender() {
        let (control, registration) =
            RollingControlHandle::spawn_prepublication_producer("actor-owner-lifetime-test");
        let alias = control.clone();
        let mut original = control.original_actor_join();
        drop(registration);
        drop(control);
        assert!(original
            .join(Instant::now() + Duration::from_millis(20))
            .await
            .is_err());
        assert!(
            original.original.task.lock().await.is_some(),
            "timed-out join lost the original task handle"
        );
        drop(alias);
        original
            .join(Instant::now() + Duration::from_secs(2))
            .await
            .expect("last sender drop lets the original actor finish");
        assert!(original.original.task.lock().await.is_none());
    }

    #[tokio::test]
    async fn failed_actor_join_never_turns_into_success_on_retry() {
        let task = tokio::spawn(async { panic!("original actor fixture failure") });
        let mut original =
            OriginalRollingActorTask::select(&OriginalRollingActorTask::registered(task));
        assert!(original
            .join(Instant::now() + Duration::from_secs(2))
            .await
            .is_err());
        assert!(original
            .join(Instant::now() + Duration::from_secs(2))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn snapshot_alias_retains_original_selector_without_retaining_sender() {
        // Real actor/transport lifetime; no physical lease or native proof is
        // minted by this portable fixture.
        let control = RollingControlHandle::spawn("snapshot-owner-lifetime-fixture");
        let weak = Arc::downgrade(&control.original_actor);
        let mut original = control.original_actor_join();
        let snapshot = control.snapshot().await.expect("live actor snapshot");
        let alias = snapshot.clone();
        drop(control);
        original
            .join(Instant::now() + Duration::from_secs(2))
            .await
            .expect("snapshot retains no Sender and actor must finish");
        drop(original);
        assert!(
            weak.upgrade().is_some(),
            "original selector escaped its snapshot"
        );
        drop(snapshot);
        assert!(
            weak.upgrade().is_some(),
            "deep alias lost its original owner"
        );
        drop(alias);
        assert!(
            weak.upgrade().is_none(),
            "last payload retained an actor cycle"
        );
    }

    #[tokio::test]
    async fn decision_alias_retains_original_selector_without_actor_cycle() {
        // The actor stores plain decision Arcs. Only this outward alias owns
        // a selector carrier; no native entry or physical receipt is involved.
        let control = RollingControlHandle::spawn("decision-owner-lifetime-fixture");
        let weak = Arc::downgrade(&control.original_actor);
        let mut original = control.original_actor_join();
        assert!(
            control
                .install_producer_decision_for_test(ProducerDecision::Fail {
                    decision_sequence: 1,
                    failed_attempt: 0,
                    reason: ProducerDecisionReason::Unsupported,
                    proposal: None,
                    cleanup: ProducerFailureCleanup {
                        kind: ProducerFailureCleanupKind::ProducerFailureCleanup,
                        cleanup_policy: CleanupPolicy::DiscardPrepublication,
                    },
                })
                .await
        );
        let decision = control
            .poll_producer_decision(0)
            .await
            .expect("live decision transport");
        assert!(matches!(decision, ProducerDecisionPoll::Decision(_)));
        let alias = decision.clone();
        drop(control);
        original
            .join(Instant::now() + Duration::from_secs(2))
            .await
            .expect("decision alias retains no Sender");
        drop(original);
        assert!(weak.upgrade().is_some());
        drop(decision);
        assert!(weak.upgrade().is_some());
        drop(alias);
        assert!(
            weak.upgrade().is_none(),
            "actor-retained decision formed a cycle"
        );
    }
}
