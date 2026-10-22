// Exact-body local safety proofs. No stubs, contracts, assumptions or copied step.
// Accepted steps form a DAG of depth six. Rejected events leave caller state
// unchanged, so deleting any number of stale stutters preserves every effect
// prefix. We enumerate every accepted trace and inspect every effect prefix;
// we do not assert that arbitrary event streams eventually terminate.
use crate::library_lifecycle::*;

// Call the pinned Kani AssertHook directly so literal theorem labels survive
// no_std analysis. Core formatting-based assert/expect macros lose their text
// on this frontend and pull irrelevant panic-formatting machinery into proofs.

fn interrupt() -> Interrupt {
    match kani::any::<u8>() % 4 {
        0 => Interrupt::QueueLoss,
        1 => Interrupt::DomainLoss,
        2 => Interrupt::Pressure,
        _ => Interrupt::Completed,
    }
}

fn settlement_kind() -> SettlementKind {
    match kani::any::<u8>() % 4 {
        0 => SettlementKind::Refused,
        1 => SettlementKind::Interrupted,
        2 => SettlementKind::Retry,
        _ => SettlementKind::None,
    }
}

fn state() -> State {
    match kani::any::<u8>() % 7 {
        0 => State::Initial,
        1 => State::Claimed,
        2 => State::DomainAcquired,
        3 => State::Bound,
        4 => State::Processing {
            interrupt: interrupt(),
        },
        5 => State::Releasing {
            settlement: settlement_kind(),
        },
        _ => State::Terminal,
    }
}

fn event() -> Event {
    match kani::any::<u8>() % 6 {
        0 => Event::Start,
        1 => Event::DomainOutcome {
            acquired: kani::any(),
            now_ms: kani::any(),
        },
        2 => Event::BindOutcome {
            accepted: kani::any(),
        },
        3 => Event::Interrupt(interrupt()),
        4 => Event::WorkJoined { ok: kani::any() },
        _ => Event::DomainReleased {
            now_ms: kani::any(),
            retry_delay_ms: kani::any(),
        },
    }
}

// Input generation chooses only the event *kind*. Outcomes and all i64 values
// remain symbolic. This is not a transition implementation or desired result.
fn enabled_event(current: State) -> Event {
    match current {
        State::Initial => Event::Start,
        State::Claimed => Event::DomainOutcome {
            acquired: kani::any(),
            now_ms: kani::any(),
        },
        State::DomainAcquired => Event::BindOutcome {
            accepted: kani::any(),
        },
        State::Bound => Event::Interrupt(interrupt()),
        State::Processing { .. } => Event::WorkJoined { ok: kani::any() },
        State::Releasing { .. } => Event::DomainReleased {
            now_ms: kani::any(),
            retry_delay_ms: kani::any(),
        },
        State::Terminal => Event::Start,
    }
}

// A proof-only Result guard. Each call expands a reached assertion with its
// literal theorem label. Err returns a typed placeholder only after that
// assertion has failed; this is never an assumption or production fallback.
macro_rules! accepted_step {
    ($current:expr, $input:expr, $label:literal $(,)?) => {{
        let current = $current;
        let result = step(current, $input);
        kani::assert(result.is_ok(), $label);
        match result {
            Ok(next) => next,
            Err(Stale) => (current, Effects::empty()),
        }
    }};
}

fn rank(current: State) -> u8 {
    match current {
        State::Initial => 0,
        State::Claimed => 1,
        State::DomainAcquired => 2,
        State::Bound => 3,
        State::Processing { .. } => 4,
        State::Releasing { .. } => 5,
        State::Terminal => 6,
    }
}

fn accepted_pair(current: State, input: Event) -> bool {
    matches!(
        (current, input),
        (State::Initial, Event::Start)
            | (State::Claimed, Event::DomainOutcome { .. })
            | (State::DomainAcquired, Event::BindOutcome { .. })
            | (State::Bound, Event::Interrupt(_))
            | (State::Processing { .. }, Event::WorkJoined { .. })
            | (State::Releasing { .. }, Event::DomainReleased { .. })
    )
}

#[kani::proof]
#[kani::unwind(8)]
fn t1_all_typed_inputs_total_deterministic() {
    let current = state();
    let input = event();
    let first = step(current, input);
    kani::assert(
        (first) == (step(current, input)),
        "T1 deterministic shared step",
    );
    kani::assert(
        (first.is_ok()) == (accepted_pair(current, input)),
        "T1 exact accepted event kinds",
    );
    if let Ok((next, effects)) = first {
        kani::assert(rank(next) > rank(current), "T1 accepted DAG rank increases");
        kani::assert(
            effects.as_slice().len() <= 6,
            "T1 bounded well formed effects",
        );
    }
    // State and Event are Copy values passed by value. Err(Stale) has no
    // Effects field, and cannot mutate the caller's retained state.
}

