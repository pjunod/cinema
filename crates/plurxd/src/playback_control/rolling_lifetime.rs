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
    _application: Option<ActorApplication>,
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
        application: Option<ActorApplication>,
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
}

impl OriginalRollingActorInput {
    pub(super) fn new(
        actor: RollingControlActor,
        receiver: tokio::sync::mpsc::Receiver<RollingControlEnvelope>,
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
    start: Option<tokio::sync::oneshot::Receiver<()>>,
    input: Option<OriginalRollingActorInput>,
) {
    let (Some(start), Some(mut input)) = (start, input) else {
        return;
    };
    if start.await.is_ok() {
        let actor = input.actor.take().expect("original registered actor");
        let receiver = input.receiver.take().expect("original registered receiver");
        let exit = input.exit.take().expect("original actor exit fence");
        actor.run(receiver, exit).await;
    }
}

/// Only the task's fixed requested layout. Actor payloads, mailbox blocks,
/// command heaps, Tokio task allocation and runtime baseline are separate.
#[cfg(target_os = "linux")]
pub(super) fn original_actor_frame_bytes() -> Option<usize> {
    std::mem::size_of_val(&run_original_actor(None, None))
        .checked_add(std::mem::size_of::<OriginalRollingActorTask>())?
        .checked_add(2 * std::mem::size_of::<usize>())
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
        let input = OriginalRollingActorInput::new(actor, receiver);
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
}
