//! Production interpreter of the library lifecycle's ordered effects.
//!
//! Real resources and asynchronous operations belong to `Environment`. This
//! module owns their control order. Numeric settlement payloads are transported
//! without branching; `control` is the finite quotient used by graph inventory.

use core::future::{poll_fn, Future};
use core::pin::pin;
use core::task::Poll;

use crate::library_lifecycle::{
    self as lifecycle, Effect, Event, Interrupt, Settlement, SettlementKind, State,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Work {
    Idle,
    Running,
    Completed { ok: bool },
    Joined { ok: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Held {
    pub admission: bool,
    pub queue: bool,
    pub domain: bool,
    pub publisher: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    AcquireDomain,
    Bind,
    PollWork,
    RevokeDomain,
    CancelWork,
    JoinWork,
    DropPublisher,
    ReleaseDomain { settlement: SettlementKind },
    Settle(Settlement),
    RetireQueue,
    DropAdmission,
}

/// The finite request class; only Settle's numeric payload is erased.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    AcquireDomain,
    Bind,
    PollWork,
    RevokeDomain,
    CancelWork,
    JoinWork,
    DropPublisher,
    ReleaseRefused,
    ReleaseInterrupted,
    ReleaseRetry,
    ReleaseNone,
    SettleYield,
    SettleRetry,
    RetireQueue,
    DropAdmission,
}

impl Request {
    pub fn kind(self) -> RequestKind {
        match self {
            Self::AcquireDomain => RequestKind::AcquireDomain,
            Self::Bind => RequestKind::Bind,
            Self::PollWork => RequestKind::PollWork,
            Self::RevokeDomain => RequestKind::RevokeDomain,
            Self::CancelWork => RequestKind::CancelWork,
            Self::JoinWork => RequestKind::JoinWork,
            Self::DropPublisher => RequestKind::DropPublisher,
            Self::ReleaseDomain { settlement } => match settlement {
                SettlementKind::Refused => RequestKind::ReleaseRefused,
                SettlementKind::Interrupted => RequestKind::ReleaseInterrupted,
                SettlementKind::Retry => RequestKind::ReleaseRetry,
                SettlementKind::None => RequestKind::ReleaseNone,
            },
            Self::Settle(Settlement::Yield { .. }) => RequestKind::SettleYield,
            Self::Settle(Settlement::Retry { .. }) => RequestKind::SettleRetry,
            Self::RetireQueue => RequestKind::RetireQueue,
            Self::DropAdmission => RequestKind::DropAdmission,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkOutcome {
    QueueLoss,
    DomainLoss,
    Pressure,
    Completed { ok: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    Domain {
        acquired: bool,
        now_ms: i64,
    },
    Bound {
        accepted: bool,
    },
    Work(WorkOutcome),
    Joined {
        ok: bool,
    },
    /// Completion of a local release attempt, including an ambiguous error.
    Released {
        now_ms: i64,
        retry_delay_ms: i64,
    },
    Done,
}

/// Finite reply classification. Control transitions receive this type only;
/// they cannot inspect a clock value or a settlement payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyTag {
    DomainRefused,
    DomainAcquired,
    BindRefused,
    BindAccepted,
    QueueLoss,
    DomainLoss,
    Pressure,
    WorkFailed,
    WorkSucceeded,
    JoinedFailed,
    JoinedSucceeded,
    ReleaseReturned,
    Done,
}

impl Reply {
    pub fn tag(self) -> ReplyTag {
        match self {
            Self::Domain {
                acquired: false, ..
            } => ReplyTag::DomainRefused,
            Self::Domain { acquired: true, .. } => ReplyTag::DomainAcquired,
            Self::Bound { accepted: false } => ReplyTag::BindRefused,
            Self::Bound { accepted: true } => ReplyTag::BindAccepted,
            Self::Work(WorkOutcome::QueueLoss) => ReplyTag::QueueLoss,
            Self::Work(WorkOutcome::DomainLoss) => ReplyTag::DomainLoss,
            Self::Work(WorkOutcome::Pressure) => ReplyTag::Pressure,
            Self::Work(WorkOutcome::Completed { ok: false }) => ReplyTag::WorkFailed,
            Self::Work(WorkOutcome::Completed { ok: true }) => ReplyTag::WorkSucceeded,
            Self::Joined { ok: false } => ReplyTag::JoinedFailed,
            Self::Joined { ok: true } => ReplyTag::JoinedSucceeded,
            Self::Released { .. } => ReplyTag::ReleaseReturned,
            Self::Done => ReplyTag::Done,
        }
    }

    fn event(self) -> Option<Event> {
        match self {
            Self::Domain { acquired, now_ms } => Some(Event::DomainOutcome { acquired, now_ms }),
            Self::Bound { accepted } => Some(Event::BindOutcome { accepted }),
            Self::Work(outcome) => Some(Event::Interrupt(match outcome {
                WorkOutcome::QueueLoss => Interrupt::QueueLoss,
                WorkOutcome::DomainLoss => Interrupt::DomainLoss,
                WorkOutcome::Pressure => Interrupt::Pressure,
                WorkOutcome::Completed { .. } => Interrupt::Completed,
            })),
            Self::Joined { ok } => Some(Event::WorkJoined { ok }),
            Self::Released {
                now_ms,
                retry_delay_ms,
            } => Some(Event::DomainReleased {
                now_ms,
                retry_delay_ms,
            }),
            Self::Done => None,
        }
    }
}

/// Every finite reply class has one concrete representative for inventory.
/// Unlike an arithmetic proof, this list does not quantify over i64 values.
pub const REPLY_CLASSES: [Reply; 13] = [
    Reply::Domain {
        acquired: false,
        now_ms: 0,
    },
    Reply::Domain {
        acquired: true,
        now_ms: 0,
    },
    Reply::Bound { accepted: false },
    Reply::Bound { accepted: true },
    Reply::Work(WorkOutcome::QueueLoss),
    Reply::Work(WorkOutcome::DomainLoss),
    Reply::Work(WorkOutcome::Pressure),
    Reply::Work(WorkOutcome::Completed { ok: false }),
    Reply::Work(WorkOutcome::Completed { ok: true }),
    Reply::Joined { ok: false },
    Reply::Joined { ok: true },
    Reply::Released {
        now_ms: 0,
        retry_delay_ms: 0,
    },
    Reply::Done,
];

/// An invalid environment reply is rejected without changing the driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidReply;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Control {
    pub controller: State,
    pub held: Held,
    pub work: Work,
    pub revoked: bool,
    pub cancelled: bool,
    pub pending: Option<RequestKind>,
    pub remaining: [Option<RequestKind>; 6],
}

impl Control {
    pub fn terminal(self) -> bool {
        self.pending.is_none()
    }
}

/// Concrete data is kept outside Control. It is never passed to
/// next_control; the lifecycle's computed settlement is forwarded unchanged.
#[derive(Clone, Copy, Debug)]
struct Payload {
    settlement: Option<Settlement>,
}

#[derive(Clone, Copy, Debug)]
pub struct Driver {
    control: Control,
    payload: Payload,
}

fn effect_kind(effect: Effect, controller: State) -> RequestKind {
    match effect {
        Effect::AcquireDomain => RequestKind::AcquireDomain,
        Effect::Bind => RequestKind::Bind,
        Effect::Process => RequestKind::PollWork,
        Effect::RevokeDomain => RequestKind::RevokeDomain,
        Effect::CancelWork => RequestKind::CancelWork,
        Effect::AwaitWorkJoin => RequestKind::JoinWork,
        Effect::DropPublisher => RequestKind::DropPublisher,
        Effect::ReleaseDomain => match controller {
            State::Releasing {
                settlement: SettlementKind::Refused,
            } => RequestKind::ReleaseRefused,
            State::Releasing {
                settlement: SettlementKind::Interrupted,
            } => RequestKind::ReleaseInterrupted,
            State::Releasing {
                settlement: SettlementKind::Retry,
            } => RequestKind::ReleaseRetry,
            State::Releasing {
                settlement: SettlementKind::None,
            } => RequestKind::ReleaseNone,
            _ => unreachable!("only Releasing emits domain release"),
        },
        Effect::Settle(Settlement::Yield { .. }) => RequestKind::SettleYield,
        Effect::Settle(Settlement::Retry { .. }) => RequestKind::SettleRetry,
        Effect::RetireQueue => RequestKind::RetireQueue,
        Effect::DropAdmission => RequestKind::DropAdmission,
    }
}

fn install(control: &mut Control, event: Event) -> Result<(), InvalidReply> {
    let (next, effects) = lifecycle::step(control.controller, event).map_err(|_| InvalidReply)?;
    control.controller = next;
    control.remaining = [None; 6];
    for (slot, effect) in control.remaining.iter_mut().zip(effects.as_slice()) {
        *slot = Some(effect_kind(*effect, next));
    }
    Ok(())
}

fn emit(control: &mut Control) {
    control.pending = control.remaining[0];
    control.remaining.rotate_left(1);
    control.remaining[5] = None;
    if control.pending == Some(RequestKind::PollWork) {
        control.work = Work::Running;
    }
}

impl Control {
    pub fn initial() -> Self {
        let mut control = Self {
            controller: State::Initial,
            held: Held {
                admission: true,
                queue: true,
                domain: false,
                publisher: false,
            },
            work: Work::Idle,
            revoked: false,
            cancelled: false,
            pending: None,
            remaining: [None; 6],
        };
        install(&mut control, Event::Start).expect("initial lifecycle accepts Start");
        emit(&mut control);
        control
    }
}

/// The actual production control transition. Its complete input type is finite
/// and contains no payload, generic payload type, i64, or external callback.
/// The tag-only lifecycle call below chooses the protocol/effect tags; a
/// separate transport call retains concrete arithmetic from that same body.
pub fn next_control(mut control: Control, reply: ReplyTag) -> Result<Control, InvalidReply> {
    use ReplyTag as R;
    use RequestKind as Q;
    let event = match (control.pending, reply) {
        (Some(Q::AcquireDomain), R::DomainRefused | R::DomainAcquired) => {
            let acquired = reply == R::DomainAcquired;
            control.held.domain = acquired;
            Some(Event::DomainOutcome {
                acquired,
                now_ms: 0,
            })
        }
        (Some(Q::Bind), R::BindRefused | R::BindAccepted) => {
            control.held.publisher = true;
            Some(Event::BindOutcome {
                accepted: reply == R::BindAccepted,
            })
        }
        (
            Some(Q::PollWork),
            R::QueueLoss | R::DomainLoss | R::Pressure | R::WorkFailed | R::WorkSucceeded,
        ) => {
            let interrupt = match reply {
                R::QueueLoss => Interrupt::QueueLoss,
                R::DomainLoss => Interrupt::DomainLoss,
                R::Pressure => Interrupt::Pressure,
                R::WorkFailed | R::WorkSucceeded => {
                    control.work = Work::Completed {
                        ok: reply == R::WorkSucceeded,
                    };
                    Interrupt::Completed
                }
                _ => unreachable!("matched work tag"),
            };
            Some(Event::Interrupt(interrupt))
        }
        (Some(Q::JoinWork), R::JoinedFailed | R::JoinedSucceeded) => {
            let ok = reply == R::JoinedSucceeded;
            if matches!(control.work, Work::Completed { ok: completed } if completed != ok) {
                return Err(InvalidReply);
            }
            control.work = Work::Joined { ok };
            Some(Event::WorkJoined { ok })
        }
        (
            Some(Q::ReleaseRefused | Q::ReleaseInterrupted | Q::ReleaseRetry | Q::ReleaseNone),
            R::ReleaseReturned,
        ) => {
            control.held.domain = false;
            Some(Event::DomainReleased {
                now_ms: 0,
                retry_delay_ms: 0,
            })
        }
        (Some(Q::RevokeDomain), R::Done) => {
            control.revoked = true;
            None
        }
        (Some(Q::CancelWork), R::Done) => {
            control.cancelled = true;
            None
        }
        (Some(Q::DropPublisher), R::Done) => {
            control.held.publisher = false;
            None
        }
        (Some(Q::SettleYield | Q::SettleRetry), R::Done) => None,
        (Some(Q::RetireQueue), R::Done) => {
            control.held.queue = false;
            None
        }
        (Some(Q::DropAdmission), R::Done) => {
            control.held.admission = false;
            None
        }
        _ => return Err(InvalidReply),
    };
    if let Some(event) = event {
        install(&mut control, event)?;
    }
    emit(&mut control);
    Ok(control)
}

impl Default for Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Driver {
    /// Entry requires a confirmed queue claim and retained physical admission.
    pub fn new() -> Self {
        Self {
            control: Control::initial(),
            payload: Payload { settlement: None },
        }
    }

    pub fn request(&self) -> Option<Request> {
        Some(match self.control.pending? {
            RequestKind::AcquireDomain => Request::AcquireDomain,
            RequestKind::Bind => Request::Bind,
            RequestKind::PollWork => Request::PollWork,
            RequestKind::RevokeDomain => Request::RevokeDomain,
            RequestKind::CancelWork => Request::CancelWork,
            RequestKind::JoinWork => Request::JoinWork,
            RequestKind::DropPublisher => Request::DropPublisher,
            RequestKind::ReleaseRefused => Request::ReleaseDomain {
                settlement: SettlementKind::Refused,
            },
            RequestKind::ReleaseInterrupted => Request::ReleaseDomain {
                settlement: SettlementKind::Interrupted,
            },
            RequestKind::ReleaseRetry => Request::ReleaseDomain {
                settlement: SettlementKind::Retry,
            },
            RequestKind::ReleaseNone => Request::ReleaseDomain {
                settlement: SettlementKind::None,
            },
            RequestKind::SettleYield | RequestKind::SettleRetry => Request::Settle(
                self.payload
                    .settlement
                    .expect("lifecycle emitted the concrete settlement"),
            ),
            RequestKind::RetireQueue => Request::RetireQueue,
            RequestKind::DropAdmission => Request::DropAdmission,
        })
    }

    /// The concrete lifecycle settlement retained until its source lifetime ends.
    /// This read-only view does not alter control or manufacture driver state.
    pub fn settlement(&self) -> Option<Settlement> {
        self.payload.settlement
    }

    pub fn control(&self) -> Control {
        self.control
    }

    /// Control depends only on reply.tag(). Concrete events go unchanged to the
    /// already proved lifecycle body, and its computed Settlement goes unchanged
    /// to the adapter. Tag consistency is checked before committing either part.
    /// Rejection leaves both control and concrete payload unchanged.
    pub fn drive(&mut self, reply: Reply) -> Result<Option<Request>, InvalidReply> {
        let next = next_control(self.control, reply.tag())?;
        let mut payload = self.payload;
        if let Some(event) = reply.event() {
            let (controller, effects) = lifecycle::step(self.control.controller, event)
                .expect("accepted reply tag admits the same concrete lifecycle event");
            assert_eq!(
                controller, next.controller,
                "numeric payload must not alter lifecycle phase"
            );
            let mut tags = [None; 6];
            payload.settlement = None;
            for (slot, effect) in tags.iter_mut().zip(effects.as_slice()) {
                *slot = Some(effect_kind(*effect, controller));
                if let Effect::Settle(settlement) = effect {
                    payload.settlement = Some(*settlement);
                }
            }
            let mut expected = next.remaining;
            expected.rotate_right(1);
            expected[0] = next.pending;
            assert_eq!(
                tags, expected,
                "numeric payload must not alter lifecycle effect tags"
            );
        }
        self.control = next;
        self.payload = payload;
        Ok(self.request())
    }
}

/// The adapter performs real operations, including cancellation followed by
/// joining the same work future. It must return only the pending request's
/// reply class. Backend refusal and ambiguity are outcomes, not protocol errors.
pub trait Environment {
    fn perform(&mut self, request: Request) -> impl Future<Output = Reply> + Send;
}

/// The actual production executor, also usable with a deterministic adapter.
pub async fn run_driver(environment: &mut impl Environment) -> Result<(), InvalidReply> {
    let mut driver = Driver::new();
    while let Some(request) = driver.request() {
        let reply = environment.perform(request).await;
        driver.drive(reply)?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Selected<T> {
    QueueLoss,
    DomainLoss,
    Work(T),
    Tick,
}

/// Loss precedence is preserved, and every select poll that does not terminate
/// on loss polls work before a ready pressure tick. This priority is shared by
/// production and the bounded-ready-source regression; task fairness alone
/// does not make a tick-first branch fair.
pub async fn select_work<Q, D, W, P, T>(
    queue_loss: Q,
    domain_loss: D,
    work: W,
    pressure: P,
) -> Selected<T>
where
    Q: Future<Output = ()>,
    D: Future<Output = ()>,
    W: Future<Output = T>,
    P: Future,
{
    let mut queue_loss = pin!(queue_loss);
    let mut domain_loss = pin!(domain_loss);
    let mut work = pin!(work);
    let mut pressure = pin!(pressure);
    poll_fn(|cx| {
        if queue_loss.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Selected::QueueLoss);
        }
        if domain_loss.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Selected::DomainLoss);
        }
        if let Poll::Ready(value) = work.as_mut().poll(cx) {
            return Poll::Ready(Selected::Work(value));
        }
        if pressure.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Selected::Tick);
        }
        Poll::Pending
    })
    .await
}