#[kani::proof]
#[kani::unwind(8)]
fn t2_every_reachable_prefix_gates_process() {
    let mut current = State::Initial;
    let mut acquired = false;
    let mut accepted_bind = false;
    let mut processes = 0u8;
    let mut starts = 0u8;
    for _ in 0..MAX_TRANSITIONS {
        if current == State::Terminal {
            break;
        }
        let input = enabled_event(current);
        if matches!(input, Event::DomainOutcome { acquired: true, .. }) {
            acquired = true;
        }
        if matches!(input, Event::BindOutcome { accepted: true }) {
            accepted_bind = true;
        }
        let (next, effects) =
            accepted_step!(current, input, "T2 reachable protocol event accepted");
        kani::assert(rank(next) > rank(current), "T2 accepted DAG rank increases");
        for effect in effects.as_slice() {
            match effect {
                Effect::AcquireDomain => {
                    starts += 1;
                    kani::assert(
                        current == State::Initial && starts == 1,
                        "T2 one shot initialization",
                    );
                }
                Effect::Process => {
                    kani::assert(
                        acquired && accepted_bind && processes == 0,
                        "T2 process requires acquired domain and accepted bind",
                    );
                    processes += 1;
                }
                _ => {}
            }
        }
        current = next;
    }
    kani::assert(
        (current) == (State::Terminal),
        "T2 complete accepted trace bound",
    );
}

#[kani::proof]
#[kani::unwind(8)]
fn t3_every_prefix_retains_resources_terminal_cleanup_ordered() {
    let mut current = State::Initial;
    let mut admission = true;
    let mut queue = true;
    let mut domain = false;
    let mut publisher = false;
    let mut work = false;
    let mut processed = false;
    let mut join_requested = false;
    let mut joined = false;
    let mut revoked = false;
    let mut release_requested = false;
    let mut admission_drops = 0u8;
    for _ in 0..MAX_TRANSITIONS {
        if current == State::Terminal {
            break;
        }
        let input = enabled_event(current);
        match input {
            Event::DomainOutcome { acquired: true, .. } => domain = true,
            Event::WorkJoined { .. } => {
                kani::assert(
                    work && join_requested && admission && domain && publisher,
                    "T3 resources retained through work join",
                );
                work = false;
                joined = true;
            }
            Event::DomainReleased { .. } => {
                kani::assert(
                    release_requested && domain && !publisher && !work && admission,
                    "T3 release completes while admission retained",
                );
                domain = false;
            }
            _ => {}
        }
        let (next, effects) =
            accepted_step!(current, input, "T3 reachable protocol event accepted");
        kani::assert(rank(next) > rank(current), "T3 accepted DAG rank increases");
        for (index, effect) in effects.as_slice().iter().enumerate() {
            kani::assert(admission, "T3 admission is last effect");
            if matches!(
                effect,
                Effect::AcquireDomain | Effect::Bind | Effect::Process | Effect::ReleaseDomain
            ) {
                kani::assert(
                    (index + 1) == (effects.as_slice().len()),
                    "T3 outcome-producing effect is last in its list",
                );
            }
            match effect {
                Effect::AcquireDomain => kani::assert(
                    queue && !domain && !publisher,
                    "T3 acquire retains admission and queue",
                ),
                Effect::Bind => {
                    kani::assert(domain && queue && !publisher, "T3 bind owns domain");
                    publisher = true;
                }
                Effect::Process => {
                    kani::assert(
                        domain && publisher && queue && !work && !processed,
                        "T3 process resource ownership",
                    );
                    work = true;
                    processed = true;
                }
                Effect::RevokeDomain => {
                    kani::assert(
                        work && domain && publisher && !join_requested,
                        "T3 revoke before join",
                    );
                    revoked = true;
                }
                Effect::CancelWork => {
                    kani::assert(work && revoked && !join_requested, "T3 cancel before join")
                }
                Effect::AwaitWorkJoin => {
                    kani::assert(
                        work && domain && publisher && !join_requested,
                        "T3 await before resource release",
                    );
                    join_requested = true;
                }
                Effect::DropPublisher => {
                    kani::assert(
                        publisher && domain && !work && (!processed || joined),
                        "T3 publisher dropped after work joined",
                    );
                    publisher = false;
                }
                Effect::ReleaseDomain => {
                    kani::assert(
                        domain && !publisher && !work && !release_requested,
                        "T3 publisher dropped before domain release",
                    );
                    release_requested = true;
                }
                Effect::Settle(_) => kani::assert(
                    queue && !domain && !publisher && !work,
                    "T3 settlement after local release completion",
                ),
                Effect::RetireQueue => {
                    kani::assert(
                        queue && !domain && !publisher && !work,
                        "T3 retire after release",
                    );
                    queue = false;
                }
                Effect::DropAdmission => {
                    kani::assert(
                        !queue
                            && !domain
                            && !publisher
                            && !work
                            && index + 1 == effects.as_slice().len(),
                        "T3 queue retired before admission dropped last",
                    );
                    admission = false;
                    admission_drops += 1;
                }
            }
        }
        if next != State::Terminal {
            kani::assert(
                admission && queue,
                "T3 nonterminal prefix retains admission and queue",
            );
        } else {
            kani::assert(
                !admission && !queue && !domain && !publisher && !work && admission_drops == 1,
                "T3 terminal cleanup exactly once",
            );
        }
        current = next;
    }
    kani::assert(
        (current) == (State::Terminal),
        "T3 complete accepted trace bound",
    );
}

