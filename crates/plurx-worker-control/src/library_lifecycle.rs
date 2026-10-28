//! A single library pass, entered with a confirmed claim and held admission.
//!
//! Each successful step emits effects in execution order. Process starts work;
//! AwaitWorkJoin consumes its join. In particular, DomainReleased
//! reports completion of the release attempt (including an ambiguous failure),
//! not proof that the durable lease has disappeared. Time is sampled only then.
//! The longest accepted path has six transitions; stale inputs emit no effects.

/// Maximum number of accepted transitions from Initial to Terminal.
pub const MAX_TRANSITIONS: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Initial,
    Claimed,
    DomainAcquired,
    Bound,
    Processing { interrupt: Interrupt },
    Releasing { settlement: SettlementKind },
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Start,
    DomainOutcome { acquired: bool, now_ms: i64 },
    BindOutcome { accepted: bool },
    Interrupt(Interrupt),
    WorkJoined { ok: bool },
    DomainReleased { now_ms: i64, retry_delay_ms: i64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupt {
    QueueLoss,
    DomainLoss,
    Pressure,
    Completed,
}

/// Protocol phase, not ownership of the real guards held by the driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettlementKind {
    Refused,
    Interrupted,
    Retry,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settlement {
    Yield { not_before_ms: i64 },
    Retry { not_before_ms: i64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    AcquireDomain,
    Bind,
    Process,
    RevokeDomain,
    CancelWork,
    AwaitWorkJoin,
    DropPublisher,
    ReleaseDomain,
    Settle(Settlement),
    RetireQueue,
    DropAdmission,
}

/// A small ordered effect sequence; unused array entries are never exposed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effects {
    len: u8,
    items: [Effect; 6],
}

impl Effects {
    pub const fn empty() -> Self {
        Self {
            len: 0,
            items: [Effect::DropAdmission; 6],
        }
    }

    pub fn as_slice(&self) -> &[Effect] {
        &self.items[..usize::from(self.len)]
    }

    const fn one(first: Effect) -> Self {
        Self {
            len: 1,
            items: [
                first,
                Effect::DropAdmission,
                Effect::DropAdmission,
                Effect::DropAdmission,
                Effect::DropAdmission,
                Effect::DropAdmission,
            ],
        }
    }

    const fn two(first: Effect, second: Effect) -> Self {
        Self {
            len: 2,
            items: [
                first,
                second,
                Effect::DropAdmission,
                Effect::DropAdmission,
                Effect::DropAdmission,
                Effect::DropAdmission,
            ],
        }
    }

    const fn three(first: Effect, second: Effect, third: Effect) -> Self {
        Self {
            len: 3,
            items: [
                first,
                second,
                third,
                Effect::DropAdmission,
                Effect::DropAdmission,
                Effect::DropAdmission,
            ],
        }
    }
}

/// The state and event do not belong to an accepted protocol transition.
/// An error has no effect sequence; the caller retains its unchanged state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stale;

/// Decide the next local protocol action without I/O, allocation or loops.
///
/// Initial is only valid after claim confirmation with admission still held.
/// Effects describe attempted operations; Store authority and async completion
/// are environmental inputs, not consequences established by this function.
pub fn step(state: State, event: Event) -> Result<(State, Effects), Stale> {
    use Effect::*;
    let next = match (state, event) {
        (State::Initial, Event::Start) => (State::Claimed, Effects::one(AcquireDomain)),
        (State::Claimed, Event::DomainOutcome { acquired: true, .. }) => {
            (State::DomainAcquired, Effects::one(Bind))
        }
        (
            State::Claimed,
            Event::DomainOutcome {
                acquired: false,
                now_ms,
            },
        ) => (
            State::Terminal,
            Effects::three(
                Settle(Settlement::Yield {
                    not_before_ms: now_ms.saturating_add(5_000),
                }),
                RetireQueue,
                DropAdmission,
            ),
        ),
        (State::DomainAcquired, Event::BindOutcome { accepted: true }) => {
            (State::Bound, Effects::one(Process))
        }
        (State::DomainAcquired, Event::BindOutcome { accepted: false }) => (
            State::Releasing {
                settlement: SettlementKind::Refused,
            },
            Effects::two(DropPublisher, ReleaseDomain),
        ),
        (State::Bound, Event::Interrupt(interrupt)) => (
            State::Processing { interrupt },
            match interrupt {
                Interrupt::Completed => Effects::one(AwaitWorkJoin),
                Interrupt::QueueLoss | Interrupt::DomainLoss | Interrupt::Pressure => {
                    Effects::three(RevokeDomain, CancelWork, AwaitWorkJoin)
                }
            },
        ),
        (State::Processing { interrupt }, Event::WorkJoined { ok }) => (
            State::Releasing {
                settlement: match interrupt {
                    Interrupt::DomainLoss | Interrupt::Pressure => SettlementKind::Interrupted,
                    Interrupt::QueueLoss | Interrupt::Completed if !ok => SettlementKind::Retry,
                    Interrupt::QueueLoss | Interrupt::Completed => SettlementKind::None,
                },
            },
            Effects::two(DropPublisher, ReleaseDomain),
        ),
        (
            State::Releasing { settlement },
            Event::DomainReleased {
                now_ms,
                retry_delay_ms,
            },
        ) => (
            State::Terminal,
            match settlement {
                SettlementKind::Refused => Effects::three(
                    Settle(Settlement::Yield {
                        not_before_ms: now_ms.saturating_add(5_000),
                    }),
                    RetireQueue,
                    DropAdmission,
                ),
                SettlementKind::Interrupted => Effects::three(
                    Settle(Settlement::Yield {
                        not_before_ms: now_ms.saturating_add(1_000),
                    }),
                    RetireQueue,
                    DropAdmission,
                ),
                SettlementKind::Retry => Effects::three(
                    Settle(Settlement::Retry {
                        not_before_ms: now_ms.saturating_add(retry_delay_ms),
                    }),
                    RetireQueue,
                    DropAdmission,
                ),
                SettlementKind::None => Effects::two(RetireQueue, DropAdmission),
            },
        ),
        _ => return Err(Stale),
    };
    Ok(next)
}
