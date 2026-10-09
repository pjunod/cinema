use super::*;
use serde_json::Value;

fn admit(
    guard: &mut ReceiverGuard,
    command: &Command,
    now: u64,
    semantic: Result<(), Outcome>,
) -> Outcome {
    guard.apply(command, now, semantic, || Outcome::Applied)
}
fn fixtures() -> Value {
    serde_json::from_str(include_str!("../../tests/fixtures/remote-control-v1.json"))
        .expect("valid fixture or receiver setup")
}
fn command() -> Command {
    Command::decode(
        &serde_json::to_vec(&fixtures()["valid"][1]["command"])
            .expect("valid fixture or receiver setup"),
    )
    .expect("valid fixture or receiver setup")
}
fn guard(command: &Command) -> ReceiverGuard {
    let mut guard = ReceiverGuard::default();
    guard
        .set_context(ReceiverContext {
            grant_id: command.grant_id,
            target: command.target.clone(),
            control_epoch: command.control_epoch,
            context_revision: command.context_revision,
            focus_revision: command.focus_revision,
            text_nonce: Some(Uuid::from_u128(6)),
        })
        .expect("valid fixture or receiver setup");
    guard
}
fn issue(guard: &mut ReceiverGuard, command: &Command, now: u64) {
    guard
        .insert_credit(
            Credit {
                nonce: command.credit,
                kind: command.action.credit_kind(),
            },
            now,
        )
        .expect("valid fixture or receiver setup");
}