fn release_effects(kind: SettlementKind, now: i64, delay: i64) -> Effects {
    accepted_step!(
        State::Releasing { settlement: kind },
        Event::DomainReleased {
            now_ms: now,
            retry_delay_ms: delay,
        },
        "T4 T5 release event accepted"
    )
    .1
}

#[kani::proof]
#[kani::unwind(8)]
fn t4_refusal_interrupt_and_completion_settlement_policy() {
    let now: i64 = kani::any();
    let delay: i64 = kani::any();
    let cause = interrupt();
    let ok: bool = kani::any();
    let (_, refused) = accepted_step!(
        State::Claimed,
        Event::DomainOutcome {
            acquired: false,
            now_ms: now,
        },
        "T4 reachable protocol event accepted"
    );
    kani::assert(
        (refused.as_slice())
            == (&[
                Effect::Settle(Settlement::Yield {
                    not_before_ms: now.saturating_add(5_000),
                }),
                Effect::RetireQueue,
                Effect::DropAdmission,
            ]),
        "T4 domain refusal yields without retry",
    );
    let (releasing, refused_bind) = accepted_step!(
        State::DomainAcquired,
        Event::BindOutcome { accepted: false },
        "T4 reachable protocol event accepted"
    );
    kani::assert(
        (releasing)
            == (State::Releasing {
                settlement: SettlementKind::Refused,
            }),
        "T4 bind refusal settlement",
    );
    kani::assert(
        (refused_bind.as_slice()) == (&[Effect::DropPublisher, Effect::ReleaseDomain]),
        "T4 bind refusal releases without processing",
    );
    let (processing, interrupt_effects) = accepted_step!(
        State::Bound,
        Event::Interrupt(cause),
        "T4 reachable protocol event accepted"
    );
    match cause {
        Interrupt::QueueLoss | Interrupt::DomainLoss | Interrupt::Pressure => kani::assert(
            (interrupt_effects.as_slice())
                == (&[
                    Effect::RevokeDomain,
                    Effect::CancelWork,
                    Effect::AwaitWorkJoin,
                ]),
            "T4 loss interrupts revoke cancel and join in order",
        ),
        Interrupt::Completed => kani::assert(
            (interrupt_effects.as_slice()) == (&[Effect::AwaitWorkJoin]),
            "T4 completed work only joins without revocation or cancellation",
        ),
    }
    let (releasing, _) = accepted_step!(
        processing,
        Event::WorkJoined { ok },
        "T4 reachable protocol event accepted"
    );
    let expected_kind = match cause {
        Interrupt::DomainLoss | Interrupt::Pressure => SettlementKind::Interrupted,
        Interrupt::QueueLoss | Interrupt::Completed if !ok => SettlementKind::Retry,
        _ => SettlementKind::None,
    };
    kani::assert(
        (releasing)
            == (State::Releasing {
                settlement: expected_kind,
            }),
        "T4 interruption selects settlement policy",
    );
    for kind in [SettlementKind::Refused, expected_kind] {
        let effects = release_effects(kind, now, delay);
        match kind {
            SettlementKind::Refused => kani::assert(
                (effects.as_slice())
                    == (&[
                        Effect::Settle(Settlement::Yield {
                            not_before_ms: now.saturating_add(5_000),
                        }),
                        Effect::RetireQueue,
                        Effect::DropAdmission,
                    ]),
                "T4 bind refusal yields five seconds after release",
            ),
            SettlementKind::Interrupted => kani::assert(
                (effects.as_slice())
                    == (&[
                        Effect::Settle(Settlement::Yield {
                            not_before_ms: now.saturating_add(1_000),
                        }),
                        Effect::RetireQueue,
                        Effect::DropAdmission,
                    ]),
                "T4 pressure and domain loss yield one second after release",
            ),
            SettlementKind::Retry => kani::assert(
                (effects.as_slice())
                    == (&[
                        Effect::Settle(Settlement::Retry {
                            not_before_ms: now.saturating_add(delay),
                        }),
                        Effect::RetireQueue,
                        Effect::DropAdmission,
                    ]),
                "T4 failed nonyielded work retries",
            ),
            SettlementKind::None => kani::assert(
                (effects.as_slice()) == (&[Effect::RetireQueue, Effect::DropAdmission]),
                "T4 successful nonyielded work does not settle",
            ),
        }
    }
}