#[test]
fn interoperability_actions_and_invalid_wire() {
    let fixture = fixtures();
    for case in fixture["valid"]
        .as_array()
        .expect("valid fixture or receiver setup")
    {
        let decoded = Command::decode(
            &serde_json::to_vec(&case["command"]).expect("valid fixture or receiver setup"),
        )
        .expect("valid fixture or receiver setup");
        assert_eq!(
            serde_json::to_value(decoded.action.credit_kind())
                .expect("valid fixture or receiver setup"),
            case["credit_kind"],
            "{}",
            case["name"]
        );
        assert_eq!(
            serde_json::from_slice::<Value>(
                &decoded.encode().expect("valid fixture or receiver setup")
            )
            .expect("valid fixture or receiver setup"),
            case["command"]
        );
    }
    for case in fixture["invalid"]
        .as_array()
        .expect("valid fixture or receiver setup")
    {
        assert_eq!(
            Command::decode(
                &serde_json::to_vec(&case["command"]).expect("valid fixture or receiver setup")
            ),
            Err(Outcome::Invalid),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn interoperable_freshness_and_context_scenarios() {
    let original = command();
    for case in fixtures()["scenarios"]
        .as_array()
        .expect("valid fixture or receiver setup")
    {
        let mut guard = guard(&original);
        guard
            .insert_credit(
                Credit {
                    nonce: original.credit,
                    kind: serde_json::from_value(case["credit_kind"].clone())
                        .expect("valid fixture or receiver setup"),
                },
                case["issued_ms"]
                    .as_u64()
                    .expect("valid fixture or receiver setup"),
            )
            .expect("valid fixture or receiver setup");
        if case["preapply"].as_bool() == Some(true) {
            assert_eq!(admit(&mut guard, &original, 100, Ok(())), Outcome::Applied);
        }
        if let Some(epoch) = case["new_control_epoch"].as_str() {
            let mut context = guard.context.clone().expect("active fixture context");
            context.control_epoch = Uuid::parse_str(epoch).expect("fixture epoch");
            guard.set_context(context).expect("takeover context");
        }
        let incoming = Command::decode(
            &serde_json::to_vec(&case["command"]).expect("valid fixture or receiver setup"),
        )
        .expect("valid fixture or receiver setup");
        let result = admit(
            &mut guard,
            &incoming,
            case["now_ms"]
                .as_u64()
                .expect("valid fixture or receiver setup"),
            Ok(()),
        );
        assert_eq!(
            serde_json::to_value(result).expect("valid fixture or receiver setup"),
            case["expected"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn duplicate_select_and_evicted_results_never_reapply() {
    let mut command = command();
    let mut guard = guard(&command);
    issue(&mut guard, &command, 0);
    assert_eq!(admit(&mut guard, &command, 1, Ok(())), Outcome::Applied);
    assert_eq!(
        admit(&mut guard, &command, 2, Ok(())),
        Outcome::DuplicateOrOld
    );
    assert_eq!(
        guard
            .result(command.control_epoch, 1, 2)
            .expect("valid fixture or receiver setup")
            .outcome,
        Outcome::Applied
    );
    for sequence in 2..=65 {
        command.sequence = sequence;
        assert_eq!(
            admit(&mut guard, &command, sequence, Ok(())),
            Outcome::Applied
        );
    }
    assert_eq!(guard.results.len(), MAX_RESULTS);
    assert!(guard.result(command.control_epoch, 1, 66).is_none());
    command.sequence = 1;
    assert_eq!(
        admit(&mut guard, &command, 66, Ok(())),
        Outcome::DuplicateOrOld
    );
    assert!(guard.result(command.control_epoch, 65, 10_065).is_none());
    issue(&mut guard, &command, 10_065);
    assert_eq!(
        admit(&mut guard, &command, 10_066, Ok(())),
        Outcome::DuplicateOrOld
    );
}

#[test]
fn rejected_semantic_actions_do_not_consume_sequence() {
    let command = command();
    let mut guard = guard(&command);
    issue(&mut guard, &command, 0);
    for reason in [
        Outcome::RestrictedSurface,
        Outcome::Unauthorized,
        Outcome::Unsupported,
    ] {
        assert_eq!(admit(&mut guard, &command, 1, Err(reason)), reason);
    }
    assert_eq!(
        admit(&mut guard, &command, 1, Err(Outcome::Applied)),
        Outcome::Invalid
    );
    assert_eq!(admit(&mut guard, &command, 1, Ok(())), Outcome::Applied);
}

#[test]
fn takeover_route_and_physical_input_invalidate_credits() {
    let mut command = command();
    let mut guard = guard(&command);
    issue(&mut guard, &command, 0);
    let mut context = guard
        .context
        .clone()
        .expect("valid fixture or receiver setup");
    context.context_revision += 1;
    guard
        .set_context(context.clone())
        .expect("valid fixture or receiver setup");
    assert_eq!(admit(&mut guard, &command, 1, Ok(())), Outcome::Expired);
    command.context_revision = context.context_revision;
    issue(&mut guard, &command, 1);
    assert_eq!(admit(&mut guard, &command, 2, Ok(())), Outcome::Applied);
    guard.invalidate();
    assert_eq!(
        admit(&mut guard, &command, 3, Ok(())),
        Outcome::DuplicateOrOld
    );
    context.control_epoch = Uuid::from_u128(42);
    guard
        .set_context(context.clone())
        .expect("valid fixture or receiver setup");
    assert_eq!(
        admit(&mut guard, &command, 3, Ok(())),
        Outcome::StaleControl
    );
    command.control_epoch = context.control_epoch;
    issue(&mut guard, &command, 3);
    assert_eq!(admit(&mut guard, &command, 4, Ok(())), Outcome::Applied);
}

#[test]
fn clock_rollback_and_overflow_fail_closed() {
    let command = command();
    let mut guard = guard(&command);
    issue(&mut guard, &command, 100);
    assert_eq!(admit(&mut guard, &command, 99, Ok(())), Outcome::Invalid);
    assert!(guard.credits.is_empty());
    assert_eq!(admit(&mut guard, &command, 101, Ok(())), Outcome::Expired);
    assert_eq!(
        guard.insert_credit(
            Credit {
                nonce: command.credit,
                kind: CreditKind::Interaction
            },
            u64::MAX
        ),
        Err(Outcome::Invalid)
    );
    assert!(guard.credits.is_empty());
}

#[test]
fn bounded_credit_ring_and_safe_integer_encode() {
    let mut command = command();
    let mut guard = guard(&command);
    for n in 1..=17 {
        guard
            .insert_credit(
                Credit {
                    nonce: Uuid::from_u128(n),
                    kind: CreditKind::Interaction,
                },
                0,
            )
            .expect("valid fixture or receiver setup");
    }
    assert_eq!(guard.credits.len(), MAX_CREDITS);
    command.credit = Uuid::from_u128(1);
    assert_eq!(admit(&mut guard, &command, 1, Ok(())), Outcome::Expired);
    command.sequence = MAX_SAFE_INTEGER;
    assert!(command.encode().is_ok());
    command.sequence += 1;
    assert_eq!(command.encode(), Err(Outcome::Invalid));
    assert_eq!(
        Command::decode(&vec![b' '; MAX_BODY_BYTES + 1]),
        Err(Outcome::Invalid)
    );
}

#[test]
fn text_nonce_focus_and_playback_parameters_are_current() {
    let mut command = command();
    let mut guard = guard(&command);
    issue(&mut guard, &command, 0);
    let mut context = guard
        .context
        .clone()
        .expect("valid fixture or receiver setup");
    context.focus_revision += 1;
    guard
        .set_context(context.clone())
        .expect("valid fixture or receiver setup");
    assert_eq!(admit(&mut guard, &command, 1, Ok(())), Outcome::StaleFocus);
    command.action = Action::Navigate {
        direction: Direction::Down,
    };
    assert_eq!(admit(&mut guard, &command, 1, Ok(())), Outcome::Applied);
    command.sequence += 1;
    command.action = Action::TextReplace {
        text_nonce: Uuid::from_u128(99),
        text: String::new(),
    };
    assert_eq!(
        admit(&mut guard, &command, 2, Ok(())),
        Outcome::StaleContext
    );
    command.action = Action::SeekAbsolute {
        position_ms: MAX_SAFE_INTEGER + 1,
    };
    assert_eq!(admit(&mut guard, &command, 3, Ok(())), Outcome::Invalid);
}

#[test]
fn expired_credit_cannot_be_revived_by_production_minting() {
    let mut command = command();
    let mut guard = guard(&command);
    let original = guard
        .mint_credit(CreditKind::Interaction, 0)
        .expect("valid fixture or receiver setup");
    command.credit = original.nonce;
    let replacement = guard
        .mint_credit(CreditKind::Interaction, 1_000)
        .expect("valid fixture or receiver setup");
    assert_ne!(original.nonce, replacement.nonce);
    assert_eq!(admit(&mut guard, &command, 1_001, Ok(())), Outcome::Expired);
}

#[test]
fn invalid_context_and_deactivation_preserve_replay_fence() {
    let command = command();
    let mut guard = guard(&command);
    issue(&mut guard, &command, 0);
    assert_eq!(admit(&mut guard, &command, 1, Ok(())), Outcome::Applied);
    let original = guard
        .context
        .clone()
        .expect("valid fixture or receiver setup");
    let mut invalid = original.clone();
    invalid.context_revision = 0;
    assert_eq!(guard.set_context(invalid), Err(Outcome::Invalid));
    assert_eq!(guard.set_context(original.clone()), Err(Outcome::Invalid));
    assert_eq!(
        guard.mint_credit(CreditKind::Interaction, 2),
        Err(Outcome::Unavailable)
    );
    assert_eq!(admit(&mut guard, &command, 2, Ok(())), Outcome::Unavailable);
    assert_eq!(guard.last_sequence, 1);
    let mut replacement = original;
    replacement.control_epoch = Uuid::new_v4();
    guard
        .set_context(replacement)
        .expect("valid fixture or receiver setup");
    guard.deactivate();
    assert_eq!(
        guard.mint_credit(CreditKind::Interaction, 3),
        Err(Outcome::Unavailable)
    );
}

#[test]
fn effect_outcome_is_recorded_after_consumption_and_never_retried() {
    let command = command();
    let mut guard = guard(&command);
    issue(&mut guard, &command, 0);
    let mut calls = 0;
    assert_eq!(
        guard.apply(&command, 1, Ok(()), || {
            calls += 1;
            Outcome::Unavailable
        }),
        Outcome::Unavailable
    );
    assert_eq!(
        guard
            .result(command.control_epoch, command.sequence, 1)
            .expect("valid fixture or receiver setup")
            .outcome,
        Outcome::Unavailable
    );
    assert_eq!(
        guard.apply(&command, 2, Ok(()), || {
            calls += 1;
            Outcome::Applied
        }),
        Outcome::DuplicateOrOld
    );
    assert_eq!(calls, 1);
}