fn check_time(effects: Effects, now: i64, delta: i64) {
    for effect in effects.as_slice() {
        if let Effect::Settle(settlement) = effect {
            let (Settlement::Yield { not_before_ms } | Settlement::Retry { not_before_ms }) =
                settlement;
            if delta >= 0 {
                kani::assert(
                    *not_before_ms >= now,
                    "T5 nonnegative settlement delay never wraps below now",
                );
            }
            // Independent wider arithmetic oracle; no assumption restricts i64 inputs.
            let mathematical_sum = i128::from(now) + i128::from(delta);
            let expected =
                mathematical_sum.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
            kani::assert(
                (*not_before_ms) == (expected),
                "T5 exact saturating addition for all i64 inputs",
            );
        }
    }
}

#[kani::proof]
#[kani::unwind(8)]
fn t5_all_i64_settlement_arithmetic_saturates() {
    let now = kani::any();
    let delay = kani::any();
    let (_, refused) = accepted_step!(
        State::Claimed,
        Event::DomainOutcome {
            acquired: false,
            now_ms: now,
        },
        "T5 reachable protocol event accepted"
    );
    check_time(refused, now, 5_000);
    check_time(
        release_effects(SettlementKind::Refused, now, delay),
        now,
        5_000,
    );
    check_time(
        release_effects(SettlementKind::Interrupted, now, delay),
        now,
        1_000,
    );
    check_time(
        release_effects(SettlementKind::Retry, now, delay),
        now,
        delay,
    );
}

#[kani::proof]
#[kani::unwind(8)]
fn t6_all_typed_stale_pairs_and_terminal_reentry_rejected() {
    let current = state();
    let input = event();
    let result = step(current, input);
    if !accepted_pair(current, input) {
        kani::assert(
            (result) == (Err(Stale)),
            "T6 all stale state event pairs rejected without effects",
        );
    }
    kani::assert(
        (step(State::Terminal, input)) == (Err(Stale)),
        "T6 terminal rejects every typed event",
    );
    if current != State::Claimed {
        kani::assert(
            (step(
                current,
                Event::DomainOutcome {
                    acquired: true,
                    now_ms: kani::any(),
                },
            )) == (Err(Stale)),
            "T6 domain outcome cannot be repeated outside claimed",
        );
    }
    let (after_refusal, _) = accepted_step!(
        State::DomainAcquired,
        Event::BindOutcome { accepted: false },
        "T6 reachable protocol event accepted"
    );
    if let Ok((_, effects)) = step(after_refusal, input) {
        for effect in effects.as_slice() {
            kani::assert(
                !matches!(
                    effect,
                    Effect::Process | Effect::Bind | Effect::AcquireDomain
                ),
                "T6 bind refusal cannot restart processing",
            );
        }
    }
    let (terminal, _) = accepted_step!(
        after_refusal,
        Event::DomainReleased {
            now_ms: kani::any(),
            retry_delay_ms: kani::any(),
        },
        "T6 reachable protocol event accepted"
    );
    kani::assert(
        (terminal) == (State::Terminal),
        "T6 refused bind terminates after release acknowledgment",
    );
    kani::assert(
        (step(terminal, input)) == (Err(Stale)),
        "T6 refused bind remains terminal",
    );
}
