#[test]
fn cluster_mode_retains_persisted_members_without_explicit_advertisement() {
    assert!(!super::cluster_enabled_for_peer(false, None));
    assert!(super::cluster_enabled_for_peer(true, None));
    for (raft, api, network) in [
        ("127.0.0.1:32401", "[::1]:32402", false),
        ("plurx-a.lan:32401", "plurx-a.lan:32402", true),
        ("192.0.2.4:32401", "192.0.2.4:32402", true),
        ("127.0.0.1:32401", "192.0.2.4:32402", true),
    ] {
        let peer = super::ClusterPeer {
            raft_id: 1,
            raft_address: raft.into(),
            api_address: api.into(),
        };
        assert_eq!(super::cluster_enabled_for_peer(false, Some(&peer)), network);
        assert!(super::cluster_enabled_for_peer(true, Some(&peer)));
    }
}

#[test]
fn sharing_purpose_master_proof_requires_canonical_256_bit_fingerprint() {
    for rejected in [
        "".to_owned(),
        "a".repeat(8),
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
        format!("{}'", "a".repeat(63)),
        "é".repeat(32),
    ] {
        assert!(super::purpose_master_proof_predicate(&rejected).is_err());
    }
    assert!(super::purpose_master_proof_predicate(&"a".repeat(64))
        .expect("canonical")
        .contains("sharing_purpose_master_v1:"));
}

use super::{
    reduction_fence_evidence, ActivityPeer, AppliedRemovalFence, ClockMembershipIdentity,
    ClockMembershipSource, ClockPeerRoster, ClockRefusal, ClusterClockGuard, ReductionEvidence,
    RemovalFenceEvidence,
};
use crate::cluster::clock::PeerClockOffset;

struct FixedClockMembership(ClockMembershipIdentity);

impl ClockMembershipSource for FixedClockMembership {
    fn current(&self) -> Option<ClockMembershipIdentity> {
        Some(self.0.clone())
    }
}

fn clock_node(raft_id: u64) -> String {
    format!("00000000-0000-0000-0000-{raft_id:012}")
}

/// Local node 1; `voters` vote, every other member is a learner. The
/// returned roster is the exact directory of every remote member.
fn clock_fixture(
    members: &[u64],
    voters: &[u64],
    enforced: bool,
) -> (ClusterClockGuard, ClockPeerRoster) {
    let identity = ClockMembershipIdentity {
        local_node: 1,
        log: (2, 1, 7),
        members: members.iter().copied().collect(),
        voters: voters.iter().copied().collect(),
    };
    let guard = ClusterClockGuard::with_membership_source(std::sync::Arc::new(
        FixedClockMembership(identity.clone()),
    ));
    guard.set_enforced(enforced);
    let roster = ClockPeerRoster {
        membership: Some(identity),
        peers: members
            .iter()
            .filter(|raft_id| **raft_id != 1)
            .map(|raft_id| ActivityPeer {
                node_id: clock_node(*raft_id),
                raft_id: *raft_id,
                http_base: Some(format!("https://node{raft_id}:443")),
                reachable: true,
            })
            .collect(),
    };
    (guard, roster)
}

fn publish_clock_round(guard: &ClusterClockGuard, roster: &ClockPeerRoster, bounded: &[u64]) {
    let at_zero: Vec<_> = bounded.iter().map(|raft_id| (*raft_id, 0)).collect();
    publish_clock_offsets(guard, roster, &at_zero);
}

/// One complete round: each listed peer is measured at its offset (µs)
/// with 1 ms uncertainty; every other peer is `Unknown`.
fn publish_clock_offsets(
    guard: &ClusterClockGuard,
    roster: &ClockPeerRoster,
    bounded: &[(u64, i64)],
) {
    let round = guard
        .roster_for_peer_directory(roster)
        .expect("exact directory");
    let observations = roster
        .peers
        .iter()
        .map(|peer| {
            let observation = match bounded.iter().find(|(id, _)| *id == peer.raft_id) {
                Some((_, offset_us)) => PeerClockOffset::Bounded {
                    offset_us: *offset_us,
                    uncertainty_us: 1_000,
                    observed_at: std::time::Instant::now(),
                },
                None => PeerClockOffset::Unknown,
            };
            (peer.node_id.clone(), observation)
        })
        .collect();
    assert!(guard.publish(round, observations));
}

fn removal_fence(target: u64, evidence: RemovalFenceEvidence) -> AppliedRemovalFence {
    AppliedRemovalFence {
        reference: hiqlite::ReductionFenceReference {
            version: 1,
            target_node_id: clock_node(target),
            target_raft_id: target,
            attempt_id: "00000000-0000-0000-0000-000000000099".into(),
            barrier_index: 7,
        },
        evidence,
    }
}

fn counter(guard: &ClusterClockGuard, family: &str, cause: &str) -> String {
    let prefix = format!("{family}{{decision=\"membership_change\",cause=\"{cause}\"}} ");
    guard
        .prometheus()
        .lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()).map(str::to_owned))
        .expect("closed metric vocabulary")
}

/// Fenced removal of a voter with a stopped learner elsewhere in the
/// cluster: the unobserved surviving learner is excused (it holds no
/// vote), an unobserved surviving VOTER still refuses.
#[test]
fn k06_fenced_removal_excuses_an_unobserved_surviving_learner_only() {
    let (guard, roster) = clock_fixture(&[1, 2, 3, 4], &[1, 2, 3], true);
    publish_clock_round(&guard, &roster, &[3]);
    let captured = guard.capture_removal_raft(2).expect("exact capture");
    assert_eq!(
        guard.admit_fenced_removal(
            &captured,
            &removal_fence(2, RemovalFenceEvidence::TargetApplied)
        ),
        Ok(()),
        "target 2 excluded, voter 3 bounded, learner 4 unobserved"
    );
    drop(captured);
    publish_clock_round(&guard, &roster, &[4]);
    let captured = guard.capture_removal_raft(2).expect("exact capture");
    assert_eq!(
        guard.admit_fenced_removal(
            &captured,
            &removal_fence(2, RemovalFenceEvidence::TargetApplied)
        ),
        Err(ClockRefusal::Unknown),
        "an unobserved surviving voter is never excused"
    );
}

/// The voters-only rule applies to the surviving set of a fenced
/// removal: a learner measured at +10 s does not block removing a dead
/// voter, and the skewed learner itself stays removable. A surviving
/// VOTER over the bound still refuses.
#[test]
fn k06_fenced_removal_ignores_a_skewed_surviving_learner_not_a_voter() {
    const STEPPED_US: i64 = 10_000_000;
    let (guard, roster) = clock_fixture(&[1, 2, 3, 4], &[1, 2, 3], true);
    // Voter 2 is dead (unobserved), voter 3 bounded, learner 4 at +10 s.
    publish_clock_offsets(&guard, &roster, &[(3, 0), (4, STEPPED_US)]);
    let captured = guard.capture_removal_raft(2).expect("exact capture");
    assert_eq!(
        guard.admit_fenced_removal(
            &captured,
            &removal_fence(2, RemovalFenceEvidence::AuthoritativeUnreachable)
        ),
        Ok(()),
        "a skewed surviving learner must not block removing a dead voter"
    );
    drop(captured);
    // Every voter bounded: the stepped learner is itself removable.
    publish_clock_offsets(&guard, &roster, &[(2, 0), (3, 0), (4, STEPPED_US)]);
    let captured = guard.capture_removal_raft(4).expect("exact capture");
    assert_eq!(
        guard.admit_fenced_removal(
            &captured,
            &removal_fence(4, RemovalFenceEvidence::TargetApplied)
        ),
        Ok(()),
        "the skewed learner itself stays removable"
    );
    drop(captured);
    // Voter 3 stepped instead; the learner is within the bound.
    publish_clock_offsets(&guard, &roster, &[(3, STEPPED_US), (4, 0)]);
    let captured = guard.capture_removal_raft(2).expect("exact capture");
    assert_eq!(
        guard.admit_fenced_removal(
            &captured,
            &removal_fence(2, RemovalFenceEvidence::AuthoritativeUnreachable)
        ),
        Err(ClockRefusal::Offset),
        "a surviving voter over the bound still refuses the removal"
    );
    assert_eq!(
        counter(&guard, "plurx_cluster_clock_refusals_total", "offset"),
        "1"
    );
}

/// K-06 finding 2. After a local wall step the original capture may not
/// use a wall-age `last_seen_at` comparison. That decision goes through
/// the guard: advisory mode admits the authoritative-unreachable proof
/// (counting ONE advisory refusal at the counted boundary), enforced mode
/// keeps waiting for TargetApplied and, if the budget ends, reports the
/// typed counted refusal instead of a bare deadline.
#[test]
fn k06_post_step_wall_reachability_is_decided_by_the_guard() {
    for enforced in [false, true] {
        let (guard, roster) = clock_fixture(&[1, 2, 3], &[1, 2, 3], enforced);
        publish_clock_round(&guard, &roster, &[2, 3]);
        guard.simulate_wall_step_for_test(15_000);
        // The step invalidates evidence; a later complete round restores
        // coverage, but the 30 s stabilization window still holds.
        publish_clock_round(&guard, &roster, &[2, 3]);
        let captured = guard.capture_removal_raft(2).expect("bounded capture");
        assert!(!captured.permits_wall_reachability());
        let advisory = "plurx_cluster_clock_advisory_refusals_total";
        let refusals = "plurx_cluster_clock_refusals_total";

        // A live target is still provable either way; a wall-reachable
        // target is simply not yet proved, never clock-blocked.
        assert!(matches!(
            reduction_fence_evidence(&guard, &captured, true, false),
            ReductionEvidence::Proved(RemovalFenceEvidence::TargetApplied)
        ));
        assert!(matches!(
            reduction_fence_evidence(&guard, &captured, false, true),
            ReductionEvidence::Pending {
                clock_blocked: false
            }
        ));

        let stale = reduction_fence_evidence(&guard, &captured, false, false);
        assert_eq!(counter(&guard, advisory, "local_discontinuity"), "0");
        assert_eq!(counter(&guard, refusals, "local_discontinuity"), "0");
        if enforced {
            assert!(matches!(
                stale,
                ReductionEvidence::Pending {
                    clock_blocked: true
                }
            ));
            // The budget ends while the poll was clock-blocked: the
            // caller sees the counted typed refusal, not a bare deadline.
            assert!(matches!(
                removal_budget_expired_error(&guard, &captured, true),
                MembershipError::ClockUnbounded(ClockRefusal::LocalDiscontinuity)
            ));
            assert_eq!(counter(&guard, refusals, "local_discontinuity"), "1");
            // An unblocked budget end is the plain deadline, uncounted.
            assert!(matches!(
                removal_budget_expired_error(&guard, &captured, false),
                MembershipError::RemovalPending(_)
            ));
            assert_eq!(counter(&guard, refusals, "local_discontinuity"), "1");
            // Enforcement turned off before the budget ended: the plain
            // deadline, and no advisory refusal for a decision that was
            // never admitted (review P3a).
            guard.set_enforced(false);
            assert!(matches!(
                removal_budget_expired_error(&guard, &captured, true),
                MembershipError::RemovalPending(_)
            ));
            assert_eq!(counter(&guard, advisory, "local_discontinuity"), "0");
            assert_eq!(counter(&guard, refusals, "local_discontinuity"), "1");
        } else {
            assert!(
                matches!(
                    stale,
                    ReductionEvidence::Proved(RemovalFenceEvidence::AuthoritativeUnreachable)
                ),
                "advisory mode must not strand the removal until its budget expires"
            );
            assert_eq!(
                guard.admit_fenced_removal(
                    &captured,
                    &removal_fence(2, RemovalFenceEvidence::AuthoritativeUnreachable)
                ),
                Ok(())
            );
            assert_eq!(counter(&guard, advisory, "local_discontinuity"), "1");
            assert_eq!(counter(&guard, refusals, "local_discontinuity"), "0");
            assert!(matches!(
                removal_budget_expired_error(&guard, &captured, true),
                MembershipError::RemovalPending(_)
            ));
            assert_eq!(counter(&guard, advisory, "local_discontinuity"), "1");
        }
    }

    // The wait carries the poll's verdict to EVERY budget-expiry exit;
    // dropping it would turn the typed refusal back into a bare deadline.
    // The waiting loop needs a live Raft client, so its wiring is pinned
    // from source while the decision itself is exercised above.
    let source = include_str!("../membership.rs");
    let wait = method_body(source, "async fn wait_for_reduction_reference(");
    assert!(wait.contains("let mut clock_blocked = false;"));
    assert!(wait.contains("clock_blocked: blocked,") && wait.contains("clock_blocked = blocked;"));
    assert_eq!(
        wait.matches("self.removal_budget_expired(").count(),
        wait.matches("self.removal_budget_expired(captured, clock_blocked)")
            .count()
    );
    assert_eq!(
        wait.matches("self.removal_budget_expired(captured, clock_blocked)")
            .count(),
        3,
        "both budget checks and the poll timeout report the verdict"
    );
    assert!(!wait.contains("RemovalPending("), "no exit bypasses it");
    let expired = method_body(source, "fn removal_budget_expired(");
    assert!(expired.contains("removal_budget_expired_error(&self.clock, captured, clock_blocked)"));
    let poll = method_body(source, "async fn prove_reduction_reference(");
    assert!(poll.contains("ReductionEvidence::Pending { clock_blocked }"));
    assert!(poll.contains("ReductionProofPoll::Pending { clock_blocked }"));
}

#[test]
fn k06_startup_deadline_is_installed_once_and_never_replenished() {
    let admission = super::StartupMembershipAdmission::default();
    assert_eq!(admission.startup_deadline(), None);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
    admission
        .install_startup_deadline(deadline)
        .expect("first install");
    assert!(admission
        .install_startup_deadline(deadline + std::time::Duration::from_secs(45))
        .is_err());
    assert_eq!(admission.startup_deadline(), Some(deadline));
}

#[test]
fn k06_reduction_binding_refuses_zero_stale_and_foreign_identity_without_borrowing() {
    use super::{
        exact_reduction_binding_matches, reduction_reference_key, removed_job_owner_key,
        ReductionReferenceRow, FREEZE_REDUCTION_REFERENCE_SQL, MAX_FROZEN_REDUCTION_REFERENCES,
        READ_REDUCTION_REFERENCE_SQL, RETIRE_OWNED_REDUCTION_REFERENCE_SQL,
    };
    let node = "00000000-0000-0000-0000-000000000002";
    let attempt = "00000000-0000-0000-0000-000000000010";
    let other = "00000000-0000-0000-0000-000000000011";
    let db = production_membership_tables();
    db.execute(
        "INSERT INTO cluster_nodes (node_id, raft_id, raft_address, api_address, last_seen_at) \
             VALUES (?1, 2, 'node-2:32401', 'node-2:32402', 100)",
        [node],
    )
    .expect("target");
    // The applied index is the target's own heartbeat progress, which
    // lives beside its roster row, not in it.
    db.execute(
        "INSERT INTO cluster_node_progress (node_id, current_term, last_applied_index, \
               bounded_read_ready, voter_storage_ready, voter_role_persisted, observed_at) \
             VALUES (?1, 1, 42, 1, 1, 1, 100)",
        [node],
    )
    .expect("target progress");
    db.execute(
        "INSERT INTO cluster_node_removals (node_id, started_at) VALUES (?1, 100)",
        [node],
    )
    .expect("route fence");
    db.execute(
        "INSERT INTO settings VALUES (?1, '1', 100)",
        [removed_job_owner_key(node)],
    )
    .expect("owner fence");
    for id in [attempt, other] {
        db.execute(
            "INSERT INTO cluster_node_removal_attempts VALUES (?1, ?2)",
            rusqlite::params![node, id],
        )
        .expect("owned refs");
    }
    let reference = |id: &str, barrier| hiqlite::ReductionFenceReference {
        version: 1,
        target_node_id: node.into(),
        target_raft_id: 2,
        attempt_id: id.into(),
        barrier_index: barrier,
    };
    let freeze = |value: &hiqlite::ReductionFenceReference| {
        db.execute(
            FREEZE_REDUCTION_REFERENCE_SQL,
            rusqlite::params![
                reduction_reference_key(value),
                serde_json::to_string(value).expect("json"),
                100,
                node,
                value.attempt_id,
                removed_job_owner_key(node),
                i64::try_from(value.target_raft_id).expect("fixture Raft identity"),
                i64::try_from(value.barrier_index).expect("fixture barrier"),
                MAX_FROZEN_REDUCTION_REFERENCES
            ],
        )
        .expect("actual guarded insert")
    };
    let original = reference(attempt, 42);
    assert_eq!(
        freeze(&reference(attempt, 0)),
        0,
        "zero cannot create a binding"
    );
    assert_eq!(freeze(&original), 1);
    assert_eq!(
        freeze(&reference(attempt, 99)),
        0,
        "replay cannot refresh the original barrier"
    );
    let read = |value: &hiqlite::ReductionFenceReference| {
        use rusqlite::OptionalExtension;
        db.query_row(
            READ_REDUCTION_REFERENCE_SQL,
            rusqlite::params![
                reduction_reference_key(value),
                node,
                value.attempt_id,
                removed_job_owner_key(node)
            ],
            |row| {
                Ok(ReductionReferenceRow {
                    reference_json: row.get("reference_json")?,
                    raft_id: row.get("raft_id")?,
                    last_seen_at: row.get("last_seen_at")?,
                    last_applied_index: row.get("last_applied_index")?,
                })
            },
        )
        .optional()
        .expect("actual bound read")
    };
    assert!(exact_reduction_binding_matches(
        &read(&original).expect("bound original"),
        &original
    ));
    assert!(
        !exact_reduction_binding_matches(
            &read(&original).expect("unchanged original"),
            &reference(attempt, 99)
        ),
        "a different barrier cannot borrow the frozen original"
    );
    let concurrent = reference(other, 43);
    assert_eq!(freeze(&concurrent), 1);
    db.execute(
        "UPDATE cluster_nodes SET raft_id = 3 WHERE node_id = ?1",
        [node],
    )
    .expect("replace Raft identity");
    assert_eq!(
        freeze(&reference(attempt, 44)),
        0,
        "changed identity cannot mint"
    );
    assert!(
        !exact_reduction_binding_matches(
            &read(&original).expect("stored foreign identity"),
            &original
        ),
        "actual receiver binding check rejects changed Raft identity"
    );
    db.execute(
        "UPDATE cluster_nodes SET raft_id = 2 WHERE node_id = ?1",
        [node],
    )
    .expect("restore fixture identity");
    assert_eq!(
        db.execute(
            RETIRE_OWNED_REDUCTION_REFERENCE_SQL,
            rusqlite::params![reduction_reference_key(&original), node, attempt]
        )
        .expect("live retirement refusal"),
        0
    );
    db.execute(
        "DELETE FROM cluster_node_removal_attempts WHERE node_id = ?1 AND attempt_id = ?2",
        rusqlite::params![node, attempt],
    )
    .expect("release only owned ref");
    assert!(
        read(&original).is_none(),
        "rolled-back ref cannot borrow the surviving attempt"
    );
    assert_eq!(
        db.execute(
            RETIRE_OWNED_REDUCTION_REFERENCE_SQL,
            rusqlite::params![reduction_reference_key(&original), node, attempt]
        )
        .expect("owned cleanup"),
        1
    );
    assert!(
        read(&concurrent).is_some(),
        "another invocation stays fenced and bound"
    );
}

#[tokio::test]
async fn finalization_preserves_clock_ticket_and_completed_retry() {
    use std::cell::Cell;
    use std::time::Instant;

    use crate::cluster::clock::{ClockRefusal, ClusterClockGuard, PeerClockOffset};

    use super::lifecycle::JoinTransition;
    use super::{ClusterRole, MembershipError, FINALIZE_JOIN_TOKEN_SQL};

    let publish = |guard: &ClusterClockGuard| {
        let round = guard.roster(&["peer".into()]);
        assert!(guard.publish(
            round,
            std::collections::BTreeMap::from([(
                "peer".into(),
                PeerClockOffset::Bounded {
                    offset_us: 0,
                    uncertainty_us: 1_000,
                    observed_at: Instant::now(),
                },
            )])
        ));
    };
    for role in [ClusterRole::Voter, ClusterRole::Learner] {
        for schedule in [
            "completed retry",
            "unknown entry recovers",
            "evidence changes during role read",
            "role not committed",
            "same original proof",
            "token owner changed",
        ] {
            let database = rusqlite::Connection::open_in_memory().expect("token CAS fixture");
            database
                .execute_batch(
                    "CREATE TABLE cluster_join_tokens (token_hash TEXT PRIMARY KEY, \
                         state TEXT, node_id TEXT, redeemed_at INTEGER);",
                )
                .expect("create token table");
            let completed = schedule == "completed retry";
            database
                .execute(
                    "INSERT INTO cluster_join_tokens VALUES ('digest', ?1, ?2, 77)",
                    rusqlite::params![
                        if completed { "redeemed" } else { "redeeming" },
                        if schedule == "token owner changed" {
                            "other"
                        } else {
                            "node"
                        }
                    ],
                )
                .expect("seed token lifecycle");
            let guard = ClusterClockGuard::new(true);
            guard.set_enforced(true);
            if !matches!(schedule, "completed retry" | "unknown entry recovers") {
                publish(&guard);
            }
            let prepared = guard.acquire();
            let original_now = prepared.as_ref().ok().map(|ticket| ticket.now_ms());
            let observations = Cell::new(0);
            let writes = Cell::new(0);
            let database = &database;
            let writes = &writes;
            let transition = JoinTransition::finalization("digest", "node", 41, role, completed);
            let outcome = super::dispatch_clocked_join_finalization(
                &guard,
                prepared,
                transition,
                || async {
                    observations.set(observations.get() + 1);
                    tokio::task::yield_now().await;
                    if schedule == "unknown entry recovers" {
                        publish(&guard);
                    } else if schedule == "evidence changes during role read" {
                        guard.roster_failed();
                        publish(&guard);
                    }
                    Ok((
                        schedule != "role not committed",
                        schedule != "role not committed" && role == ClusterRole::Voter,
                    ))
                },
                |step, now| async move {
                    writes.set(writes.get() + 1);
                    assert_eq!(Some(now), original_now, "{role:?}: {schedule}");
                    Ok(database
                        .execute(
                            FINALIZE_JOIN_TOKEN_SQL,
                            rusqlite::params![now, step.token_digest, step.node_id],
                        )
                        .expect("production token CAS SQL"))
                },
            )
            .await;
            let (state, redeemed_at): (String, i64) = database
                .query_row(
                    "SELECT state, redeemed_at FROM cluster_join_tokens WHERE token_hash='digest'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .expect("inspect actual CAS outcome");
            match schedule {
                "completed retry" => {
                    assert!(outcome.is_ok());
                    assert_eq!((observations.get(), writes.get()), (0, 0));
                    assert_eq!((state.as_str(), redeemed_at), ("redeemed", 77));
                    assert!(guard
                        .prometheus()
                        .contains("decision=\"membership_change\",cause=\"unknown\"} 0\n"));
                }
                "same original proof" => {
                    assert!(outcome.is_ok());
                    assert_eq!((observations.get(), writes.get()), (1, 1));
                    assert_eq!(state, "redeemed");
                    assert_eq!(Some(redeemed_at), original_now);
                }
                "token owner changed" => {
                    assert!(matches!(outcome, Err(MembershipError::ReusedToken)));
                    assert_eq!(writes.get(), 1);
                    assert_eq!((state.as_str(), redeemed_at), ("redeeming", 77));
                }
                "unknown entry recovers" | "evidence changes during role read" => {
                    let cause = if schedule == "unknown entry recovers" {
                        ClockRefusal::Unknown
                    } else {
                        ClockRefusal::GenerationChanged
                    };
                    assert!(
                        matches!(outcome, Err(MembershipError::ClockUnbounded(actual)) if actual == cause)
                    );
                    assert_eq!(writes.get(), 0);
                    assert_eq!((state.as_str(), redeemed_at), ("redeeming", 77));
                }
                "role not committed" => {
                    assert!(matches!(outcome, Err(MembershipError::Internal(_))));
                    assert_eq!(writes.get(), 0);
                    assert_eq!((state.as_str(), redeemed_at), ("redeeming", 77));
                }
                _ => unreachable!("closed schedules"),
            }
        }
    }
    let source = include_str!("../membership.rs");
    let finalization = super::tests::method_body(source, "async fn finalize_for_role(");
    assert!(
        finalization
            .find("let prepared_admission = clock.acquire();")
            .expect("entry")
            < finalization.find(".await").expect("token read")
    );
    assert!(finalization.contains("dispatch_clocked_join_finalization("));
    assert!(!finalization.contains("let now = unix_ms()?"));
}

#[tokio::test]
async fn join_finalization_consumer_preserves_failure_and_effect_order() {
    use super::lifecycle::{JoinEffect, JoinTransition};
    for (redeemed, observation, changed, expected_events, succeeds) in [
        (true, Some((false, false)), Some(0), vec![], true),
        (false, Some((true, false)), Some(1), vec!["observe"], false),
        (false, None, Some(1), vec!["observe"], false),
        (
            false,
            Some((true, true)),
            None,
            vec!["observe", "redeem"],
            false,
        ),
        (
            false,
            Some((true, true)),
            Some(0),
            vec!["observe", "redeem"],
            false,
        ),
        (
            false,
            Some((true, true)),
            Some(1),
            vec!["observe", "redeem"],
            true,
        ),
    ] {
        let events = std::sync::Mutex::new(Vec::new());
        let events = &events;
        let transition =
            JoinTransition::finalization("digest", "node", 41, super::ClusterRole::Voter, redeemed);
        let outcome = super::dispatch_join_finalization(
            transition,
            || async {
                events.lock().expect("record observation").push("observe");
                observation
                    .ok_or_else(|| super::MembershipError::Internal("metrics failed".to_owned()))
            },
            |step| async move {
                assert_eq!(step.effect, JoinEffect::RedeemToken);
                assert_eq!(
                    (step.token_digest, step.node_id, step.raft_id, step.role),
                    ("digest", "node", 41, super::ClusterRole::Voter)
                );
                events.lock().expect("record redemption").push("redeem");
                changed.ok_or_else(|| super::MembershipError::Internal("CAS failed".to_owned()))
            },
        )
        .await;
        assert_eq!(outcome.is_ok(), succeeds);
        assert_eq!(
            *events.lock().expect("read ordered effects"),
            expected_events
        );
        if !redeemed && observation.is_none() {
            assert!(
                matches!(outcome, Err(super::MembershipError::Internal(message)) if message == "metrics failed")
            );
        } else if !redeemed && observation == Some((true, true)) && changed.is_none() {
            assert!(
                matches!(outcome, Err(super::MembershipError::Internal(message)) if message == "CAS failed")
            );
        } else if !redeemed && observation == Some((true, true)) && changed == Some(0) {
            assert!(matches!(outcome, Err(super::MembershipError::ReusedToken)));
        }
    }
}

#[test]
fn snapshot_floor_requires_current_process_heartbeat_proof() {
    let database = production_membership_tables();
    database.execute_batch(
            "INSERT INTO cluster_nodes (node_id, raft_id, raft_address, api_address, last_seen_at, role) \
               VALUES ('target', 1, 'target:32401', 'target:32402', 50, 'learner'); \
             INSERT INTO cluster_node_progress (node_id, current_term, last_applied_index, \
               apply_lag_entries, bounded_read_ready, voter_storage_ready, storage_headroom_bytes, \
               storage_probe_observed_at, voter_role_persisted, observed_at) \
               VALUES ('target', 1, 10, 0, 1, 1, 536870912, 50, 0, 50);"
        ).expect("seed legacy readiness rows");
    let proven = || {
        database
            .query_row(
                super::PROMOTION_TARGET_SQL,
                rusqlite::params!["target", false],
                |row| {
                    Ok(super::snapshot_floor_heartbeat_matches(
                        row.get("snapshot_floor_observed_at")?,
                        row.get("observed_at")?,
                        row.get("last_seen_at")?,
                    ))
                },
            )
            .expect("production promotion target projection")
    };
    assert!(
        !proven(),
        "legacy 512MiB readiness is not the new floor proof"
    );
    database
        .execute(
            "INSERT INTO cluster_node_capabilities VALUES ('target',?1,49)",
            [super::SNAPSHOT_STORAGE_FLOOR_CAPABILITY],
        )
        .expect("insert stale capability");
    assert!(!proven(), "stale capability must fail closed");
    database
        .execute("UPDATE cluster_node_capabilities SET last_seen_at=50", [])
        .expect("publish current capability");
    assert!(
        proven(),
        "same heartbeat must prove the target's new semantics"
    );
    database
        .execute("UPDATE cluster_node_progress SET observed_at=51", [])
        .expect("advance legacy progress");
    assert!(
        !proven(),
        "a later legacy progress row invalidates the retained marker"
    );
    database
        .execute("UPDATE cluster_nodes SET last_seen_at=51", [])
        .expect("advance node heartbeat");
    assert!(!proven());
    database
        .execute("UPDATE cluster_node_capabilities SET last_seen_at=51", [])
        .expect("publish new same-heartbeat capability");
    assert!(proven());
    database
        .execute("UPDATE cluster_nodes SET removed_at=52", [])
        .expect("mark node removed");
    assert!(database
        .query_row(
            super::PROMOTION_TARGET_SQL,
            rusqlite::params!["target", false],
            |_| Ok(())
        )
        .is_err());
    assert!(database
        .query_row(
            super::PROMOTION_TARGET_SQL,
            rusqlite::params!["target", true],
            |_| Ok(())
        )
        .is_ok());
}

#[test]
fn voter_snapshot_storage_floor_is_target_local_known_fresh_and_at_least_512_mib() {
    let minimum = super::MIN_VOTER_STORAGE_HEADROOM_BYTES;
    assert!(!super::voter_snapshot_storage_ready(
        true,
        true,
        Some(1),
        Some(minimum - 1)
    ));
    assert!(super::voter_snapshot_storage_ready(
        true,
        true,
        Some(1),
        Some(minimum)
    ));
    assert!(!super::voter_snapshot_storage_ready(
        true,
        true,
        Some(2 * minimum),
        Some(minimum)
    ));
    assert!(super::voter_snapshot_storage_ready(
        true,
        true,
        Some(2 * minimum),
        Some(2 * minimum)
    ));
    assert!(!super::voter_snapshot_storage_ready(
        true,
        true,
        None,
        Some(u64::MAX)
    ));
    assert!(!super::voter_snapshot_storage_ready(
        true,
        true,
        Some(1),
        None
    ));
    assert!(!super::voter_snapshot_storage_ready(
        false,
        true,
        Some(1),
        Some(u64::MAX)
    ));
    assert!(!super::voter_snapshot_storage_ready(
        true,
        false,
        Some(1),
        Some(u64::MAX)
    ));
}
use super::*;

fn sharing_principal_floor_fixture() -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().expect("floor fixture");
    connection
        .execute_batch(
            "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, raft_id INTEGER NOT NULL, \
               last_seen_at INTEGER NOT NULL, removed_at INTEGER, role TEXT); \
             CREATE TABLE cluster_node_capabilities (node_id TEXT NOT NULL, \
               capability TEXT NOT NULL, last_seen_at INTEGER NOT NULL, \
               PRIMARY KEY(node_id, capability)); \
             CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
             CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY); \
             CREATE TABLE cluster_node_removal_attempts (node_id TEXT NOT NULL, attempt_id TEXT); \
             INSERT INTO cluster_nodes VALUES \
               ('voter', 1, 1000000, NULL, NULL), \
               ('learner', 2, 1000000, NULL, 'learner');",
        )
        .expect("floor schema and members");
    connection.execute(
            "INSERT INTO cluster_node_capabilities SELECT node_id, ?1, last_seen_at FROM cluster_nodes",
            [SHARING_SESSION_PRINCIPAL_CAPABILITY],
        ).expect("current binary proofs");
    connection
}

fn sharing_principal_floor_allows(connection: &rusqlite::Connection, members: &str) -> bool {
    connection
        .query_row(
            &format!(
                "SELECT {}",
                sharing_session_principal_guard_predicate(1, 2, 3)
            ),
            rusqlite::params![members, 880000_i64, 1000000_i64],
            |row| row.get(0),
        )
        .expect("execute production floor predicate")
}

#[test]
fn sharing_principal_floor_requires_every_current_voter_and_learner() {
    let connection = sharing_principal_floor_fixture();
    assert!(sharing_principal_floor_allows(&connection, "[1,2]"));
    assert!(!sharing_principal_floor_allows(&connection, "[]"));
    assert!(!sharing_principal_floor_allows(&connection, "[1,2,3]"));
    connection
        .execute(
            "DELETE FROM cluster_node_capabilities WHERE node_id = 'learner'",
            [],
        )
        .expect("floor fixture mutation");
    assert!(!sharing_principal_floor_allows(&connection, "[1,2]"));
    connection
        .execute(
            "INSERT INTO cluster_node_capabilities VALUES ('learner', ?1, 999999)",
            [SHARING_SESSION_PRINCIPAL_CAPABILITY],
        )
        .expect("floor fixture mutation");
    assert!(!sharing_principal_floor_allows(&connection, "[1,2]"));
    connection
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at = 1000000",
            [],
        )
        .expect("floor fixture mutation");
    assert!(sharing_principal_floor_allows(&connection, "[1,2]"));
    // The next heartbeat from an older rejoined binary must invalidate its
    // former capability, even while all timestamps remain fresh.
    connection
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = 999999 WHERE node_id = 'learner'",
            [],
        )
        .expect("floor fixture mutation");
    assert!(!sharing_principal_floor_allows(&connection, "[1,2]"));
}

#[test]
fn sharing_principal_floor_refuses_stale_future_join_and_orphan_removal() {
    for heartbeat in [879999_i64, 1000001] {
        let connection = sharing_principal_floor_fixture();
        connection
            .execute(
                "UPDATE cluster_nodes SET last_seen_at = ?1 WHERE node_id = 'learner'",
                [heartbeat],
            )
            .expect("floor fixture mutation");
        connection
            .execute(
                "UPDATE cluster_node_capabilities SET last_seen_at = ?1 WHERE node_id = 'learner'",
                [heartbeat],
            )
            .expect("floor fixture mutation");
        assert!(!sharing_principal_floor_allows(&connection, "[1,2]"));
    }
    for mutation in [
        "INSERT INTO cluster_node_join_staging VALUES ('learner')",
        "INSERT INTO cluster_node_join_staging VALUES ('unknown-rejoin')",
        "INSERT INTO cluster_node_removals VALUES ('learner')",
        "INSERT INTO cluster_node_removals VALUES ('unknown-removed')",
        "INSERT INTO cluster_node_removal_attempts VALUES ('unknown-removed', 'attempt')",
    ] {
        let connection = sharing_principal_floor_fixture();
        connection
            .execute(mutation, [])
            .expect("floor fixture mutation");
        assert!(
            !sharing_principal_floor_allows(&connection, "[1,2]"),
            "{mutation}"
        );
    }
    let connection = sharing_principal_floor_fixture();
    connection.execute_batch("UPDATE cluster_nodes SET removed_at = 1000000 WHERE node_id = 'learner'; INSERT INTO cluster_node_removals VALUES ('learner'); INSERT INTO cluster_node_removal_attempts VALUES ('learner', 'done'); INSERT INTO cluster_node_join_staging VALUES ('learner');").expect("floor fixture mutation");
    assert!(
        !sharing_principal_floor_allows(&connection, "[1,2]"),
        "still a committed member"
    );
    assert!(
        sharing_principal_floor_allows(&connection, "[1]"),
        "completed removal is not permanent debt"
    );
}

#[test]
fn sharing_principal_floor_guard_rechecks_the_write_after_a_legacy_heartbeat() {
    let connection = sharing_principal_floor_fixture();
    connection
        .execute(
            "CREATE TABLE sharing_admission_receipts (id INTEGER PRIMARY KEY)",
            [],
        )
        .expect("floor fixture mutation");
    assert!(sharing_principal_floor_allows(&connection, "[1,2]"));
    connection
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = 999999 WHERE node_id = 'learner'",
            [],
        )
        .expect("floor fixture mutation");
    let changed = connection
        .execute(
            &format!(
                "INSERT INTO sharing_admission_receipts SELECT 1 WHERE {}",
                sharing_session_principal_guard_predicate(1, 2, 3)
            ),
            rusqlite::params!["[1,2]", 880000_i64, 1000000_i64],
        )
        .expect("conditional admission");
    assert_eq!(
        changed, 0,
        "a successful earlier preflight is not admission authority"
    );
    connection
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at = 999999 WHERE node_id = 'learner'",
            [],
        )
        .expect("floor fixture mutation");
    assert_eq!(
        connection
            .execute(
                &format!(
                    "INSERT INTO sharing_admission_receipts SELECT 1 WHERE {}",
                    sharing_session_principal_guard_predicate(1, 2, 3)
                ),
                rusqlite::params!["[1,2]", 880000_i64, 1000000_i64],
            )
            .expect("floor fixture mutation"),
        1
    );
}

#[test]
fn sharing_principal_floor_observation_cannot_outlive_its_heartbeat() {
    let row = SharingPrincipalFloorRow {
        ready: 1,
        oldest_heartbeat: Some(1000000),
    };
    assert!(sharing_principal_floor_observation_ready(&row, 1120000));
    assert!(!sharing_principal_floor_observation_ready(&row, 1120001));
    assert!(!sharing_principal_floor_observation_ready(&row, 999999));
    assert!(!sharing_principal_floor_observation_ready(
        &SharingPrincipalFloorRow {
            ready: 1,
            oldest_heartbeat: None
        },
        1000000
    ));
    assert!(!sharing_principal_floor_observation_ready(
        &SharingPrincipalFloorRow {
            ready: 0,
            oldest_heartbeat: Some(1000000)
        },
        1000000
    ));
}

#[tokio::test]
async fn sharing_principal_floor_missing_membership_is_unavailable() {
    assert!(MembershipManager::unavailable()
        .sharing_session_principal_floor_ready()
        .await
        .is_err());
    assert!(MembershipManager::unavailable()
        .observe_source_admission_members()
        .await
        .is_err());
}

#[test]
fn sharing_source_purpose_installation_invalidates_legacy_proof_and_requires_current_master() {
    let connection = sharing_principal_floor_fixture();
    connection.execute_batch("CREATE TABLE cluster_sharing_membership_intents(id TEXT); CREATE TABLE cluster_sharing_join_declarations(token_hash TEXT); CREATE TABLE cluster_join_tokens(token_hash TEXT,state TEXT); CREATE TABLE sharing_purpose_census_intents(boot_metadata TEXT)").expect("unit transition and census metadata");
    for sql in sharing_membership_generation_schema() {
        connection
            .execute_batch(&sql)
            .expect("durable generation fixture");
    }
    connection.execute("INSERT INTO cluster_node_capabilities SELECT node_id,?1,last_seen_at FROM cluster_nodes",[SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY]).expect("catalogue proofs");
    let legacy = source_admission_members_for_unit_test(1, &BTreeSet::from([1, 2]), 1000000)
        .expect("legacy observation");
    let allows = |observation: &SourceAdmissionMembers| {
        let (guard, roster, cutoff, now) = observation
            .write_guard(1000001, 1, 2, 3)
            .expect("current private witness");
        connection
            .query_row(
                &format!("SELECT {guard}"),
                rusqlite::params![roster, cutoff, now],
                |row| row.get::<_, bool>(0),
            )
            .expect("same-write predicate")
    };
    assert!(
        allows(&legacy),
        "census-only metadata preserves legacy floor"
    );
    connection
        .execute_batch("CREATE TABLE sharing_catalogue_keys(partial TEXT)")
        .expect("purpose material appears after observation");
    assert!(
        !allows(&legacy),
        "partial installation cannot inherit a cached Both proof"
    );
    let master = crate::secrets::CredentialKey::from_bytes([129; 32]);
    let current = source_admission_members_with_purpose_for_unit_test(
        1,
        &BTreeSet::from([1, 2]),
        1000000,
        &master,
    )
    .expect("private fixture master witness");
    assert!(!allows(&current), "missing purpose capability refuses");
    connection.execute("INSERT INTO cluster_node_capabilities SELECT node_id,?1,last_seen_at FROM cluster_nodes",[SHARING_PURPOSE_KEYS_CAPABILITY]).expect("binary proofs");
    assert!(
        !allows(&current),
        "capability alone cannot replace master proof"
    );
    let fingerprint = format!(
        "sharing_purpose_master_v1:{}",
        master.sharing_purpose_master_fingerprint()
    );
    connection.execute("INSERT INTO cluster_node_capabilities SELECT node_id,?1,last_seen_at FROM cluster_nodes",[fingerprint.as_str()]).expect("actual fixture master proof");
    assert!(allows(&current));
    connection
        .execute(
            "DELETE FROM cluster_node_capabilities WHERE node_id='learner' AND capability=?1",
            [SHARING_PURPOSE_KEYS_CAPABILITY],
        )
        .expect("purpose proof disappears after observation");
    assert!(!allows(&current));
    connection.execute("INSERT INTO cluster_node_capabilities SELECT node_id,?1,last_seen_at FROM cluster_nodes WHERE node_id='learner'",[SHARING_PURPOSE_KEYS_CAPABILITY]).expect("purpose proof returns");
    connection.execute("UPDATE cluster_node_capabilities SET last_seen_at=last_seen_at-1 WHERE node_id='learner' AND capability=?1",[fingerprint]).expect("master proof no longer coupled to heartbeat");
    assert!(!allows(&current));
}

#[test]
fn sharing_source_member_observation_rechecks_both_floors_and_unresolved_intents() {
    let connection = sharing_principal_floor_fixture();
    connection.execute_batch("CREATE TABLE cluster_sharing_membership_intents (id TEXT); CREATE TABLE cluster_sharing_join_declarations (token_hash TEXT); CREATE TABLE cluster_join_tokens (token_hash TEXT, state TEXT); CREATE TABLE source_write_receipts (id INTEGER PRIMARY KEY)").expect("candidate transition fence fixture");
    for sql in sharing_membership_generation_schema() {
        connection
            .execute_batch(&sql)
            .expect("candidate generation factory");
    }
    let mut observation =
        source_admission_members_for_unit_test(1, &BTreeSet::from([1, 2]), 1000000)
            .expect("unit-only fixture observation");
    observation.completed_at_ms = 1000001;
    let (guard, members, cutoff, now) = observation
        .write_guard(1000002, 1, 2, 3)
        .expect("fresh server observation");
    let write = |id: i64| {
        connection
            .execute(
                &format!("INSERT INTO source_write_receipts SELECT {id} WHERE {guard}"),
                rusqlite::params![members, cutoff, now],
            )
            .expect("same-mutation closed Source guard")
    };
    assert_eq!(write(1), 0, "principal capability alone is insufficient");
    connection.execute("INSERT INTO cluster_node_capabilities SELECT node_id,?1,last_seen_at FROM cluster_nodes", [SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY]).expect("allocator binary proofs");
    assert_eq!(write(1), 1);
    connection
        .execute(
            "INSERT INTO cluster_sharing_membership_intents VALUES('unresolved')",
            [],
        )
        .expect("membership proposal starts after observation");
    assert_eq!(
        write(2),
        0,
        "an earlier observation cannot bypass an unresolved transition"
    );
    connection
        .execute("DELETE FROM cluster_sharing_membership_intents", [])
        .expect("authoritative fixture resolution");
    assert_eq!(
        write(2),
        0,
        "a completed transition also invalidates the old observation"
    );
    observation.membership_generation = 2;
    let (guard, members, cutoff, now) = observation
        .write_guard(1000002, 1, 2, 3)
        .expect("new lineage observation");
    let write = |id: i64| {
        connection
            .execute(
                &format!("INSERT INTO source_write_receipts SELECT {id} WHERE {guard}"),
                rusqlite::params![members, cutoff, now],
            )
            .expect("fresh lineage guard")
    };
    connection.execute_batch("INSERT INTO cluster_sharing_join_declarations VALUES('joining'); INSERT INTO cluster_join_tokens VALUES('joining','redeeming')").expect("joining member declaration");
    assert_eq!(write(2), 0);
    connection
        .execute("UPDATE cluster_join_tokens SET state='joined'", [])
        .expect("fixture join finishes");
    assert_eq!(write(2), 1);
    connection
        .execute(
            "UPDATE cluster_nodes SET last_seen_at=last_seen_at-1 WHERE node_id='learner'",
            [],
        )
        .expect("legacy heartbeat races admission");
    assert_eq!(write(3), 0);
    assert!(connection
        .execute(
            "UPDATE cluster_sharing_membership_generation SET generation=0",
            []
        )
        .is_err());
    assert!(connection
        .execute("DELETE FROM cluster_sharing_membership_generation", [])
        .is_err());
    assert!(connection
        .execute(
            "INSERT OR REPLACE INTO cluster_sharing_membership_generation VALUES(1,0)",
            []
        )
        .is_err());
    assert!(observation.write_guard(1000000, 1, 2, 3).is_err());
    assert!(observation.write_guard(1005001, 1, 2, 3).is_err());
    assert!(observation.write_guard(1000002, 1, 1, 3).is_err());
    assert!(observation.write_guard(1000002, 0, 2, 3).is_err());
}

#[test]
fn sharing_source_member_observation_refuses_completed_identity_change_and_partial_generation() {
    let connection = sharing_principal_floor_fixture();
    connection.execute_batch("CREATE TABLE cluster_sharing_membership_intents(id TEXT); CREATE TABLE cluster_sharing_join_declarations(token_hash TEXT); CREATE TABLE cluster_join_tokens(token_hash TEXT,state TEXT)").expect("transition fixture");
    for sql in sharing_membership_generation_schema() {
        connection.execute_batch(&sql).expect("generation factory");
    }
    connection.execute("INSERT INTO cluster_node_capabilities SELECT node_id,?1,last_seen_at FROM cluster_nodes",[SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY]).expect("both current writer capabilities");
    let mut observation =
        source_admission_members_for_unit_test(1, &BTreeSet::from([1, 2]), 1000000)
            .expect("actual fixture roster");
    let allows = |observation: &SourceAdmissionMembers| {
        let (guard, members, cutoff, now) = observation
            .write_guard(1000001, 1, 2, 3)
            .expect("fresh observation");
        connection
            .query_row(
                &format!("SELECT {guard}"),
                rusqlite::params![members, cutoff, now],
                |row| row.get::<_, bool>(0),
            )
            .expect("guarded mutation predicate")
    };
    assert!(allows(&observation));
    connection.execute_batch("INSERT INTO cluster_nodes VALUES('transient',3,1000000,NULL,'learner'); DELETE FROM cluster_nodes WHERE node_id='transient'").expect("completed identity transition restores the original visible roster");
    assert!(
        !allows(&observation),
        "restored roster and empty intent tables cannot revive the old observation"
    );
    observation.membership_generation = 2;
    assert!(allows(&observation));
    connection
        .execute_batch("DROP TRIGGER cluster_sharing_generation_intent_update")
        .expect("partial generation factory fixture");
    assert!(
        !allows(&observation),
        "a matching number without the complete durable factory is unavailable"
    );
}

#[test]
fn heartbeat_reconciles_addresses_only_for_the_same_live_identity() {
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, raft_id INTEGER NOT NULL, \
                   raft_address TEXT NOT NULL, api_address TEXT NOT NULL, \
                   last_seen_at INTEGER NOT NULL, removed_at INTEGER, role TEXT); \
                 INSERT INTO cluster_nodes VALUES \
                   ('live', 1, '127.0.0.1:32401', '127.0.0.1:32402', 10, NULL, 'voter'), \
                   ('removed', 2, 'old:32401', 'old:32402', 10, 20, 'learner'), \
                   ('collision', 3, 'owner:32401', 'owner:32402', 10, NULL, 'voter');",
        )
        .expect("seed projected cluster nodes");

    assert_eq!(
        connection
            .execute(
                UPSERT_CLUSTER_NODE_HEARTBEAT_SQL,
                rusqlite::params![
                    "live",
                    1_i64,
                    "10.42.5.236:32401",
                    "10.42.5.236:32402",
                    30_i64,
                    "learner"
                ],
            )
            .expect("reconcile supported voter readdress"),
        1
    );
    let live = connection
        .query_row(
            "SELECT raft_address, api_address, last_seen_at, role \
                 FROM cluster_nodes WHERE node_id = 'live'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .expect("read reconciled node");
    assert_eq!(
        live,
        (
            "10.42.5.236:32401".to_owned(),
            "10.42.5.236:32402".to_owned(),
            30,
            "voter".to_owned(),
        ),
        "the heartbeat owns addresses and freshness, not the admitted role"
    );

    for (node_id, raft_id) in [("removed", 2_i64), ("collision", 99_i64)] {
        assert_eq!(
            connection
                .execute(
                    UPSERT_CLUSTER_NODE_HEARTBEAT_SQL,
                    rusqlite::params![
                        node_id,
                        raft_id,
                        "attacker:32401",
                        "attacker:32402",
                        40_i64,
                        "voter"
                    ],
                )
                .expect("refuse address claim"),
            0,
            "{node_id} must not accept the heartbeat claim"
        );
    }
    let unchanged = connection
        .prepare(
            "SELECT node_id, raft_address, api_address, last_seen_at \
                 FROM cluster_nodes WHERE node_id != 'live' ORDER BY node_id",
        )
        .expect("prepare unchanged identities")
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .expect("read unchanged identities")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect unchanged identities");
    assert_eq!(
        unchanged,
        vec![
            (
                "collision".to_owned(),
                "owner:32401".to_owned(),
                "owner:32402".to_owned(),
                10,
            ),
            (
                "removed".to_owned(),
                "old:32401".to_owned(),
                "old:32402".to_owned(),
                10,
            ),
        ]
    );
}

#[test]
fn operations_peer_directory_preserves_identities_beyond_the_probe_limit() {
    let now = 1_000_000;
    let members = (1_u64..=13).collect::<BTreeSet<_>>();
    let rows = (2_u64..=13)
        .map(|raft_id| ActivityPeerRow {
            node_id: format!("node-{raft_id}"),
            raft_id,
            last_seen_at: now,
            http_base: Some(format!("http://node-{raft_id}:32400")),
        })
        .collect();

    let peers = MembershipManager::operations_peer_directory(now, &members, rows);

    assert_eq!(peers.len(), 12);
    assert!(peers.len() > MAX_OPERATIONS_PEERS);
    assert_eq!(peers.first().map(|peer| peer.raft_id), Some(2));
    assert_eq!(peers.last().map(|peer| peer.raft_id), Some(13));
    assert!(peers.iter().all(|peer| peer.reachable));
}

#[test]
fn operations_peer_query_materializes_only_the_committed_roster() {
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, raft_id INTEGER NOT NULL, \
                   last_seen_at INTEGER NOT NULL, removed_at INTEGER); \
                 CREATE TABLE cluster_node_http (\
                   node_id TEXT PRIMARY KEY, public_http_url TEXT); \
                 CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY);",
        )
        .expect("create production directory tables");
    {
        let mut node = connection
            .prepare("INSERT INTO cluster_nodes VALUES (?1, ?2, 1000000, NULL)")
            .expect("prepare node insert");
        let mut http = connection
            .prepare("INSERT INTO cluster_node_http VALUES (?1, ?2)")
            .expect("prepare HTTP insert");
        for raft_id in 1_u64..=13 {
            let node_id = format!("node-{raft_id}");
            node.execute(rusqlite::params![node_id, raft_id as i64])
                .expect("insert committed node");
            http.execute(rusqlite::params![
                format!("node-{raft_id}"),
                format!("http://node-{raft_id}:32400")
            ])
            .expect("insert committed HTTP endpoint");
        }
        for index in 0_u64..1_000 {
            let node_id = format!("abandoned-{index}");
            node.execute(rusqlite::params![node_id, (10_000 + index) as i64])
                .expect("insert abandoned join row");
            http.execute(rusqlite::params![
                format!("abandoned-{index}"),
                format!("http://abandoned-{index}:32400")
            ])
            .expect("insert abandoned HTTP endpoint");
        }
    }

    let members = (1_u64..=13).collect::<BTreeSet<_>>();
    let members_json = bounded_committed_raft_ids_json(&members).expect("bounded committed roster");
    let mut statement = connection
        .prepare(OPERATIONS_PEERS_SQL)
        .expect("prepare the production operations directory query");
    let rows = statement
        .query_map(rusqlite::params!["node-1", members_json], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("query exact committed peers")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect exact committed peers");

    assert_eq!(rows.len(), 12);
    assert!(rows.len() > MAX_OPERATIONS_PEERS);
    assert_eq!(rows.first(), Some(&("node-2".to_owned(), 2_i64)));
    assert_eq!(rows.last(), Some(&("node-13".to_owned(), 13_i64)));
    assert!(rows
        .iter()
        .all(|(node_id, _)| !node_id.starts_with("abandoned-")));
}

#[test]
fn k06_final_clock_directory_reads_sender_fence_in_same_snapshot() {
    let db = rusqlite::Connection::open_in_memory().expect("owned sqlite");
    db.execute_batch(
        "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, raft_id INTEGER, \
               last_seen_at INTEGER, removed_at INTEGER); \
             CREATE TABLE cluster_node_http (node_id TEXT PRIMARY KEY, public_http_url TEXT); \
             CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY); \
             INSERT INTO cluster_nodes VALUES ('local',1,0,NULL),('learner',2,0,NULL); \
             INSERT INTO cluster_node_http VALUES ('learner','http://learner:32400');",
    )
    .expect("fixture schema");
    let read = |sender: &str| {
        db.prepare(AUTHENTICATED_CLOCK_PEERS_SQL)
            .expect("production query")
            .query_map(rusqlite::params!["local", "[1,2]", sender], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, bool>(4)?))
            })
            .expect("consistent statement")
            .collect::<Result<Vec<_>, _>>()
            .expect("whole directory")
    };
    assert_eq!(read("learner"), vec![("learner".into(), true)]);
    assert_eq!(read("foreign"), vec![("learner".into(), false)]);
    db.execute("INSERT INTO cluster_node_removals VALUES ('learner')", [])
        .expect("actual durable sender fence");
    // Full coverage still includes the peer; authority in that SAME read
    // is false, not borrowed from a preceding unfenced response check.
    assert_eq!(read("learner"), vec![("learner".into(), false)]);
    db.execute(
        "UPDATE cluster_nodes SET removed_at=1 WHERE node_id='learner'",
        [],
    )
    .expect("actual durable removal");
    assert!(read("learner").is_empty());
}

#[tokio::test]
async fn k06_startup_activation_retains_deadline_without_reclassifying_clock() {
    // An actual standalone guard supplies no invented remote membership
    // or clock samples. This isolates phase budget from valid clock age.
    let guard = Arc::new(ClusterClockGuard::new(false));
    let deadline = tokio::time::Instant::now() + Duration::from_millis(20);
    let admission = StartupActivationAdmission {
        clock: guard
            .acquire_owned_for(ClockDecision::MembershipChange)
            .expect("NoPeers"),
        deadline,
        settlement_pause: None,
    };
    admission
        .revalidate()
        .expect("before original phase deadline");
    tokio::time::sleep_until(deadline).await;
    assert!(matches!(
        admission.revalidate(),
        Err(MembershipError::Internal(_))
    ));
    admission
        .clock
        .revalidate()
        .expect("clock proof remains valid");
    assert_eq!(admission.deadline, deadline, "no replenished phase budget");
}

#[test]
fn k06_durable_activation_settlement_keeps_original_phase_and_refuses_next_submission() {
    let worker = std::thread::Builder::new()
        .name("k06-delayed-settlement".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("owned settlement runtime")
                .block_on(Box::pin(delayed_activation_settlement_fixture(false)));
        })
        .expect("owned settlement thread");
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn k06_direct_activation_phase_precedes_clock_refusal_before_and_after_capture() {
    let worker = std::thread::Builder::new()
        .name("k06-direct-phase".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("owned phase runtime")
                .block_on(Box::pin(delayed_activation_settlement_fixture(true)));
        })
        .expect("owned phase thread");
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
}

async fn delayed_activation_settlement_fixture(direct_phase: bool) {
    use crate::cluster::migration::{select_daemon_store_observing, StartupClockObserver};
    use crate::config::Config;

    struct SingletonObserver;
    impl StartupClockObserver for SingletonObserver {
        fn start(
            &self,
            manager: MembershipManager,
            node: String,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), crate::error::StoreError>> + Send + '_>>
        {
            Box::pin(async move {
                let roster = manager
                    .clock_peers()
                    .await
                    .map_err(|error| crate::error::StoreError::Database(error.to_string()))?;
                let applied = roster.membership.as_ref().expect("actual singleton watch");
                assert_eq!(applied.members, BTreeSet::from([applied.local_node]));
                assert_eq!(applied.voters, applied.members);
                assert!(roster.peers.is_empty());
                let nonce = uuid::Uuid::now_v7().to_string();
                let body = b"signed singleton route control, not a remote clock sample";
                let path = "/_internal/v1/clock";
                let signature = manager
                    .sign_internal_peer_response(&node, &nonce, path, body)
                    .expect("actual signing manager");
                let message = internal_peer_response_message(&node, &node, &nonce, path, body)
                    .expect("exact signed message");
                let decoded = hex::decode(&signature).expect("actual signature bytes");
                assert!(manager
                    .activity_signature_is_valid(&node, &message, &decoded)
                    .await
                    .expect("actual durable signing-key verification"));
                let changed =
                    internal_peer_response_message(&node, &node, &nonce, path, b"changed")
                        .expect("changed signed message");
                assert!(!manager
                    .activity_signature_is_valid(&node, &changed, &decoded)
                    .await
                    .expect("changed body refuses signature"));
                // A singleton has no peer HTTP exchange. Never relabel
                // self-signing as an authorized learner/peer route.
                assert!(!manager
                    .authorize_internal_peer_member_response(
                        &node, &node, &nonce, path, body, &signature,
                    )
                    .await
                    .expect("self-relay authorization refuses"));
                let guard = manager.clock_guard();
                let round = guard
                    .roster_for_peer_directory(&roster)
                    .expect("proved actual singleton roster");
                assert!(guard.publish(round, BTreeMap::new()));
                Ok(())
            })
        }
    }

    let root = tempfile::tempdir().expect("owned activation directory");
    let held: Vec<_> = (0..3)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").expect("owned fixture port"))
        .collect();
    let addresses: Vec<_> = held
        .iter()
        .map(|listener| listener.local_addr().expect("address"))
        .collect();
    let mut config = Config::default();
    config.storage.data_dir = root.path().into();
    config.server.bind = addresses[0];
    config.cluster.raft_bind = addresses[1];
    config.cluster.api_bind = addresses[2];
    config.cluster.advertise_host = "localhost".into();
    config.cluster.join_url = format!("http://{}", addresses[0]);
    config.cluster.artwork_url = config.cluster.join_url.clone();
    drop(held);
    drop(crate::store::SqliteStore::open(&root.path().join("plurx.db")).expect("source schema"));
    let selected = Box::pin(select_daemon_store_observing(
        &config,
        Some(&SingletonObserver),
    ))
    .await
    .expect("actual singleton activation and lock owner");
    let client = selected.local_client().expect("actual Client");
    let installed = client
        .local_membership_admission()
        .expect("installed policy");
    let original_deadline = installed
        .as_any()
        .downcast_ref::<StartupMembershipAdmission>()
        .expect("same installed phase policy")
        .startup_deadline()
        .expect("original 45-second deadline");
    let manager = selected.membership_manager();
    if direct_phase {
        let policy = installed
            .as_any()
            .downcast_ref::<StartupMembershipAdmission>()
            .expect("exact installed policy");
        let (entered, captured) = tokio::sync::oneshot::channel();
        let (release, resumed) = tokio::sync::oneshot::channel();
        *policy
            .activation_capture_pause
            .once
            .lock()
            .expect("instance pause") = Some((entered, resumed));
        let pending_manager = manager.clone();
        let pending = tokio::spawn(async move {
            pending_manager
                .finish_clock_observation(original_deadline)
                .await
        });
        captured.await.expect("actual clock capture reached");
        assert!(!pending.is_finished());
        tokio::time::sleep_until(original_deadline).await;
        release.send(()).expect("release original capture");
        assert!(matches!(
            pending.await.expect("capture consumer completed"),
            Err(MembershipError::Internal(_))
        ));
        assert!(matches!(
            manager.finish_clock_observation(original_deadline).await,
            Err(MembershipError::Internal(_))
        ));
        assert_eq!(policy.startup_deadline(), Some(original_deadline));
        selected
            .shutdown()
            .await
            .expect("actual phase Client drain");
        return;
    }
    let (manager, mut admission) = manager
        .finish_clock_observation(original_deadline)
        .await
        .expect("actual activation proof, no reconstructed owner");
    assert_eq!(admission.deadline, original_deadline);
    client
        .execute("DROP TRIGGER cluster_node_removal_insert_guard", params!())
        .await
        .expect("make actual transaction effect observable");
    let (entered, durable) = tokio::sync::oneshot::channel();
    let (release, resumed) = tokio::sync::oneshot::channel();
    admission.settlement_pause = Some(StartupSettlementPause {
        once: std::sync::Mutex::new(Some((entered, resumed))),
    });
    let admission = Arc::new(admission);
    let pending_manager = manager.clone();
    let pending_admission = Arc::clone(&admission);
    let pending = tokio::spawn(async move {
        pending_manager
            .install_membership_schema_only_admitted(Some(&pending_admission))
            .await
    });
    durable
        .await
        .expect("actual transaction settled durably before pause");
    let rows = client
        .query_consistent_map::<CountRow, _>(
            "SELECT COUNT(*) AS count FROM sqlite_master \
                 WHERE type='trigger' AND name='cluster_node_removal_insert_guard'",
            params!(),
        )
        .await
        .expect("actual durable trigger readback");
    assert_eq!(rows.first().map(|row| row.count), Some(1));
    assert!(!pending.is_finished(), "caller has not observed settlement");
    tokio::time::sleep_until(original_deadline).await;
    assert!(matches!(
        admission.revalidate(),
        Err(MembershipError::Internal(_))
    ));
    assert_eq!(admission.deadline, original_deadline);
    release
        .send(())
        .expect("release settlement, not cancel accepted work");
    assert!(matches!(
        pending.await.expect("retained consumer completion"),
        Err(MembershipError::Internal(_))
    ));
    assert!(matches!(
        manager
            .install_membership_schema_only_admitted(Some(&admission))
            .await,
        Err(MembershipError::Internal(_))
    ));
    // Do not reacquire a clock ticket here: that separate API can refuse
    // expired clock evidence before evaluating the phase. This control
    // proves the original admission's post-settlement/next-write fence.
    assert_eq!(
        admission.deadline, original_deadline,
        "no renewed phase budget"
    );
    selected
        .shutdown()
        .await
        .expect("actual Client and directory owner drain");
}

#[cfg(feature = "cluster-read-cost-validation")]
#[test]
fn k06_actual_activation_transaction_commits_after_phase_expiry_without_next_submission() {
    let hold = Arc::new(std::sync::Mutex::new(None));
    let worker_hold = Arc::clone(&hold);
    let (completed, completion) = std::sync::mpsc::channel();
    let worker = std::thread::Builder::new()
        .name("k06-precommit-owner".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let root = tempfile::tempdir().expect("actual precommit fixture");
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("actual precommit fixture");
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                runtime.block_on(Box::pin(actual_precommit_activation_fixture(
                    Arc::clone(&worker_hold),
                    root.path(),
                )));
            }));
            if result.is_err() {
                // Failure is not terminal-drain evidence. Release the injection,
                // but retain the runtime owning accepted tasks/Client/lock.
                drop(worker_hold.lock().expect("owned writer hold").take());
                std::mem::forget(root);
                std::mem::forget(runtime);
            } else {
                drop(runtime);
                drop(root);
            }
            let _ = completed.send(result);
        })
        .expect("actual precommit fixture");
    let result = completion.recv_timeout(Duration::from_secs(110));
    drop(hold.lock().expect("owned writer hold").take());
    match result {
        Ok(Ok(())) => {}
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(error) => panic!("precommit worker did not complete; ownership retained: {error}"),
    }
    let finished_by = std::time::Instant::now() + Duration::from_secs(1);
    while !worker.is_finished() && std::time::Instant::now() < finished_by {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        worker.is_finished(),
        "precommit worker terminal return missing"
    );
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
}

#[cfg(feature = "cluster-read-cost-validation")]
async fn actual_precommit_activation_fixture(
    hold: Arc<std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>>,
    root: &std::path::Path,
) {
    use crate::cluster::migration::{select_daemon_store_observing, StartupClockObserver};
    use crate::error::StoreError;
    struct CapturedWriter {
        entered: tokio::sync::oneshot::Receiver<()>,
        release: std::sync::mpsc::Sender<()>,
        deadline: tokio::time::Instant,
        manager: MembershipManager,
        baseline_submissions: usize,
    }
    struct Observer(std::sync::Mutex<Option<tokio::sync::oneshot::Sender<CapturedWriter>>>);
    impl StartupClockObserver for Observer {
        fn start<'a>(
            &'a self,
            manager: MembershipManager,
            node: String,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), StoreError>> + Send + 'a>>
        {
            Box::pin(async move {
                let roster = manager
                    .clock_peers()
                    .await
                    .map_err(|e| StoreError::Database(e.to_string()))?;
                let applied = roster.membership.as_ref().expect("actual applied roster");
                assert_eq!(applied.members, BTreeSet::from([applied.local_node]));
                assert_eq!(applied.members, applied.voters);
                assert!(roster.peers.is_empty());
                let nonce = uuid::Uuid::now_v7().to_string();
                let body = b"actual singleton ownership, not a learner HTTP exchange";
                let signature = manager
                    .sign_internal_peer_response(&node, &nonce, "/_internal/v1/clock", body)
                    .expect("actual precommit fixture");
                let message = internal_peer_response_message(
                    &node,
                    &node,
                    &nonce,
                    "/_internal/v1/clock",
                    body,
                )
                .expect("actual precommit fixture");
                assert!(manager
                    .activity_signature_is_valid(
                        &node,
                        &message,
                        &hex::decode(signature).expect("actual precommit fixture")
                    )
                    .await
                    .expect("actual precommit fixture"));
                let guard = manager.clock_guard();
                assert!(guard.publish(
                    guard
                        .roster_for_peer_directory(&roster)
                        .expect("actual precommit fixture"),
                    BTreeMap::new()
                ));
                let inner = manager
                    .replicated_inner()
                    .expect("actual precommit fixture");
                let installed = inner
                    .client
                    .local_membership_admission()
                    .expect("actual precommit fixture");
                let policy = installed
                    .as_any()
                    .downcast_ref::<StartupMembershipAdmission>()
                    .expect("actual precommit fixture");
                let deadline = policy.startup_deadline().expect("actual precommit fixture");
                inner
                    .client
                    .execute(
                        "DROP TRIGGER IF EXISTS cluster_node_removal_insert_guard",
                        params!(),
                    )
                    .await
                    .expect("actual precommit fixture");
                let (entered, release) = inner
                    .client
                    .validation_hold_transaction_before_commit(
                        REQUIRE_REMOVAL_INTENT_SQL.to_owned(),
                        deadline.into_std() + Duration::from_secs(5),
                    )
                    .await
                    .expect("actual precommit fixture");
                let baseline_submissions = policy
                    .activation_metadata_submissions
                    .load(std::sync::atomic::Ordering::Relaxed);
                self.0
                    .lock()
                    .expect("actual precommit fixture")
                    .take()
                    .expect("actual precommit fixture")
                    .send(CapturedWriter {
                        entered,
                        release,
                        deadline,
                        manager,
                        baseline_submissions,
                    })
                    .map_err(|_| StoreError::Database("fixture observer disappeared".into()))?;
                Ok(())
            })
        }
    }
    fn trigger_count(path: &std::path::Path) -> i64 {
        let connection =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .expect("actual precommit fixture");
        connection.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name='cluster_node_removal_insert_guard'", [], |row| row.get(0)).expect("actual precommit fixture")
    }
    let held: Vec<_> = (0..3)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").expect("actual precommit fixture"))
        .collect();
    let addresses: Vec<_> = held
        .iter()
        .map(|listener| listener.local_addr().expect("actual precommit fixture"))
        .collect();
    let mut config = crate::config::Config::default();
    config.storage.data_dir = root.into();
    config.server.bind = addresses[0];
    config.cluster.raft_bind = addresses[1];
    config.cluster.api_bind = addresses[2];
    config.cluster.advertise_host = "localhost".into();
    config.cluster.join_url = format!("http://{}", addresses[0]);
    config.cluster.artwork_url = config.cluster.join_url.clone();
    drop(held);
    drop(
        crate::store::SqliteStore::open(&root.join("plurx.db")).expect("actual precommit fixture"),
    );
    let (capture, captured) = tokio::sync::oneshot::channel();
    let mut pending = tokio::spawn(async move {
        let observer = Observer(std::sync::Mutex::new(Some(capture)));
        Box::pin(select_daemon_store_observing(&config, Some(&observer))).await
    });
    let CapturedWriter {
        entered,
        release,
        deadline,
        manager,
        baseline_submissions,
    } = tokio::time::timeout(Duration::from_secs(15), captured)
        .await
        .expect("bounded actual observer capture")
        .expect("actual precommit fixture");
    *hold.lock().expect("owned writer hold") = Some(release);
    tokio::time::timeout(Duration::from_secs(10), entered)
        .await
        .expect("bounded actual precommit entry")
        .expect("actual transaction statements completed before commit");
    let database = root.join("hiqlite/state_machine/db/plurx.db");
    assert_eq!(
        trigger_count(&database),
        0,
        "independent reader cannot see uncommitted trigger"
    );
    assert!(!pending.is_finished());
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(".plurxd.lock"))
        .expect("actual precommit fixture");
    assert!(matches!(
        lock.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    tokio::time::sleep_until(deadline).await;
    assert!(
        !pending.is_finished(),
        "accepted SQL not cancelled at phase expiry"
    );
    assert!(matches!(
        lock.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    let release = hold.lock().expect("owned writer hold").take();
    release
        .expect("actual writer release owner")
        .send(())
        .expect("release actual uncommitted writer");
    let error = match tokio::time::timeout(Duration::from_secs(15), &mut pending)
        .await
        .expect("bounded post-release activation completion; no cancellation proof")
        .expect("actual precommit fixture")
    {
        Ok(_) => panic!("expired activation succeeded"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("original deadline"), "{error}");
    let installed = manager
        .replicated_inner()
        .expect("actual precommit fixture")
        .client
        .local_membership_admission()
        .expect("actual precommit fixture");
    let policy = installed
        .as_any()
        .downcast_ref::<StartupMembershipAdmission>()
        .expect("actual precommit fixture");
    assert_eq!(policy.startup_deadline(), Some(deadline));
    assert_eq!(
        policy
            .activation_metadata_submissions
            .load(std::sync::atomic::Ordering::Relaxed),
        baseline_submissions,
        "no next metadata submission after expired transaction"
    );
    assert_eq!(
        trigger_count(&database),
        1,
        "actual transaction committed after expiry"
    );
    let cleanup_limit = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock)
                if tokio::time::Instant::now() < cleanup_limit =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => panic!("actual terminal cleanup did not release lock: {other:?}"),
        }
    }
    lock.unlock().expect("actual precommit fixture");
    assert!(std::net::TcpListener::bind(addresses[1]).is_ok());
    assert!(std::net::TcpListener::bind(addresses[2]).is_ok());
}

#[test]
fn cache_admin_revocation_roster_includes_pending_removals_and_fails_on_omission() {
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, raft_id INTEGER NOT NULL, \
                   last_seen_at INTEGER NOT NULL, removed_at INTEGER); \
                 CREATE TABLE cluster_node_http (\
                   node_id TEXT PRIMARY KEY, public_http_url TEXT); \
                 CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY); \
                 INSERT INTO cluster_nodes VALUES \
                   ('node-1', 1, 1000000, NULL), \
                   ('node-2', 2, 1000000, NULL), \
                   ('node-3', 3, 1000000, NULL), \
                   ('abandoned', 99, 1000000, NULL); \
                 INSERT INTO cluster_node_http VALUES \
                   ('node-1', 'http://node-1:32400'), \
                   ('node-2', 'http://node-2:32400'), \
                   ('node-3', 'http://node-3:32400'), \
                   ('abandoned', 'http://abandoned:32400'); \
                 INSERT INTO cluster_node_removals VALUES ('node-2');",
        )
        .expect("create revocation roster fixture");
    let members = BTreeSet::from([1_u64, 2, 3]);
    let members_json = bounded_committed_raft_ids_json(&members).expect("bounded committed roster");
    let read = |sql| {
        let mut statement = connection.prepare(sql).expect("prepare roster query");
        statement
            .query_map(rusqlite::params!["node-1", members_json.as_str()], |row| {
                let raft_id: i64 = row.get(1)?;
                Ok(ActivityPeerRow {
                    node_id: row.get(0)?,
                    raft_id: u64::try_from(raft_id).unwrap_or_default(),
                    last_seen_at: row.get(2)?,
                    http_base: row.get(3)?,
                })
            })
            .expect("query committed roster")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect committed roster")
    };

    let operations = read(OPERATIONS_PEERS_SQL);
    assert_eq!(
        operations
            .iter()
            .map(|peer| peer.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["node-3"]
    );
    let revocation = read(CACHE_ADMIN_REVOCATION_PEERS_SQL);
    assert_eq!(
        revocation
            .iter()
            .map(|peer| peer.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["node-2", "node-3"]
    );
    assert!(MembershipManager::cache_admin_revocation_peer_directory(
        1_000_000,
        &members,
        1,
        revocation.clone(),
    )
    .is_ok());

    let missing_pending_member = revocation
        .iter()
        .filter(|peer| peer.raft_id != 2)
        .cloned()
        .collect();
    assert!(MembershipManager::cache_admin_revocation_peer_directory(
        1_000_000,
        &members,
        1,
        missing_pending_member,
    )
    .is_err());
    let mut missing_endpoint = revocation;
    missing_endpoint[0].http_base = None;
    assert!(MembershipManager::cache_admin_revocation_peer_directory(
        1_000_000,
        &members,
        1,
        missing_endpoint,
    )
    .is_err());
}

#[test]
fn status_protocol_query_materializes_only_the_committed_roster() {
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, raft_id INTEGER NOT NULL, \
                   last_seen_at INTEGER NOT NULL, removed_at INTEGER); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_capabilities (\
                   node_id TEXT NOT NULL, capability TEXT NOT NULL, last_seen_at INTEGER NOT NULL, \
                   PRIMARY KEY(node_id, capability));",
        )
        .expect("create production status tables");
    {
        let mut node = connection
            .prepare("INSERT INTO cluster_nodes VALUES (?1, ?2, 1000000, NULL)")
            .expect("prepare node insert");
        let mut capability = connection
            .prepare("INSERT INTO cluster_node_capabilities VALUES (?1, ?2, 1000000)")
            .expect("prepare capability insert");
        for raft_id in 1_u64..=13 {
            let node_id = format!("node-{raft_id}");
            node.execute(rusqlite::params![node_id, raft_id as i64])
                .expect("insert committed node");
            if raft_id % 2 == 1 {
                capability
                    .execute(rusqlite::params![
                        format!("node-{raft_id}"),
                        LEARNER_PROTOCOL_CAPABILITY
                    ])
                    .expect("mark committed node ready");
            }
        }
        for index in 0_u64..1_000 {
            node.execute(rusqlite::params![
                format!("abandoned-{index}"),
                (10_000 + index) as i64
            ])
            .expect("insert abandoned join row");
        }
    }

    let members = (1_u64..=13).collect::<BTreeSet<_>>();
    let members_json = bounded_committed_raft_ids_json(&members).expect("bounded committed roster");
    let sql = committed_capability_unready_nodes_sql(LEARNER_PROTOCOL_CAPABILITY);
    let mut statement = connection
        .prepare(&sql)
        .expect("prepare the production status protocol query");
    let rows = statement
        .query_map(rusqlite::params![members_json], |row| {
            row.get::<_, String>(0)
        })
        .expect("query exact committed pending nodes")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect exact committed pending nodes");

    assert_eq!(
        rows.iter().cloned().collect::<BTreeSet<_>>(),
        [2_u64, 4, 6, 8, 10, 12]
            .into_iter()
            .map(|raft_id| format!("node-{raft_id}"))
            .collect::<BTreeSet<_>>()
    );
    assert!(rows
        .iter()
        .all(|node_id| !node_id.starts_with("abandoned-")));

    // Supplemental wiring guard: the data regression above executes the
    // exact production SQL, while these narrow slices ensure `status()`
    // cannot silently return to the broader lifecycle projection.
    let source = production_source();
    let status = method_body(&source, "pub async fn status(&self)");
    assert!(status.contains("protocol_status_for_committed_members(&members_json)"));
    assert!(!status.contains("self.protocol_status().await"));

    let committed_protocol =
        method_body(&source, "async fn protocol_status_for_committed_members(");
    assert!(committed_protocol
        .contains("committed_unready_nodes(LEARNER_PROTOCOL_CAPABILITY, members_json)"));

    let committed_unready = method_body(&source, "async fn committed_unready_nodes(");
    assert!(committed_unready.contains("committed_capability_unready_nodes_sql(capability)"));
    assert!(committed_unready.contains("params!(members_json)"));
}

/// Actual Hiqlite learner + actual signed response, not a clock sample or
/// a manufactured applied-membership watch. This is route authorization.
#[test]
fn k06_aged_committed_learner_clock_response_keeps_other_routes_live_only() {
    // The full import/real learner futures exceed default libtest stack.
    // Own a finite stack explicitly; ordinary suites need no global env.
    let worker = std::thread::Builder::new()
        .name("k06-aged-clock-response".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("owned fixture runtime")
                .block_on(Box::pin(aged_committed_learner_clock_response_fixture()));
        })
        .expect("owned fixture thread");
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
}

async fn aged_committed_learner_clock_response_fixture() {
    use crate::cluster::migration::{select_daemon_store_observing, StartupClockObserver};
    use crate::config::Config;
    use std::borrow::Cow;

    struct SingletonObserver;
    impl StartupClockObserver for SingletonObserver {
        fn start(
            &self,
            manager: MembershipManager,
            _node: String,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), crate::error::StoreError>> + Send + '_>>
        {
            Box::pin(async move {
                let roster = manager
                    .clock_peers()
                    .await
                    .map_err(|error| crate::error::StoreError::Database(error.to_string()))?;
                let applied = roster.membership.as_ref().expect("actual singleton watch");
                assert_eq!(applied.members, BTreeSet::from([applied.local_node]));
                assert!(roster.peers.is_empty());
                let guard = manager.clock_guard();
                let round = guard
                    .roster_for_peer_directory(&roster)
                    .expect("proved singleton");
                assert!(
                    guard.publish(round, BTreeMap::new()),
                    "no remote samples exist"
                );
                Ok(())
            })
        }
    }
    fn addresses() -> Vec<std::net::SocketAddr> {
        let held: Vec<_> = (0..3)
            .map(|_| {
                std::net::TcpListener::bind("127.0.0.1:0").expect("reserve owned fixture port")
            })
            .collect();
        held.iter()
            .map(|listener| listener.local_addr().expect("owned port"))
            .collect()
    }
    let root = tempfile::tempdir().expect("leader root");
    let ports = addresses();
    let mut config = Config::default();
    config.storage.data_dir = root.path().into();
    config.server.bind = ports[0];
    config.cluster.raft_bind = ports[1];
    config.cluster.api_bind = ports[2];
    config.cluster.advertise_host = "localhost".into();
    config.cluster.join_url = format!("http://{}", ports[0]);
    config.cluster.artwork_url = config.cluster.join_url.clone();
    drop(crate::store::SqliteStore::open(&root.path().join("plurx.db")).expect("source schema"));
    let selected = Box::pin(select_daemon_store_observing(
        &config,
        Some(&SingletonObserver),
    ))
    .await
    .expect("actual singleton activation");
    let manager = selected.membership_manager();
    let token = manager
        .issue_token(Duration::from_secs(120))
        .await
        .expect("owned join token");
    let learner_root = tempfile::tempdir().expect("learner root");
    let learner_ports = addresses();
    let learner_id = uuid::Uuid::new_v4().to_string();
    let (protocol_min, protocol_max) = manager.active_protocol_range().await.expect("actual range");
    manager
        .redeem(&RedeemJoinRequest {
            token_digest: join_token_digest(&token.token),
            raft_id: token.raft_id,
            node_id: learner_id.clone(),
            hostname: "clock-fixture".into(),
            raft_address: learner_ports[1].to_string(),
            api_address: learner_ports[2].to_string(),
            http_base: format!("http://{}", learner_ports[0]),
            schema_version: AUTH_SCHEMA_VERSION,
            protocol_version: protocol_min,
            protocol_min,
            protocol_max,
            live_tv_v1: true,
            sharing: SharingJoinCapabilities::for_current_binary(),
        })
        .await
        .expect("actual token-authorized staged identity");
    let leader = manager.replicated_inner().expect("actual leader");
    let local = ClusterPeer {
        raft_id: token.raft_id,
        raft_address: learner_ports[1].to_string(),
        api_address: learner_ports[2].to_string(),
    };
    let policy = Arc::new(StartupMembershipAdmission::default());
    // Startup returning does not imply its passive local watch has published
    // the committed learner entry yet. Keep startup and that readiness wait
    // inside the same original budget rather than retrying manager creation.
    let learner_deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    let learner_client = Box::pin(tokio::time::timeout_at(
        learner_deadline,
        Box::pin(hiqlite::start_node_for_clock_observation(
            hiqlite::NodeConfig {
                node_id: token.raft_id,
                nodes: vec![Node::from(&leader.local), Node::from(&local)],
                listen_addr_api: Cow::Borrowed("127.0.0.1"),
                listen_addr_raft: Cow::Borrowed("127.0.0.1"),
                data_dir: Cow::Owned(learner_root.path().to_string_lossy().into_owned()),
                filename_db: Cow::Borrowed("clock-test.db"),
                secret_raft: leader.secrets.raft.clone(),
                secret_api: leader.secrets.api.clone(),
                tls_raft: Some(hiqlite::tls::ServerTlsConfig::TlsAutoCertificates),
                tls_api: Some(hiqlite::tls::ServerTlsConfig::TlsAutoCertificates),
                learner_only: true,
                health_check_delay_secs: 0,
                ..crate::cluster::migration::production_hiqlite_defaults_with_read_pool(1)
            },
            policy,
        )),
    ))
    .await
    .expect("bounded learner startup")
    .expect("actual learner");
    let learner_metrics = learner_client
        .local_db_raft_metrics()
        .expect("actual learner local watch");
    let leader_metrics = leader
        .client
        .local_db_raft_metrics()
        .expect("actual leader local watch");
    let ready = tokio::time::timeout_at(learner_deadline, async {
        loop {
            // Use the production guard's exact applied-watch conversion:
            // running, committed, a real log identity and local membership.
            // Both actual nodes must agree before the single constructor and
            // subsequent leader-side clock authorization use that identity.
            if let (Some(learner_applied), Some(leader_applied)) =
                (learner_metrics.current(), leader_metrics.current())
            {
                if learner_applied.local_node == token.raft_id
                    && leader_applied.local_node == leader.local.raft_id
                    && learner_applied.members.contains(&token.raft_id)
                    && learner_applied.members.contains(&leader.local.raft_id)
                    && !learner_applied.voters.contains(&token.raft_id)
                    && learner_applied.voters.contains(&leader.local.raft_id)
                    && learner_applied.log == leader_applied.log
                    && learner_applied.members == leader_applied.members
                    && learner_applied.voters == leader_applied.voters
                {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        ready.is_ok(),
        "committed learner readiness exceeded original startup deadline: leader={:?}, learner={:?}",
        leader_metrics.membership_snapshot(),
        learner_metrics.membership_snapshot()
    );
    let mut identity = selected.identity.clone();
    identity.node_id.clone_from(&learner_id);
    identity.raft_id = token.raft_id;
    let learner = Box::pin(MembershipManager::clock_observation(
        learner_client.clone(),
        selected.replication_monitor(),
        Arc::clone(&selected.store),
        identity,
        local,
        config.cluster.join_url.clone(),
        format!("http://{}", learner_ports[0]),
        JoinSecrets {
            raft: leader.secrets.raft.clone(),
            api: leader.secrets.api.clone(),
            credential_key: leader.secrets.credential_key.clone(),
        },
        ActivitySigningKey::from_seed_hex(&"12".repeat(32)).expect("fixture durable key"),
        leader.activation_marker.clone(),
        ClusterRole::Voter,
        learner_root.path().into(),
    ))
    .await
    .expect("actual committed learner observation manager");
    let applied = learner_client
        .local_db_raft_metrics()
        .expect("actual local watch")
        .membership_snapshot();
    assert!(applied.committed && applied.members.contains(&token.raft_id));
    assert!(
        !applied.voters.contains(&token.raft_id),
        "no promotion/fabricated admission"
    );
    let aged = unix_ms().expect("now") - NODE_REACHABLE_WINDOW_MS - 1_000;
    assert_eq!(
        leader
            .client
            .execute(
                "UPDATE cluster_nodes SET last_seen_at=$1 WHERE node_id=$2",
                params!(aged, learner_id.as_str())
            )
            .await
            .expect("age actual committed identity"),
        1
    );
    let nonce = uuid::Uuid::new_v4().to_string();
    let body = b"actual signed fixture response, not a measured clock sample";
    let clock_path = "/_internal/v1/clock";
    let target = selected.identity.node_id.as_str();
    let signature = learner
        .sign_internal_peer_response(target, &nonce, clock_path, body)
        .expect("signed clock response");
    assert!(manager
        .authorize_internal_peer_member_response(
            &learner_id,
            target,
            &nonce,
            clock_path,
            body,
            &signature
        )
        .await
        .expect("clock authorization"));
    assert!(!manager
        .authorize_internal_peer_member_response(
            &learner_id,
            "wrong-target",
            &nonce,
            clock_path,
            body,
            &signature
        )
        .await
        .expect("wrong-target refusal"));
    assert!(!manager
        .authorize_internal_peer_member_response(
            &learner_id,
            target,
            &nonce,
            clock_path,
            b"forged body",
            &signature
        )
        .await
        .expect("forged response refusal"));
    let ordinary_path = "/api/v1/internal/auth/cache-revocation";
    let ordinary_signature = learner
        .sign_internal_peer_response(target, &nonce, ordinary_path, body)
        .expect("signed ordinary response");
    assert!(!manager
        .authorize_internal_peer_member_response(
            &learner_id,
            target,
            &nonce,
            ordinary_path,
            body,
            &ordinary_signature
        )
        .await
        .expect("ordinary aged member refusal"));
    leader
        .client
        .execute(
            "UPDATE cluster_nodes SET last_seen_at=$1 WHERE node_id=$2",
            params!(unix_ms().expect("current heartbeat"), learner_id.as_str()),
        )
        .await
        .expect("fresh actual identity");
    assert!(manager
        .authorize_internal_peer_member_response(
            &learner_id,
            target,
            &nonce,
            ordinary_path,
            body,
            &ordinary_signature
        )
        .await
        .expect("ordinary live member acceptance"));
    leader
        .client
        .execute(
            "UPDATE cluster_nodes SET removed_at=$1 WHERE node_id=$2",
            params!(unix_ms().expect("actual removal time"), learner_id.as_str()),
        )
        .await
        .expect("actual durable removal");
    assert!(!manager
        .authorize_internal_peer_member_response(
            &learner_id,
            target,
            &nonce,
            clock_path,
            body,
            &signature
        )
        .await
        .expect("removed peer refusal"));
    // Existing Hiqlite TLS shutdown assertion occurs only after writer drain.
    let stopped = tokio::spawn(async move { learner_client.shutdown().await }).await;
    match stopped {
        Ok(result) => result.expect("learner drain"),
        Err(error) if error.is_panic() => {
            let payload = error.into_panic();
            let text = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied());
            assert_eq!(
                text,
                Some("The global Hiqlite shutdown handler to always listen: SendError { .. }")
            );
        }
        Err(error) => panic!("learner shutdown task: {error}"),
    }
    selected.shutdown().await.expect("leader drain");
}

#[test]
fn committed_roster_bound_fails_closed_instead_of_truncating() {
    let members = (0_u64..=MAX_COMMITTED_ROSTER_MEMBERS as u64).collect::<BTreeSet<_>>();
    let error = bounded_committed_raft_ids_json(&members)
        .expect_err("an oversized committed roster must not be truncated");
    assert!(error.to_string().contains("bounded operations limit"));
}

#[test]
fn internal_peer_response_signature_binds_each_exchange_field_and_domain() {
    let key = ActivitySigningKey::from_seed_hex(&"42".repeat(32)).expect("fixture signing key");
    let nonce = uuid::Uuid::new_v4().to_string();
    let other_nonce = uuid::Uuid::new_v4().to_string();
    let path = "/_internal/v1/live-tv/start-state";
    let payload = br#"["owner","controller","nonce",7,2]"#;
    let message = internal_peer_response_message("owner", "controller", &nonce, path, payload)
        .expect("message");
    let signature = hex::decode(key.sign_hex(&message)).expect("signature");
    let public_key = hex::decode(key.public_key_hex()).expect("public key");
    let verifier = UnparsedPublicKey::new(&ED25519, &public_key);
    assert!(verifier.verify(&message, &signature).is_ok());
    for changed in [
        internal_peer_response_message("other-owner", "controller", &nonce, path, payload),
        internal_peer_response_message("owner", "other-controller", &nonce, path, payload),
        internal_peer_response_message("owner", "controller", &other_nonce, path, payload),
        internal_peer_response_message("owner", "controller", &nonce, "/other", payload),
        internal_peer_response_message("owner", "controller", &nonce, path, b"changed payload"),
        internal_peer_auth_message("owner", "controller", 1, &nonce, "POST", path, payload),
    ] {
        assert!(verifier
            .verify(&changed.expect("valid alternate message"), &signature)
            .is_err());
    }
    let mut tampered = signature;
    tampered[0] ^= 1;
    assert!(verifier.verify(&message, &tampered).is_err());
}

fn install_live_tv_membership_fixture(connection: &rusqlite::Connection) {
    connection
            .execute_batch(&format!(
                "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL); \
                 CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, role TEXT, removed_at INTEGER, last_seen_at INTEGER NOT NULL); \
                 CREATE TABLE cluster_node_capabilities (node_id TEXT NOT NULL, capability TEXT NOT NULL, last_seen_at INTEGER NOT NULL, PRIMARY KEY(node_id, capability)); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_maintenance (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_join_tokens (token_hash TEXT PRIMARY KEY, state TEXT NOT NULL, node_id TEXT); \
                 CREATE TABLE cluster_live_tv_join_intents (token_hash TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_live_tv_activation (singleton INTEGER PRIMARY KEY CHECK (singleton = 1), enabled INTEGER NOT NULL); \
                 {MIRROR_LIVE_TV_ACTIVATION_ON_INSERT_SQL}; \
                 {MIRROR_LIVE_TV_ACTIVATION_ON_UPDATE_SQL}; \
                 {MIRROR_LIVE_TV_ACTIVATION_ON_DELETE_SQL}; \
                 {REQUIRE_LIVE_TV_JOIN_INTENT_ON_RESERVATION_SQL}; \
                 {REQUIRE_LIVE_TV_JOIN_INTENT_ON_STAGING_SQL}; \
                 INSERT INTO cluster_nodes (node_id, role, removed_at, last_seen_at) VALUES ('owner', NULL, NULL, 1);"
            ))
            .expect("live-TV membership fixture");
    connection
        .execute(
            "INSERT INTO cluster_node_capabilities VALUES ('owner', $1, 1)",
            rusqlite::params![LIVE_TV_CAPABILITY],
        )
        .expect("owner capability");
}

fn commit_live_tv_activation(
    connection: &mut rusqlite::Connection,
    session_limit: &str,
    absence_cutoff: i64,
) -> bool {
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .expect("activation transaction");
    let sql = guarded_live_tv_setting_sql();
    let mut changed = Vec::new();
    for (key, value) in [
        (crate::store::keys::LIVE_TV_ENABLED, "1"),
        (crate::store::keys::LIVE_TV_MAX_SESSIONS, session_limit),
        (crate::store::keys::LIVE_TV_CONFIG_GENERATION, "1"),
    ] {
        changed.push(
            transaction
                .execute(
                    &sql,
                    rusqlite::params![
                        key,
                        value,
                        1_i64,
                        crate::store::keys::LIVE_TV_CONFIG_GENERATION,
                        0_i64,
                        "owner",
                        absence_cutoff
                    ],
                )
                .expect("guarded activation write"),
        );
    }
    transaction.commit().expect("activation commit");
    changed.into_iter().all(|count| count == 1)
}

// A learner refused every internal peer request — its own operations-status
// answer to the cluster panel, and every media-session control request it
// originated — because the shared authority check required a committed
// VOTER at both ends. plurxd's `learner_route_eligible` publishes exactly
// that surface to learners, and `operations_peers`/`media_peers` name
// learners in both directions, so the predicate refused the traffic those
// directories exist to carry. The 401 then reached the operator as
// "unreachable" on a node whose heartbeat was fresh.
//
// `docs/cluster/MEMBERSHIP-CREDENTIAL-SPLIT-PLAN.md` §2 already specified the
// member-scoped predicate here and gave the voter predicate to membership
// mutation alone; the code was stricter than its own design.

#[test]
fn a_committed_learner_is_admitted_as_a_member_and_refused_as_a_voter() {
    let voters = [1_u64, 2, 3];
    let members = [1_u64, 2, 3, 7];
    let admits = |role, raft_id| {
        peer_authority_admits(role, raft_id, voters.into_iter(), members.into_iter())
    };

    // The learner: the whole point of the fix.
    assert!(admits(PeerAuthorityRole::CommittedMember, 7));
    assert!(!admits(PeerAuthorityRole::CommittedVoter, 7));

    // A voter is admitted under both, so nothing that worked stops working.
    for raft_id in voters {
        assert!(admits(PeerAuthorityRole::CommittedMember, raft_id));
        assert!(admits(PeerAuthorityRole::CommittedVoter, raft_id));
    }

    // A node in neither set — removed, or never committed — is refused
    // under both. Widening to members is not widening to strangers.
    assert!(!admits(PeerAuthorityRole::CommittedMember, 9));
    assert!(!admits(PeerAuthorityRole::CommittedVoter, 9));
}

#[test]
fn an_empty_committed_roster_admits_nobody_under_either_role() {
    let none: [u64; 0] = [];
    for role in [
        PeerAuthorityRole::CommittedVoter,
        PeerAuthorityRole::CommittedMember,
    ] {
        assert!(!peer_authority_admits(
            role,
            1,
            none.into_iter(),
            none.into_iter()
        ));
    }
}

// The predicate above is only correct where it is actually used, and a
// helper test leaves the call sites free: the defect was one role constant
// in three places. Pin each authorizer to the role it must ask for.
#[test]
fn each_authorizer_asks_for_the_role_its_peer_directory_uses() {
    let source = include_str!("../membership.rs");
    let activity = source
        .split_once("pub async fn authorize_activity_request(")
        .expect("authorize_activity_request was renamed")
        .1
        .split_once("\n    }\n")
        .expect("authorize_activity_request never closes at fn indent")
        .0;
    assert!(
        activity.contains("PeerAuthorityRole::CommittedVoter"),
        "activity aggregation is voter-only: activity_peers never names a learner",
    );

    for authorizer in [
        "pub async fn authorize_internal_peer_request(",
        "pub async fn authorize_internal_peer_read_request(",
    ] {
        let body = source
            .split_once(authorizer)
            .unwrap_or_else(|| panic!("{authorizer} was renamed"))
            .1
            .split_once("\n    }\n")
            .unwrap_or_else(|| panic!("{authorizer} never closes at fn indent"))
            .0;
        assert!(
            body.contains("PeerAuthorityRole::CommittedMember"),
            "{authorizer} must admit committed learners: plurxd publishes this \
                 surface to them and both media directories name them",
        );
        assert!(
            !body.contains("PeerAuthorityRole::CommittedVoter"),
            "{authorizer} must not reinstate the voter predicate",
        );
    }
    let voter = source
        .split_once("pub async fn authorize_internal_peer_voter_request(")
        .expect("voter authorizer was renamed")
        .1
        .split_once("\n    }\n")
        .expect("voter authorizer never closes at fn indent")
        .0;
    assert!(voter.contains("PeerAuthorityRole::CommittedVoter"));
}

// The route gate answers 503 for `Fenced`, so the cost of an interim
// publication is a real outage window on a healthy node, once per
// heartbeat. These three pin the whole contract of the guard that makes
// an interim publication unwriteable.

#[test]
fn a_serving_role_publication_does_not_touch_the_slot_before_it_commits() {
    let slot = AtomicU8::new(LocalServingRole::Voter.encoded());
    let publication = LocalServingRolePublication::new(&slot);
    // The reads a refresh performs happen here. A node that is serving
    // must still be serving.
    assert_eq!(
        LocalServingRole::from_encoded(slot.load(Ordering::Acquire)),
        LocalServingRole::Voter,
    );
    publication.commit(LocalServingRole::Learner);
    assert_eq!(
        LocalServingRole::from_encoded(slot.load(Ordering::Acquire)),
        LocalServingRole::Learner,
    );
}

#[test]
fn an_uncommitted_serving_role_publication_fences_on_drop() {
    let slot = AtomicU8::new(LocalServingRole::Voter.encoded());
    {
        let _publication = LocalServingRolePublication::new(&slot);
        // An early `?`, a panic, or a cancelled future leaves here.
    }
    assert_eq!(
        LocalServingRole::from_encoded(slot.load(Ordering::Acquire)),
        LocalServingRole::Fenced,
        "a refresh that did not finish must leave the request path fenced",
    );
}

#[test]
fn a_committed_serving_role_publication_survives_its_own_drop() {
    let slot = AtomicU8::new(LocalServingRole::Fenced.encoded());
    LocalServingRolePublication::new(&slot).commit(LocalServingRole::Voter);
    assert_eq!(
        LocalServingRole::from_encoded(slot.load(Ordering::Acquire)),
        LocalServingRole::Voter,
        "the drop that follows a commit must not re-fence the node",
    );
}

#[test]
fn a_maintenance_publication_does_not_close_the_gate_before_it_commits() {
    let slot = AtomicBool::new(false);
    let publication = LocalMaintenancePublication::new(&slot);
    assert!(
        !slot.load(Ordering::Acquire),
        "a node nobody asked to drain must keep serving while the read runs",
    );
    publication.commit(false);
    assert!(!slot.load(Ordering::Acquire));
}

#[test]
fn an_uncommitted_maintenance_publication_holds_the_node_in_maintenance() {
    let slot = AtomicBool::new(false);
    {
        let _publication = LocalMaintenancePublication::new(&slot);
    }
    assert!(
        slot.load(Ordering::Acquire),
        "a refresh that did not finish must leave the node in maintenance",
    );
}

#[test]
fn a_committed_maintenance_publication_survives_its_own_drop() {
    let slot = AtomicBool::new(true);
    LocalMaintenancePublication::new(&slot).commit(false);
    assert!(
        !slot.load(Ordering::Acquire),
        "the drop that follows a commit must not re-close the gate",
    );
}

fn guarded_operation_lease_fixture() -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().expect("sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, removed_at INTEGER); \
                 INSERT INTO cluster_nodes VALUES ('node-a', NULL); \
                 CREATE TABLE cluster_node_maintenance (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_removal_attempts (node_id TEXT, attempt_id TEXT); \
                 CREATE TABLE cluster_node_promotions (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_operation_leases (\
                   singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
                   node_id TEXT NOT NULL, operation TEXT NOT NULL, \
                   claim_id TEXT NOT NULL UNIQUE, expires_at INTEGER NOT NULL); \
                 CREATE TABLE cluster_operation_lease_releases (claim_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_operation_lease_intents (claim_id TEXT PRIMARY KEY);",
        )
        .expect("guarded operation lease schema");
    for trigger in [
        REQUIRE_OPERATION_LEASE_INSERT_INTENT_SQL,
        REQUIRE_OPERATION_LEASE_UPDATE_INTENT_SQL,
        REQUIRE_OPERATION_LEASE_DELETE_INTENT_SQL,
    ] {
        connection
            .execute_batch(trigger)
            .expect("install current-coordinator lease guard");
    }
    connection
}

fn guarded_cache_admin_lease_fixture() -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().expect("sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, last_seen_at INTEGER NOT NULL, removed_at INTEGER); \
                 INSERT INTO cluster_nodes VALUES \
                   ('node-a', 100, NULL), ('node-b', 100, NULL); \
                 CREATE TABLE cluster_node_maintenance (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_removal_attempts (node_id TEXT, attempt_id TEXT); \
                 CREATE TABLE cluster_node_promotions (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_join_tokens (token_hash TEXT PRIMARY KEY, state TEXT); \
                 INSERT INTO cluster_join_tokens VALUES ('joining-node', 'issued'); \
                 CREATE TABLE cluster_operation_leases (\
                   singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
                   node_id TEXT NOT NULL, operation TEXT NOT NULL, \
                   claim_id TEXT NOT NULL UNIQUE, expires_at INTEGER NOT NULL); \
                 CREATE TABLE cluster_cache_admin_revocation_leases (\
                   singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
                   node_id TEXT NOT NULL, claim_id TEXT NOT NULL UNIQUE, \
                   expires_at INTEGER NOT NULL, \
                   retain_release_receipt INTEGER NOT NULL \
                     CHECK (retain_release_receipt IN (0, 1))); \
                 CREATE TABLE cluster_cache_admin_revocation_lease_releases (\
                   claim_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_cache_admin_revocation_release_watermark (\
                   singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
                   expires_through INTEGER NOT NULL); \
                 CREATE TABLE cluster_cache_admin_revocation_lease_intents (\
                   claim_id TEXT PRIMARY KEY); \
                 CREATE TABLE users (id INTEGER PRIMARY KEY, password_hash TEXT NOT NULL); \
                 INSERT INTO users VALUES (1, 'old-hash');",
        )
        .expect("cache-admin exclusion schema");
    for trigger in [
        REQUIRE_CACHE_ADMIN_LEASE_INSERT_INTENT_SQL,
        REQUIRE_CACHE_ADMIN_LEASE_UPDATE_INTENT_SQL,
        REQUIRE_CACHE_ADMIN_LEASE_DELETE_INTENT_SQL,
        PROTECT_CACHE_ADMIN_LEASE_FROM_REMOVAL_SQL,
        PROTECT_CACHE_ADMIN_LEASE_FROM_PROMOTION_SQL,
        PROTECT_CACHE_ADMIN_LEASE_FROM_JOIN_RESERVATION_SQL,
        PROTECT_CACHE_ADMIN_LEASE_FROM_JOIN_STAGING_SQL,
        PROTECT_CACHE_ADMIN_LEASE_FROM_OPERATION_SQL,
        EXPIRE_CACHE_ADMIN_LEASE_FROM_HEARTBEAT_SQL,
    ] {
        connection
            .execute_batch(trigger)
            .expect("install cache-admin exclusion trigger");
    }
    connection
}

/// The refusal has to be able to explain itself, in the fleet's own state.
///
/// `cache_admin_exclusion_blockers` is runtime-built SQL read back by
/// column name, and hiqlite's row accessor *panics* on a missing column —
/// inside the activation loop's task, which would kill it outright and
/// restore the original defect permanently and with no retry. So run the
/// exact query, against the same fixture the acquire is tested on, and
/// require every standing condition of the acquire to have a column that
/// answers, and to flip when that condition is made true.
#[test]
fn cache_admin_exclusion_blockers_name_every_standing_condition() {
    let connection = guarded_cache_admin_lease_fixture();
    let sql = cache_admin_exclusion_blockers_sql()
        .replace("$1", "?1")
        .replace("$2", "?2");
    let read = |connection: &rusqlite::Connection| -> Vec<(String, i64)> {
        let mut statement = connection.prepare(&sql).expect("blockers query");
        let names = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let values = statement
            .query_row(rusqlite::params!["node-a", 1_000_i64], |row| {
                (0..names.len())
                    .map(|index| row.get::<_, i64>(index))
                    .collect::<Result<Vec<_>, _>>()
            })
            .expect("blockers row");
        names.into_iter().zip(values).collect()
    };

    let baseline = read(&connection);
    assert_eq!(
        baseline
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        CacheAdminBlockerRow::COLUMNS.to_vec(),
        "the query and the row reader must name the same columns, in the same order"
    );
    assert!(
        baseline.iter().all(|(_, value)| *value == 0),
        "a healthy fixture blocks nothing: {baseline:?}"
    );

    // Each condition, made true one at a time, must light exactly its own
    // column. A predicate wired to the wrong column would otherwise report
    // a plausible and wrong cause, which is worse than reporting none.
    for (column, statement) in [
        (
            "local_inactive",
            "UPDATE cluster_nodes SET removed_at = 5 WHERE node_id = 'node-a'",
        ),
        (
            "maintenance",
            "INSERT INTO cluster_node_maintenance VALUES ('node-b')",
        ),
        (
            "removal",
            "INSERT INTO cluster_node_removals VALUES ('node-b')",
        ),
        (
            "removal_attempt",
            "INSERT INTO cluster_node_removal_attempts VALUES ('node-b', 'attempt')",
        ),
        (
            "promotion",
            "INSERT INTO cluster_node_promotions VALUES ('node-b')",
        ),
        (
            "join_staging",
            "INSERT INTO cluster_node_join_staging VALUES ('node-b')",
        ),
        (
            "join_redeeming",
            "INSERT INTO cluster_join_tokens VALUES ('redeeming-token', 'redeeming')",
        ),
        (
            "operation_lease",
            "INSERT INTO cluster_operation_leases VALUES (1, 'node-b', 'maintenance', \
                   'lease-claim', 9999)",
        ),
        (
            "held_elsewhere",
            "INSERT INTO cluster_cache_admin_revocation_lease_intents VALUES \
                   ('other-claim'); \
                 INSERT INTO cluster_cache_admin_revocation_leases \
                   VALUES (1, 'node-b', 'other-claim', 9999, 0)",
        ),
        (
            "release_watermark",
            "INSERT INTO cluster_cache_admin_revocation_release_watermark VALUES (1, 5000)",
        ),
    ] {
        let scratch = guarded_cache_admin_lease_fixture();
        scratch.execute_batch(statement).expect(column);
        let lit = read(&scratch)
            .into_iter()
            .filter(|(_, value)| *value != 0)
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(
            lit,
            vec![column.to_owned()],
            "{column} did not light its own column alone"
        );
    }

    // The local hold is the same table as the remote one, told apart by
    // node id: reporting "another node holds it" for our own lease sends an
    // operator to the wrong machine.
    let scratch = guarded_cache_admin_lease_fixture();
    scratch
        .execute_batch(
            "INSERT INTO cluster_cache_admin_revocation_lease_intents VALUES ('own-claim'); \
                 INSERT INTO cluster_cache_admin_revocation_leases \
                   VALUES (1, 'node-a', 'own-claim', 9999, 0)",
        )
        .expect("local hold");
    let lit = read(&scratch)
        .into_iter()
        .filter(|(_, value)| *value != 0)
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    assert_eq!(lit, vec!["held_locally".to_owned()]);
}

/// The fleet defect, as two rows.
///
/// A completed removal leaves its `cluster_node_removals` and
/// `cluster_node_removal_attempts` rows behind on purpose — they are the
/// durable tombstone, and a trigger protects them. Both guarded
/// acquisitions used to read those tables table-wide, so the first node a
/// cluster ever removed disabled the cache-admin exclusion and the planned
/// outage lease permanently. Measured on a four-node fleet: two removals
/// in August, the one-time credential-guard activation retrying every
/// three seconds ever since, the Store-free admin proof cache fenced
/// closed on every node as a result, `/cluster/status` answering 401 to a
/// valid administrator, and the operator returned to the login page on
/// every visit to Settings.
///
/// A removal still in flight must keep blocking both. That is the half
/// that must not regress while fixing the other.
#[test]
fn a_completed_removal_stops_fencing_the_exclusions_it_finished_with() {
    let mut connection = guarded_cache_admin_lease_fixture();
    connection
        .execute_batch(
            "INSERT INTO cluster_nodes VALUES ('node-gone', 100, 900); \
                 INSERT INTO cluster_node_removals VALUES ('node-gone'); \
                 INSERT INTO cluster_node_removal_attempts VALUES ('node-gone', 'attempt-1');",
        )
        .expect("a removal that finished, tombstone and all");
    assert_eq!(
        acquire_guarded_cache_admin_lease(&mut connection, "claim-after-removal", false),
        1,
        "a tombstoned node's leftover fence is not a removal in progress"
    );
    let mut in_flight = guarded_cache_admin_lease_fixture();
    in_flight
        .execute_batch(
            "INSERT INTO cluster_nodes VALUES ('node-leaving', 100, NULL); \
                 INSERT INTO cluster_node_removals VALUES ('node-leaving'); \
                 INSERT INTO cluster_node_removal_attempts VALUES ('node-leaving', 'attempt-2');",
        )
        .expect("a removal still under way");
    assert_eq!(
        acquire_guarded_cache_admin_lease(&mut in_flight, "claim-during-removal", false),
        0,
        "a removal in flight still excludes the cache-admin claim"
    );

    // The planned-outage lease carries the identical predicate, which is
    // why maintenance entry, restart preparation and force election were
    // refused on that fleet too.
    let mut leases = guarded_operation_lease_fixture();
    leases
        .execute_batch(
            "INSERT INTO cluster_nodes VALUES ('node-gone', 900); \
                 INSERT INTO cluster_node_removals VALUES ('node-gone'); \
                 INSERT INTO cluster_node_removal_attempts VALUES ('node-gone', 'attempt-1');",
        )
        .expect("a removal that finished");
    assert_eq!(
        acquire_guarded_operation_lease(&mut leases, "node-a", "maintenance", "claim-1"),
        1,
        "a finished removal must not fence planned outages for the life of the cluster"
    );
    let mut fenced = guarded_operation_lease_fixture();
    fenced
        .execute_batch(
            "INSERT INTO cluster_nodes VALUES ('node-leaving', NULL); \
                 INSERT INTO cluster_node_removals VALUES ('node-leaving');",
        )
        .expect("a removal still under way");
    assert_eq!(
        acquire_guarded_operation_lease(&mut fenced, "node-a", "maintenance", "claim-2"),
        0,
        "a removal in flight still excludes a planned outage"
    );

    // And the admission that follows the lease. Taking the lease and then
    // failing here is the worse failure of the two: the operator is told
    // "maintenance and membership changes are mutually exclusive" while no
    // membership change is happening or ever will again.
    let admission = enter_maintenance_admission_sql()
        .replace("$1", "?1")
        .replace("$2", "?2")
        .replace("$3", "?3");
    // RETURNING makes this a query, not an execute: count the rows it
    // gives back, which is what the daemon reads to decide it was admitted.
    let admit = |connection: &rusqlite::Connection| -> usize {
        let mut statement = connection
            .prepare(&admission)
            .expect("maintenance admission");
        statement
            .query_map(rusqlite::params!["node-a", 100_i64, "claim-1"], |row| {
                row.get::<_, String>(0)
            })
            .expect("maintenance admission rows")
            .count()
    };
    let ready = maintenance_admission_fixture();
    assert_eq!(admit(&ready), 1, "a healthy cluster admits maintenance");
    let after_removal = maintenance_admission_fixture();
    after_removal
        .execute_batch(
            "INSERT INTO cluster_nodes VALUES ('node-gone', 100, 900); \
                 INSERT INTO cluster_node_removals VALUES ('node-gone'); \
                 INSERT INTO cluster_node_removal_attempts VALUES ('node-gone', 'attempt-1');",
        )
        .expect("a removal that finished");
    assert_eq!(
        admit(&after_removal),
        1,
        "a finished removal must not refuse maintenance for the life of the cluster"
    );
    let during_removal = maintenance_admission_fixture();
    during_removal
        .execute_batch(
            "INSERT INTO cluster_nodes VALUES ('node-leaving', 100, NULL); \
                 INSERT INTO cluster_node_removals VALUES ('node-leaving');",
        )
        .expect("a removal still under way");
    assert_eq!(
        admit(&during_removal),
        0,
        "a removal in flight still refuses maintenance"
    );
    // And a fence whose target this cluster cannot resolve at all stays a
    // fence: an orphaned removal row is not evidence that it finished.
    let orphaned = maintenance_admission_fixture();
    orphaned
        .execute_batch("INSERT INTO cluster_node_removals VALUES ('node-vanished');")
        .expect("an orphaned removal fence");
    assert_eq!(
        admit(&orphaned),
        0,
        "an unresolvable removal target must fail closed, not open"
    );
}

/// A cluster that can admit `node-a` into maintenance: it holds the
/// maintenance lease, proves the capability with a current heartbeat, and
/// nothing else is in flight.
fn maintenance_admission_fixture() -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().expect("sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, last_seen_at INTEGER NOT NULL, removed_at INTEGER); \
                 INSERT INTO cluster_nodes VALUES ('node-a', 100, NULL); \
                 CREATE TABLE cluster_node_capabilities (\
                   node_id TEXT, capability TEXT, last_seen_at INTEGER, \
                   PRIMARY KEY (node_id, capability)); \
                 CREATE TABLE cluster_node_maintenance (\
                   node_id TEXT PRIMARY KEY, requested_at INTEGER, acknowledged_at INTEGER); \
                 CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_removal_attempts (node_id TEXT, attempt_id TEXT); \
                 CREATE TABLE cluster_node_promotions (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_operation_leases (\
                   singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
                   node_id TEXT NOT NULL, operation TEXT NOT NULL, \
                   claim_id TEXT NOT NULL, expires_at INTEGER NOT NULL); \
                 INSERT INTO cluster_operation_leases \
                   VALUES (1, 'node-a', 'maintenance', 'claim-1', 9999);",
        )
        .expect("maintenance admission schema");
    connection
        .execute(
            "INSERT INTO cluster_node_capabilities VALUES ('node-a', ?1, 100)",
            rusqlite::params![NODE_MAINTENANCE_CAPABILITY],
        )
        .expect("current maintenance capability");
    connection
}

fn acquire_guarded_cache_admin_lease(
    connection: &mut rusqlite::Connection,
    claim_id: &str,
    retain_release_receipt: bool,
) -> usize {
    acquire_guarded_cache_admin_lease_until(connection, claim_id, retain_release_receipt, 1_000)
}

fn acquire_guarded_cache_admin_lease_until(
    connection: &mut rusqlite::Connection,
    claim_id: &str,
    retain_release_receipt: bool,
    expires_at: i64,
) -> usize {
    let transaction = connection.transaction().expect("acquire transaction");
    transaction
        .execute(
            "INSERT INTO cluster_cache_admin_revocation_lease_intents (claim_id) \
                 VALUES ($1) ON CONFLICT(claim_id) DO NOTHING",
            rusqlite::params![claim_id],
        )
        .expect("authorize cache-admin acquisition");
    let changed = transaction
        .execute(
            &acquire_cache_admin_revocation_lease_sql(),
            rusqlite::params![
                "node-a",
                claim_id,
                expires_at,
                if retain_release_receipt { 1_i64 } else { 0_i64 }
            ],
        )
        .expect("acquire cache-admin exclusion");
    transaction
        .execute(
            "DELETE FROM cluster_cache_admin_revocation_lease_intents \
                 WHERE claim_id = $1",
            rusqlite::params![claim_id],
        )
        .expect("consume cache-admin acquisition authorization");
    transaction.commit().expect("commit cache-admin exclusion");
    changed
}

fn release_guarded_cache_admin_lease(
    connection: &mut rusqlite::Connection,
    claim_id: &str,
    retain_release_receipt: bool,
    expires_at: i64,
) {
    let transaction = connection.transaction().expect("release transaction");
    transaction
        .execute(
            "INSERT INTO cluster_cache_admin_revocation_lease_intents (claim_id) \
                 VALUES ($1) ON CONFLICT(claim_id) DO NOTHING",
            rusqlite::params![claim_id],
        )
        .expect("authorize cache-admin release");
    if retain_release_receipt {
        transaction
            .execute(
                ADVANCE_CACHE_ADMIN_REVOCATION_RELEASE_WATERMARK_SQL,
                rusqlite::params![expires_at],
            )
            .expect("advance cache-admin release watermark");
    }
    transaction
        .execute(
            RELEASE_CACHE_ADMIN_REVOCATION_LEASE_SQL,
            rusqlite::params!["node-a", claim_id],
        )
        .expect("release exact cache-admin claim");
    transaction
        .execute(
            "DELETE FROM cluster_cache_admin_revocation_lease_intents \
                 WHERE claim_id = $1",
            rusqlite::params![claim_id],
        )
        .expect("consume cache-admin release authorization");
    if !retain_release_receipt {
        transaction
            .execute(
                "DELETE FROM cluster_cache_admin_revocation_lease_releases \
                     WHERE claim_id = $1",
                rusqlite::params![claim_id],
            )
            .expect("discard definitive-acquisition receipt");
    }
    transaction.commit().expect("commit cache-admin release");
}

fn mark_guarded_cache_admin_acquire_definitive(
    connection: &mut rusqlite::Connection,
    claim_id: &str,
) {
    let transaction = connection.transaction().expect("definitive transaction");
    transaction
        .execute(
            "INSERT INTO cluster_cache_admin_revocation_lease_intents (claim_id) \
                 VALUES ($1) ON CONFLICT(claim_id) DO NOTHING",
            rusqlite::params![claim_id],
        )
        .expect("authorize definitive-acquire marker");
    assert_eq!(
        transaction
            .execute(
                MARK_CACHE_ADMIN_REVOCATION_ACQUIRE_DEFINITIVE_SQL,
                rusqlite::params!["node-a", claim_id],
            )
            .expect("mark cache-admin acquire definitive"),
        1
    );
    transaction
        .execute(
            "DELETE FROM cluster_cache_admin_revocation_lease_intents \
                 WHERE claim_id = $1",
            rusqlite::params![claim_id],
        )
        .expect("consume definitive-acquire authorization");
    transaction.commit().expect("commit definitive marker");
}

fn acquire_guarded_operation_lease(
    connection: &mut rusqlite::Connection,
    node_id: &str,
    operation: &str,
    claim_id: &str,
) -> usize {
    let transaction = connection.transaction().expect("acquire transaction");
    transaction
        .execute(
            "INSERT INTO cluster_operation_lease_intents (claim_id) VALUES ($1) \
                 ON CONFLICT(claim_id) DO NOTHING",
            rusqlite::params![claim_id],
        )
        .expect("authorize acquisition");
    let changed = transaction
        .execute(
            &acquire_cluster_operation_lease_sql(),
            rusqlite::params![node_id, operation, claim_id, 1_000_i64, 100_i64],
        )
        .expect("acquire guarded planned-outage claim");
    transaction
        .execute(
            "DELETE FROM cluster_operation_lease_intents WHERE claim_id = $1",
            rusqlite::params![claim_id],
        )
        .expect("consume acquisition authorization");
    transaction.commit().expect("commit acquisition");
    changed
}

fn release_guarded_operation_lease(
    connection: &mut rusqlite::Connection,
    node_id: &str,
    operation: &str,
    claim_id: &str,
) {
    let transaction = connection.transaction().expect("release transaction");
    transaction
        .execute(
            "INSERT INTO cluster_operation_lease_intents (claim_id) VALUES ($1) \
                 ON CONFLICT(claim_id) DO NOTHING",
            rusqlite::params![claim_id],
        )
        .expect("authorize exact release");
    transaction
        .execute(
            TOMBSTONE_CLUSTER_OPERATION_LEASE_SQL,
            rusqlite::params![claim_id],
        )
        .expect("record permanent exact-claim release");
    transaction
        .execute(
            RELEASE_CLUSTER_OPERATION_LEASE_SQL,
            rusqlite::params![node_id, operation, claim_id],
        )
        .expect("delete exact claim");
    transaction
        .execute(
            "DELETE FROM cluster_operation_lease_intents WHERE claim_id = $1",
            rusqlite::params![claim_id],
        )
        .expect("consume release authorization");
    transaction.commit().expect("commit release");
}

fn commit_guarded_maintenance(
    connection: &mut rusqlite::Connection,
    node_id: &str,
    claim_id: &str,
) -> bool {
    let transaction = connection.transaction().expect("maintenance transaction");
    let inserted = transaction
        .execute(
            "INSERT INTO cluster_node_maintenance (node_id) \
                 SELECT $1 WHERE EXISTS (SELECT 1 FROM cluster_operation_leases \
                   WHERE singleton = 1 AND node_id = $1 \
                     AND operation = 'maintenance' AND claim_id = $2) \
                 ON CONFLICT(node_id) DO NOTHING",
            rusqlite::params![node_id, claim_id],
        )
        .expect("conditionally commit maintenance");
    if inserted == 0 {
        transaction
            .rollback()
            .expect("rollback rejected maintenance");
        return false;
    }
    transaction
        .execute(
            "INSERT INTO cluster_operation_lease_intents (claim_id) VALUES ($1) \
                 ON CONFLICT(claim_id) DO NOTHING",
            rusqlite::params![claim_id],
        )
        .expect("authorize maintenance lease consumption");
    transaction
        .execute(
            TOMBSTONE_CLUSTER_OPERATION_LEASE_SQL,
            rusqlite::params![claim_id],
        )
        .expect("record maintenance claim consumption");
    transaction
        .execute(
            RELEASE_CLUSTER_OPERATION_LEASE_SQL,
            rusqlite::params![node_id, "maintenance", claim_id],
        )
        .expect("consume maintenance claim");
    transaction
        .execute(
            "DELETE FROM cluster_operation_lease_intents WHERE claim_id = $1",
            rusqlite::params![claim_id],
        )
        .expect("consume maintenance authorization");
    transaction.commit().expect("commit maintenance");
    true
}

#[test]
fn membership_cannot_commit_after_final_roster_read_before_credential_store_write() {
    let mut connection = guarded_cache_admin_lease_fixture();
    let claim_id = "after-final-roster-read";
    assert_eq!(
        acquire_guarded_cache_admin_lease(&mut connection, claim_id, true),
        1
    );

    // This is the final committed-roster read made after all current peers
    // acknowledged Begin. The exclusion was committed first and remains
    // live across the subsequent Store mutation.
    let roster = connection
        .prepare("SELECT node_id FROM cluster_nodes ORDER BY node_id")
        .expect("prepare final roster")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("read final roster")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect final roster");
    assert_eq!(roster, ["node-a", "node-b"]);

    // A join that linearizes after that read but before the credential
    // statement cannot reserve admission. The replicated trigger also
    // protects a previous-release coordinator that lacks the new WHERE
    // predicate entirely.
    assert!(connection
        .execute(
            "UPDATE cluster_join_tokens SET state = 'redeeming' \
                 WHERE token_hash = 'joining-node' AND state = 'issued'",
            [],
        )
        .is_err());

    assert_eq!(
        connection
            .execute(
                "UPDATE users SET password_hash = 'new-hash' \
                     WHERE id = 1 AND EXISTS (\
                       SELECT 1 FROM cluster_cache_admin_revocation_leases \
                       WHERE claim_id = $1)",
                rusqlite::params![claim_id],
            )
            .expect("credential mutation under exact exclusion"),
        1
    );

    release_guarded_cache_admin_lease(&mut connection, claim_id, true, 1_000);
    assert_eq!(
        acquire_guarded_cache_admin_lease(&mut connection, claim_id, true),
        0,
        "a delayed disconnected acquire cannot resurrect an exact released claim"
    );
    assert_eq!(
        connection
            .execute(
                "UPDATE users SET password_hash = 'too-late' \
                     WHERE id = 1 AND EXISTS (\
                       SELECT 1 FROM cluster_cache_admin_revocation_leases \
                       WHERE claim_id = $1)",
                rusqlite::params![claim_id],
            )
            .expect("delayed credential proposal is refused"),
        0,
        "an exact release must make a delayed Store proposal a no-op"
    );
    assert_eq!(
        connection
            .execute(
                "UPDATE cluster_join_tokens SET state = 'redeeming' \
                     WHERE token_hash = 'joining-node' AND state = 'issued'",
                [],
            )
            .expect("membership may resume after credential boundary"),
        1
    );
}

#[test]
fn definitive_cache_admin_acquire_release_cycles_leave_no_receipts() {
    let mut connection = guarded_cache_admin_lease_fixture();
    for attempt in 0..256 {
        let claim_id = format!("definitive-{attempt}");
        assert_eq!(
            acquire_guarded_cache_admin_lease(&mut connection, &claim_id, true),
            1
        );
        mark_guarded_cache_admin_acquire_definitive(&mut connection, &claim_id);
        release_guarded_cache_admin_lease(&mut connection, &claim_id, false, 1_000);
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_cache_admin_revocation_lease_releases",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count definitive release receipts"),
        0,
        "ordinary credential churn must not grow replicated receipts"
    );
}

#[test]
fn definitive_cache_admin_singleton_losers_leave_no_receipts() {
    assert!(!cache_admin_rejection_requires_release_receipt(false));
    let mut connection = guarded_cache_admin_lease_fixture();
    assert_eq!(
        acquire_guarded_cache_admin_lease(&mut connection, "owner", true),
        1
    );

    for attempt in 0..256 {
        let claim_id = format!("definite-loser-{attempt}");
        assert_eq!(
            acquire_guarded_cache_admin_lease(&mut connection, &claim_id, true),
            0,
            "the live singleton owner must reject every competing claim"
        );
        // A first-attempt, definite zero-row result has no delayed acquire
        // to suppress, so production disarms receipt retention before its
        // exact cleanup runs.
        release_guarded_cache_admin_lease(&mut connection, &claim_id, false, 1_000);
    }

    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_cache_admin_revocation_lease_releases",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count rejected-claim release receipts"),
        0,
        "definitive singleton contention must not grow replicated storage"
    );
}

#[test]
fn ambiguous_cache_admin_acquire_advances_watermark_and_cannot_resurrect() {
    let mut connection = guarded_cache_admin_lease_fixture();
    let claim_id = "ambiguous-acquire";
    assert_eq!(
        acquire_guarded_cache_admin_lease(&mut connection, claim_id, true),
        1
    );
    release_guarded_cache_admin_lease(&mut connection, claim_id, true, 1_000);
    assert_eq!(
        connection
            .query_row(
                "SELECT expires_through FROM cluster_cache_admin_revocation_release_watermark \
                     WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("ambiguous release watermark"),
        1_000
    );
    assert_eq!(
        acquire_guarded_cache_admin_lease(&mut connection, claim_id, true),
        0,
        "the delayed acquire must lose to the expiration watermark"
    );
    assert_eq!(
        acquire_guarded_cache_admin_lease_until(&mut connection, "newer-acquire", true, 1_001,),
        1,
        "the watermark must not refuse a newer bounded claim"
    );
}

#[test]
fn repeated_ambiguous_cache_admin_cleanup_keeps_one_bounded_watermark() {
    let mut connection = guarded_cache_admin_lease_fixture();
    for attempt in 0..1_024 {
        let claim_id = format!("cancelled-logout-{attempt}");
        release_guarded_cache_admin_lease(&mut connection, &claim_id, true, 1_000 + attempt);
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_cache_admin_revocation_release_watermark",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count bounded watermark rows"),
        1,
        "cancelled credential churn must retain one watermark, not one UUID row per request"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT expires_through FROM cluster_cache_admin_revocation_release_watermark \
                     WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("latest bounded watermark"),
        2_023,
        "the single row must retain the newest expiration boundary"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_cache_admin_revocation_lease_releases",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count legacy exact receipts"),
        0,
        "current cleanup must not grow the legacy exact-receipt table"
    );
}

#[test]
fn zero_after_ambiguous_cache_admin_acquire_advances_watermark() {
    assert!(cache_admin_rejection_requires_release_receipt(true));
    let mut connection = guarded_cache_admin_lease_fixture();
    assert_eq!(
        acquire_guarded_cache_admin_lease(&mut connection, "owner", true),
        1
    );
    mark_guarded_cache_admin_acquire_definitive(&mut connection, "owner");

    let delayed_claim = "ambiguous-then-zero";
    assert_eq!(
        acquire_guarded_cache_admin_lease(&mut connection, delayed_claim, true),
        0,
        "the retry observes a competing singleton after an earlier ambiguous submission"
    );
    release_guarded_cache_admin_lease(&mut connection, delayed_claim, true, 1_000);
    release_guarded_cache_admin_lease(&mut connection, "owner", false, 1_000);
    assert_eq!(
        acquire_guarded_cache_admin_lease(&mut connection, delayed_claim, true),
        0,
        "the watermark must defeat the earlier delayed acquire after the owner leaves"
    );
}

#[test]
fn server_commit_response_loss_and_crash_expire_cache_admin_exclusion_conservatively() {
    let mut connection = guarded_cache_admin_lease_fixture();
    let claim_id = "crashed-cache-admin-owner";
    // The server committed the first acquire with its conservative bit,
    // but the response was lost and the process crashed before it could
    // submit the separate definitive marker or an exact release.
    assert_eq!(
        acquire_guarded_cache_admin_lease(&mut connection, claim_id, true),
        1
    );

    // A rolled-back binary knows none of the cache-admin tables. Its
    // unchanged heartbeat UPDATE still executes the replicated trigger,
    // which tombstones and removes an expired exclusion.
    connection
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = 1_001 WHERE node_id = 'node-b'",
            [],
        )
        .expect("legacy heartbeat expires the lease");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_cache_admin_revocation_leases",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("live cache-admin leases"),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT expires_through FROM cluster_cache_admin_revocation_release_watermark \
                     WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("cache-admin expiry watermark"),
        1_000
    );
    assert_eq!(
        acquire_guarded_cache_admin_lease(&mut connection, claim_id, true),
        0,
        "a delayed acquire must lose to heartbeat-owned expiry cleanup"
    );

    let definitive_claim = "definitive-owner-crash";
    assert_eq!(
        acquire_guarded_cache_admin_lease_until(&mut connection, definitive_claim, true, 2_000,),
        1
    );
    mark_guarded_cache_admin_acquire_definitive(&mut connection, definitive_claim);
    connection
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = 2_001 WHERE node_id = 'node-b'",
            [],
        )
        .expect("legacy heartbeat expires definite lease");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_cache_admin_revocation_lease_releases \
                     WHERE claim_id = $1",
                rusqlite::params![definitive_claim],
                |row| row.get::<_, i64>(0),
            )
            .expect("definitive expiry receipt"),
        0,
        "a definitive acquire has no delayed proposal to tombstone"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT expires_through FROM cluster_cache_admin_revocation_release_watermark \
                     WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("definitive expiry preserves prior watermark"),
        1_000,
        "definitive expiry must not expand the ambiguous anti-replay boundary"
    );
}

#[test]
fn cache_admin_exclusion_is_separate_rolling_safe_and_capability_v4() {
    assert_eq!(
        CACHE_ADMIN_REVOCATION_CAPABILITY,
        "cache_admin_revocation_v4"
    );
    assert!(MEMBERSHIP_SCHEMA.iter().any(|statement| {
        statement.contains("CREATE TABLE IF NOT EXISTS cluster_operation_leases")
            && statement.contains("CHECK (operation IN ('restart', 'maintenance'))")
    }));
    assert!(MEMBERSHIP_SCHEMA.iter().any(|statement| {
        statement.contains("CREATE TABLE IF NOT EXISTS cluster_cache_admin_revocation_leases")
            && statement.contains("retain_release_receipt")
    }));
    for table in [
        "cluster_cache_admin_revocation_leases",
        "cluster_cache_admin_revocation_lease_releases",
        "cluster_cache_admin_revocation_release_watermark",
        "cluster_cache_admin_revocation_lease_intents",
        "cluster_credential_mutation_intents",
    ] {
        assert!(MEMBERSHIP_SCHEMA.iter().any(|statement| {
            statement.contains(&format!("CREATE TABLE IF NOT EXISTS {table}"))
        }));
    }
    for trigger in [
        "cluster_cache_admin_lease_heartbeat_expiry",
        "cluster_cache_admin_lease_removal_guard",
        "cluster_cache_admin_lease_promotion_guard",
        "cluster_cache_admin_lease_join_reservation_guard",
        "cluster_cache_admin_lease_join_staging_guard",
        "cluster_cache_admin_lease_operation_guard",
        "cluster_cache_admin_v1_insert_retired",
        "cluster_cache_admin_v1_update_retired",
        "cluster_cache_admin_v2_insert_retired",
        "cluster_cache_admin_v2_update_retired",
        "cluster_credential_user_update_guard",
        "cluster_credential_user_delete_guard",
        "cluster_credential_token_insert_guard",
        "cluster_credential_token_delete_guard",
    ] {
        assert!(MEMBERSHIP_SCHEMA
            .iter()
            .any(|statement| statement.contains(trigger)));
    }
    for trigger in [
        REQUIRE_CACHE_ADMIN_LEASE_INSERT_INTENT_SQL,
        REQUIRE_CACHE_ADMIN_LEASE_UPDATE_INTENT_SQL,
        REQUIRE_CACHE_ADMIN_LEASE_DELETE_INTENT_SQL,
    ] {
        assert!(trigger.contains("cluster_cache_admin_revocation_lease_intents"));
    }
}

#[test]
fn maintenance_is_replicated_and_rollout_gated() {
    assert!(MEMBERSHIP_SCHEMA.iter().any(|statement| {
        statement.contains("CREATE TABLE IF NOT EXISTS cluster_node_maintenance")
            && statement.contains("acknowledged_at")
    }));
    assert!(MEMBERSHIP_SCHEMA.iter().any(|statement| {
        statement.contains("CREATE TRIGGER IF NOT EXISTS cluster_node_maintenance_heartbeat_guard")
            && statement.contains("node maintenance requires current binary")
    }));
    assert!(MEMBERSHIP_SCHEMA.iter().any(|statement| {
        statement.contains("CREATE TABLE IF NOT EXISTS cluster_operation_leases")
            && statement.contains("CHECK (operation IN ('restart', 'maintenance'))")
    }));
    assert!(MEMBERSHIP_SCHEMA.iter().any(|statement| {
        statement.contains("CREATE TABLE IF NOT EXISTS cluster_operation_lease_releases")
    }));
    assert!(MEMBERSHIP_SCHEMA.iter().any(|statement| {
        statement.contains("CREATE TABLE IF NOT EXISTS cluster_operation_lease_intents")
    }));
    for trigger in [
        REQUIRE_OPERATION_LEASE_INSERT_INTENT_SQL,
        REQUIRE_OPERATION_LEASE_UPDATE_INTENT_SQL,
        REQUIRE_OPERATION_LEASE_DELETE_INTENT_SQL,
    ] {
        assert!(trigger.contains("requires current coordinator"));
    }
    for trigger in [
        "cluster_operation_lease_heartbeat_expiry",
        "cluster_operation_lease_removal_guard",
        "cluster_operation_lease_promotion_guard",
        "cluster_operation_lease_join_reservation_guard",
        "cluster_operation_lease_join_staging_guard",
    ] {
        assert!(MEMBERSHIP_SCHEMA
            .iter()
            .any(|statement| statement.contains(trigger)));
    }
    let ready = capability_ready_predicate(NODE_MAINTENANCE_CAPABILITY);
    assert!(ready.contains(NODE_MAINTENANCE_CAPABILITY));
    assert!(ready.contains("capability.last_seen_at = active.last_seen_at"));
}

#[test]
fn maintenance_preserves_the_live_voter_quorum() {
    assert!(maintenance_preserves_quorum(true, true, 3, 3, 2));
    assert!(!maintenance_preserves_quorum(true, true, 3, 2, 2));
    assert!(maintenance_preserves_quorum(true, true, 4, 4, 3));
    assert!(!maintenance_preserves_quorum(true, true, 4, 3, 3));
    assert!(maintenance_preserves_quorum(true, true, 1, 1, 1));
    assert!(maintenance_preserves_quorum(false, true, 3, 1, 2));
}

#[test]
fn live_tv_activation_and_legacy_join_race_to_one_safe_winner() {
    let path = std::env::temp_dir().join(format!(
        "plurx-live-tv-join-race-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let connection = rusqlite::Connection::open(&path).expect("race fixture");
    install_live_tv_membership_fixture(&connection);
    connection
        .execute(
            "INSERT INTO cluster_join_tokens VALUES ('legacy-token', 'issued', NULL)",
            [],
        )
        .expect("legacy token");
    drop(connection);

    let barrier = Arc::new(std::sync::Barrier::new(3));
    let activation_path = path.clone();
    let activation_barrier = Arc::clone(&barrier);
    let activation = std::thread::spawn(move || {
        let mut connection = rusqlite::Connection::open(activation_path).expect("activator");
        connection
            .busy_timeout(Duration::from_secs(5))
            .expect("activation busy timeout");
        activation_barrier.wait();
        commit_live_tv_activation(&mut connection, "1", 0)
    });
    let join_path = path.clone();
    let join_barrier = Arc::clone(&barrier);
    let join = std::thread::spawn(move || {
        let mut connection = rusqlite::Connection::open(join_path).expect("joiner");
        connection
            .busy_timeout(Duration::from_secs(5))
            .expect("join busy timeout");
        join_barrier.wait();
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .expect("join transaction");
        let result = transaction.execute(
            "UPDATE cluster_join_tokens SET state = 'redeeming', node_id = 'legacy-node' \
                 WHERE token_hash = 'legacy-token' AND state = 'issued'",
            [],
        );
        if result.as_ref().is_ok_and(|changed| *changed == 1) {
            transaction
                .execute(
                    "INSERT INTO cluster_node_join_staging VALUES ('legacy-node')",
                    [],
                )
                .expect("stage legacy join while disabled");
            transaction
                .execute(
                    "INSERT INTO cluster_nodes VALUES ('legacy-node', 'voter', NULL, 1)",
                    [],
                )
                .expect("publish legacy node");
            transaction.commit().expect("legacy join commit");
            true
        } else {
            false
        }
    });
    barrier.wait();
    let activated = activation.join().expect("activation outcome");
    let joined = join.join().expect("join outcome");
    assert_ne!(activated, joined, "exactly one side must commit");

    let connection = rusqlite::Connection::open(&path).expect("race result");
    let unsafe_state: bool = connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM settings WHERE key = 'live_tv.enabled' AND value = '1') \
                   AND EXISTS (SELECT 1 FROM cluster_node_join_staging)",
            [],
            |row| row.get(0),
        )
        .expect("unsafe-state query");
    assert!(
        !unsafe_state,
        "enabled state must never cross a legacy join"
    );
}

/// A joining node has the cluster schema and nothing else yet.
///
/// It replays the leader's Raft log before its own application store
/// exists, so `settings` is simply absent while every cluster table is
/// present. Redeeming the join token has to work in exactly that state.
/// With a guard that named `settings`, it did not: SQLite resolves a
/// trigger's body when it PREPARES the guarded statement, so the UPDATE
/// failed with `no such table: main.settings` before the WHEN clause was
/// ever considered, the join could never be redeemed, and the node
/// answered 503 to `/readyz` forever.
#[test]
fn a_joining_node_redeems_before_it_has_any_application_schema() {
    let connection = rusqlite::Connection::open_in_memory().expect("fixture");
    connection
            .execute_batch(&format!(
                "CREATE TABLE cluster_join_tokens (token_hash TEXT PRIMARY KEY, state TEXT NOT NULL, node_id TEXT); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_live_tv_join_intents (token_hash TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_live_tv_activation (singleton INTEGER PRIMARY KEY CHECK (singleton = 1), enabled INTEGER NOT NULL); \
                 {REQUIRE_LIVE_TV_JOIN_INTENT_ON_RESERVATION_SQL}; \
                 {REQUIRE_LIVE_TV_JOIN_INTENT_ON_STAGING_SQL}; \
                 INSERT INTO cluster_join_tokens VALUES ('joining-token', 'issued', NULL);"
            ))
            .expect("a joining node's cluster schema, with no `settings` anywhere");

    connection
        .execute(
            "UPDATE cluster_join_tokens SET state = 'redeeming', node_id = 'joining-node' \
                 WHERE token_hash = 'joining-token'",
            [],
        )
        .expect("the reservation must prepare without the application schema");
    connection
        .execute(
            "INSERT INTO cluster_node_join_staging VALUES ('joining-node')",
            [],
        )
        .expect("staging must prepare without the application schema");
    connection
        .execute(
            "UPDATE cluster_join_tokens SET state = 'redeemed' \
                 WHERE token_hash = 'joining-token' AND state = 'redeeming'",
            [],
        )
        .expect("redemption must prepare without the application schema");
}

/// The rule the defect broke, stated over the whole schema.
///
/// A trigger on a cluster table runs on every node's state machine,
/// including one that has replayed only this schema. Naming any other
/// table there makes the guarded statement unpreparable on that node --
/// not quietly inert, refused. A trigger on an application table is a
/// different matter and is not judged here: it is never created on a node
/// that lacks its target, which is the failure mode we want.
#[test]
fn a_cluster_table_guard_may_only_read_cluster_tables() {
    fn identifier(token: &str) -> String {
        token
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
            .to_owned()
    }

    let installed = MEMBERSHIP_SCHEMA
        .iter()
        .copied()
        .chain([
            REQUIRE_REMOVAL_INTENT_SQL,
            REQUIRE_LEARNER_JOIN_INTENT_SQL,
            MIRROR_LIVE_TV_ACTIVATION_ON_INSERT_SQL,
            MIRROR_LIVE_TV_ACTIVATION_ON_UPDATE_SQL,
            MIRROR_LIVE_TV_ACTIVATION_ON_DELETE_SQL,
        ])
        .collect::<Vec<_>>();
    let created = MEMBERSHIP_SCHEMA
        .iter()
        .filter_map(|statement| {
            statement
                .strip_prefix("CREATE TABLE IF NOT EXISTS ")
                .and_then(|rest| rest.split_whitespace().next())
                .map(identifier)
        })
        .collect::<BTreeSet<_>>();
    assert!(
        created.contains("cluster_live_tv_activation"),
        "the schema census found no tables; the scanner is broken, not the tree"
    );

    let mut offenders = Vec::new();
    for statement in installed {
        let Some(rest) = statement.strip_prefix("CREATE TRIGGER IF NOT EXISTS ") else {
            continue;
        };
        let words = rest.split_whitespace().collect::<Vec<_>>();
        let name = identifier(words.first().copied().unwrap_or_default());
        // The first `ON` in a trigger is its target: the event clause
        // precedes both WHEN and BEGIN.
        let Some(position) = words.iter().position(|word| *word == "ON") else {
            offenders.push(format!("{name}: no target table"));
            continue;
        };
        let target = identifier(words.get(position + 1).copied().unwrap_or_default());
        if !target.starts_with("cluster_") {
            continue;
        }
        // Only what follows the target: the event clause before it is
        // `BEFORE UPDATE OF <column>`, whose `OF` list names columns.
        for (index, word) in words.iter().enumerate().skip(position + 2) {
            if !matches!(*word, "FROM" | "JOIN" | "INTO" | "UPDATE") {
                continue;
            }
            let next = words.get(index + 1).copied().unwrap_or_default();
            // `DO UPDATE SET` assigns; it does not name a table.
            if matches!(next, "OF" | "SET") {
                continue;
            }
            let named = identifier(next);
            if named.is_empty() || named == target || created.contains(&named) {
                continue;
            }
            offenders.push(format!("{name} (on {target}) reads {named}"));
        }
    }

    assert!(
        offenders.is_empty(),
        "a trigger on a cluster table may only read cluster tables -- a joining node \
             has replayed nothing else, and SQLite refuses to prepare the guarded statement \
             rather than skipping the guard:\n{}",
        offenders.join("\n")
    );
}

/// A voter that joined as one owns Live TV; a learner does not.
///
/// `cluster_nodes.role` is written in exactly one place -- `promote_learner`
/// -- so a node that joined as a voter carries NULL there. `owner.role =
/// 'voter'` therefore matched no row in any real cluster, and the guard was
/// unsatisfiable: every visible requirement could be met and the write was
/// still refused, with nothing on screen saying which clause said no. The
/// fixture hid it by declaring `role TEXT NOT NULL` in a column order
/// production does not have, so the test agreed with the bug.
#[test]
fn a_voter_that_was_never_promoted_can_own_live_tv() {
    let connection = rusqlite::Connection::open_in_memory().expect("fixture");
    install_live_tv_membership_fixture(&connection);
    connection
        .execute(
            "INSERT INTO cluster_nodes (node_id, role, removed_at, last_seen_at) \
                 VALUES ('promoted', 'voter', NULL, 1)",
            [],
        )
        .expect("a node promoted from learner does carry the string");
    connection
        .execute(
            "INSERT INTO cluster_nodes (node_id, role, removed_at, last_seen_at) \
                 VALUES ('a-learner', 'learner', NULL, 1)",
            [],
        )
        .expect("learner row");

    let owner_ready = |node: &str| -> bool {
        connection
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM cluster_nodes owner \
                       WHERE owner.node_id = ?1 AND owner.role IS NOT 'learner' \
                         AND owner.removed_at IS NULL)",
                rusqlite::params![node],
                |row| row.get::<_, bool>(0),
            )
            .expect("owner readiness")
    };

    // `owner` is the fixture's node: joined as a voter, so NULL role.
    assert!(
        owner_ready("owner"),
        "a voter that joined as one carries NULL and must still be able to own Live TV"
    );
    assert!(owner_ready("promoted"), "a promoted learner is a voter too");
    assert!(
        !owner_ready("a-learner"),
        "a learner must not own the tuner"
    );
}

#[test]
fn enabled_live_tv_rejects_old_joins_and_rollback_heartbeats_fail_closed() {
    let mut connection = rusqlite::Connection::open_in_memory().expect("fixture");
    install_live_tv_membership_fixture(&connection);
    connection
        .execute_batch(
            "INSERT INTO settings VALUES ('live_tv.enabled', '1', 1); \
                 INSERT INTO settings VALUES ('live_tv.config_generation', '1', 1); \
                 INSERT INTO cluster_join_tokens VALUES ('old-token', 'issued', NULL); \
                 INSERT INTO cluster_join_tokens VALUES ('new-token', 'issued', NULL);",
        )
        .expect("enabled fixture");

    let old_join = connection.execute(
        "UPDATE cluster_join_tokens SET state = 'redeeming', node_id = 'old-node' \
             WHERE token_hash = 'old-token'",
        [],
    );
    assert!(
        old_join.is_err(),
        "old coordinator must hit the replicated trigger"
    );

    let transaction = connection.transaction().expect("compatible join");
    transaction
        .execute(
            "INSERT INTO cluster_live_tv_join_intents VALUES ('new-token')",
            [],
        )
        .expect("compatible proof");
    transaction
        .execute(
            "UPDATE cluster_join_tokens SET state = 'redeeming', node_id = 'new-node' \
                 WHERE token_hash = 'new-token'",
            [],
        )
        .expect("compatible reservation");
    transaction
        .execute(
            "INSERT INTO cluster_node_join_staging VALUES ('new-node')",
            [],
        )
        .expect("compatible staging");
    transaction
        .execute(
            // NULL role, like every voter that joined as one rather than
            // being promoted from a learner.
            "INSERT INTO cluster_nodes (node_id, role, removed_at, last_seen_at) \
                 VALUES ('new-node', NULL, NULL, 1)",
            [],
        )
        .expect("compatible node");
    transaction
        .execute("DELETE FROM cluster_live_tv_join_intents", [])
        .expect("consume proof");
    transaction.commit().expect("compatible join commit");

    connection
        .execute(
            "INSERT INTO cluster_node_capabilities VALUES ('new-node', $1, 1)",
            rusqlite::params![LIVE_TV_CAPABILITY],
        )
        .expect("new capability");
    connection
        .execute_batch(
            "UPDATE cluster_nodes SET last_seen_at = 2 WHERE node_id = 'new-node'; \
                 DELETE FROM cluster_node_join_staging WHERE node_id = 'new-node';",
        )
        .expect("rolled-back binary heartbeat shape");
    let ready: bool = connection
        .query_row(
            &format!("SELECT {}", capability_ready_predicate(LIVE_TV_CAPABILITY)),
            [],
            |row| row.get(0),
        )
        .expect("rollback capability verdict");
    assert!(
        !ready,
        "a rollback heartbeat must invalidate live-TV authority"
    );

    connection
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at = 2 \
                 WHERE node_id = 'new-node' AND capability = $1",
            rusqlite::params![LIVE_TV_CAPABILITY],
        )
        .expect("current heartbeat capability");
    let ready: bool = connection
        .query_row(
            &format!("SELECT {}", capability_ready_predicate(LIVE_TV_CAPABILITY)),
            [],
            |row| row.get(0),
        )
        .expect("current capability verdict");
    assert!(ready);
}

#[test]
fn absent_capable_node_cannot_publish_any_live_tv_setting() {
    let mut connection = rusqlite::Connection::open_in_memory().expect("fixture");
    install_live_tv_membership_fixture(&connection);
    assert!(
        !commit_live_tv_activation(&mut connection, "2", 2),
        "a capability proof from before the absence cutoff is stale"
    );
    let published: i64 = connection
        .query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))
        .expect("published setting count");
    assert_eq!(
        published, 0,
        "a refused activation must leave the complete tuple unpublished"
    );
}

#[test]
fn live_tv_generation_allows_one_concurrent_activation_writer() {
    let path = std::env::temp_dir().join(format!(
        "plurx-live-tv-cas-race-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let connection = rusqlite::Connection::open(&path).expect("CAS fixture");
    install_live_tv_membership_fixture(&connection);
    drop(connection);
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let writer = |limit: &'static str| {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            let mut connection = rusqlite::Connection::open(path).expect("CAS writer");
            connection
                .busy_timeout(Duration::from_secs(5))
                .expect("CAS busy timeout");
            barrier.wait();
            commit_live_tv_activation(&mut connection, limit, 0)
        })
    };
    let first = writer("1");
    let second = writer("2");
    barrier.wait();
    let winners = usize::from(first.join().expect("first writer"))
        + usize::from(second.join().expect("second writer"));
    assert_eq!(winners, 1);
    let connection = rusqlite::Connection::open(path).expect("CAS result");
    let generation: String = connection
        .query_row(
            "SELECT value FROM settings WHERE key = 'live_tv.config_generation'",
            [],
            |row| row.get(0),
        )
        .expect("committed generation");
    assert_eq!(generation, "1");
}

#[test]
fn leader_handoff_precedes_the_durable_maintenance_fence() {
    let source = include_str!("../membership.rs")
        .split_once("pub async fn enter_maintenance(")
        .expect("maintenance entry")
        .1
        .split_once("pub async fn exit_maintenance(")
        .expect("maintenance entry end")
        .0;
    let handoff = source
        .find("self.handoff_leadership(target.raft_id, &status).await?")
        .expect("leader handoff");
    // The fence moved into a named statement so its removal predicate is
    // testable, so pin the call rather than the literal; the ordering this
    // test exists for is unchanged.
    let commit = source
        .find("enter_maintenance_admission_sql()")
        .expect("durable maintenance fence");
    assert!(handoff < commit);
    assert!(enter_maintenance_admission_sql().contains("INSERT INTO cluster_node_maintenance"));
    assert!(source.contains("lease.operation != \"maintenance\""));
    assert!(source.contains("Param::StmtOutputNamed(0, \"node_id\".into())"));
    assert!(source.contains("DELETE FROM cluster_operation_leases"));
}

#[test]
fn restart_and_maintenance_race_for_one_replicated_outage_lease() {
    let path = std::env::temp_dir().join(format!(
        "plurx-cluster-operation-race-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let connection = rusqlite::Connection::open(&path).expect("lease fixture");
    connection
        .execute_batch(
            "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, removed_at INTEGER); \
                 INSERT INTO cluster_nodes VALUES ('node-a', NULL), ('node-b', NULL); \
                 CREATE TABLE cluster_node_maintenance (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_removal_attempts (node_id TEXT, attempt_id TEXT); \
                 CREATE TABLE cluster_node_promotions (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_operation_leases (\
                   singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
                   node_id TEXT NOT NULL, operation TEXT NOT NULL, \
                   claim_id TEXT NOT NULL UNIQUE, expires_at INTEGER NOT NULL); \
                 CREATE TABLE cluster_operation_lease_releases (claim_id TEXT PRIMARY KEY);",
        )
        .expect("lease schema");
    drop(connection);

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let race = |node_id: &'static str, operation: &'static str| {
        let path = path.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        std::thread::spawn(move || {
            let connection = rusqlite::Connection::open(path).expect("racing lease client");
            connection
                .busy_timeout(Duration::from_secs(5))
                .expect("busy timeout");
            barrier.wait();
            connection
                .execute(
                    &acquire_cluster_operation_lease_sql(),
                    rusqlite::params![
                        node_id,
                        operation,
                        uuid::Uuid::new_v4().to_string(),
                        1_000_i64,
                        100_i64
                    ],
                )
                .expect("serialized lease race")
        })
    };
    let restart = race("node-a", "restart");
    let maintenance = race("node-b", "maintenance");
    barrier.wait();
    let changed = restart.join().expect("restart claimant")
        + maintenance.join().expect("maintenance claimant");
    assert_eq!(changed, 1, "exactly one planned outage may commit");

    let connection = rusqlite::Connection::open(&path).expect("read lease winner");
    let winner_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM cluster_operation_leases", [], |row| {
            row.get(0)
        })
        .expect("winner count");
    assert_eq!(winner_count, 1);
    connection
        .execute("UPDATE cluster_operation_leases SET expires_at = 99", [])
        .expect("expire first claimant");
    assert_eq!(
        connection
            .execute(
                &acquire_cluster_operation_lease_sql(),
                rusqlite::params!["node-b", "maintenance", "successor", 2_000_i64, 100_i64],
            )
            .expect("replace expired claimant"),
        1
    );
    assert!(EXPIRE_OPERATION_LEASE_FROM_HEARTBEAT_SQL
        .contains("DELETE FROM cluster_operation_leases WHERE expires_at <= NEW.last_seen_at"));
    drop(connection);
    std::fs::remove_file(path).expect("remove lease fixture");
}

#[test]
fn released_planned_outage_claim_cannot_be_resurrected_by_a_delayed_write() {
    let mut connection = guarded_operation_lease_fixture();
    assert!(connection
        .execute(
            &acquire_cluster_operation_lease_sql(),
            rusqlite::params![
                "node-a",
                "restart",
                "previous-release-claim",
                1_000_i64,
                100_i64
            ],
        )
        .is_err());
    assert_eq!(
        acquire_guarded_operation_lease(&mut connection, "node-a", "restart", "ambiguous-claim",),
        1
    );
    assert_eq!(
        acquire_guarded_operation_lease(&mut connection, "node-a", "restart", "ambiguous-claim",),
        1,
        "retrying the exact claim is idempotent before release"
    );
    assert!(connection
        .execute(
            "UPDATE cluster_operation_leases SET expires_at = 2_000 \
                 WHERE claim_id = 'ambiguous-claim'",
            [],
        )
        .is_err());
    assert_eq!(
        connection
            .execute(
                "DELETE FROM cluster_operation_leases WHERE claim_id = 'ambiguous-claim'",
                [],
            )
            .expect("legacy exact release is a safe no-op"),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM cluster_operation_leases", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("ignored legacy release keeps exact claim"),
        1
    );
    release_guarded_operation_lease(&mut connection, "node-a", "restart", "ambiguous-claim");

    assert_eq!(
        acquire_guarded_operation_lease(&mut connection, "node-a", "restart", "ambiguous-claim",),
        0,
        "a delayed disconnected acquire must not recreate its released lease"
    );
    assert_eq!(
        acquire_guarded_operation_lease(&mut connection, "node-a", "restart", "successor-claim",),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT claim_id FROM cluster_operation_leases", [], |row| {
                row.get::<_, String>(0)
            },)
            .expect("successor lease"),
        "successor-claim"
    );
}

#[test]
fn maintenance_and_exact_release_are_safe_in_both_commit_orders() {
    let mut release_first = guarded_operation_lease_fixture();
    assert_eq!(
        acquire_guarded_operation_lease(
            &mut release_first,
            "node-a",
            "maintenance",
            "release-first",
        ),
        1
    );
    release_guarded_operation_lease(&mut release_first, "node-a", "maintenance", "release-first");
    assert!(!commit_guarded_maintenance(
        &mut release_first,
        "node-a",
        "release-first",
    ));
    assert_eq!(
        release_first
            .query_row("SELECT COUNT(*) FROM cluster_node_maintenance", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("release-first maintenance count"),
        0
    );

    let mut maintenance_first = guarded_operation_lease_fixture();
    assert_eq!(
        acquire_guarded_operation_lease(
            &mut maintenance_first,
            "node-a",
            "maintenance",
            "maintenance-first",
        ),
        1
    );
    assert!(commit_guarded_maintenance(
        &mut maintenance_first,
        "node-a",
        "maintenance-first",
    ));
    release_guarded_operation_lease(
        &mut maintenance_first,
        "node-a",
        "maintenance",
        "maintenance-first",
    );
    assert_eq!(
        maintenance_first
            .query_row("SELECT COUNT(*) FROM cluster_node_maintenance", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("maintenance-first maintenance count"),
        1
    );
    assert_eq!(
        maintenance_first
            .query_row("SELECT COUNT(*) FROM cluster_operation_leases", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("maintenance-first lease count"),
        0
    );
}

#[test]
fn ambiguous_operation_acquire_retries_only_while_its_owned_claim_is_live() {
    let ambiguous = hiqlite::Error::Connect("any disconnected response".to_owned());
    assert!(retry_ambiguous_cluster_operation_acquire(
        &ambiguous, 999, 1_000,
    ));
    assert!(!retry_ambiguous_cluster_operation_acquire(
        &ambiguous, 1_000, 1_000,
    ));
    assert!(!retry_ambiguous_cluster_operation_acquire(
        &hiqlite::Error::Timeout("definitive timeout".to_owned()),
        999,
        1_000,
    ));
    assert!(cluster_operation_write_may_be_ambiguous(
        &hiqlite::Error::Connect("leader changed".to_owned()),
    ));
}

#[test]
fn planned_outage_lease_excludes_a_concurrent_production_removal_attempt() {
    let path = std::env::temp_dir().join(format!(
        "plurx-outage-preflight-removal-race-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let connection = rusqlite::Connection::open(&path).expect("membership race fixture");
    connection
        .execute_batch(
            "CREATE TABLE cluster_node_removal_attempts (\
                   node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, \
                   PRIMARY KEY(node_id, attempt_id)); \
                 CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, last_seen_at INTEGER NOT NULL, removed_at INTEGER); \
                 CREATE TABLE cluster_node_capabilities (\
                   node_id TEXT, capability TEXT, last_seen_at INTEGER, \
                   PRIMARY KEY(node_id, capability)); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_promotions (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_maintenance (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_operation_leases (\
                   singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
                   node_id TEXT NOT NULL, operation TEXT NOT NULL, \
                   claim_id TEXT NOT NULL UNIQUE, expires_at INTEGER NOT NULL); \
                 CREATE TABLE cluster_operation_lease_releases (claim_id TEXT PRIMARY KEY); \
                 CREATE TABLE media_sessions (\
                   owner_node_id TEXT, state TEXT, lease_expires_at_ms INTEGER); \
                 INSERT INTO cluster_nodes VALUES ('node-a', 10, NULL), ('node-b', 10, NULL); \
                 INSERT INTO cluster_node_capabilities VALUES \
                   ('node-a', 'membership_removal_attempt_refs_v1', 10), \
                   ('node-b', 'membership_removal_attempt_refs_v1', 10);",
        )
        .expect("production membership lifecycle schema");
    connection
        .execute_batch(PROTECT_OPERATION_LEASE_FROM_REMOVAL_SQL)
        .expect("previous-release removal guard");
    assert_eq!(
        connection
            .execute(
                &acquire_cluster_operation_lease_sql(),
                rusqlite::params!["node-a", "restart", "preflight", 1_000_i64, 100_i64],
            )
            .expect("claim planned-outage lease before preflight"),
        1
    );

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let removal_path = path.clone();
    let removal_barrier = barrier.clone();
    let removal = std::thread::spawn(move || {
        let connection =
            rusqlite::Connection::open(removal_path).expect("concurrent removal client");
        connection
            .busy_timeout(Duration::from_secs(5))
            .expect("removal busy timeout");
        removal_barrier.wait();
        connection
            .execute(
                &begin_removal_attempt_sql(false),
                rusqlite::params!["node-b", "racing-removal", 50_i64],
            )
            .expect("lease predicate rejects current removal without a SQL error")
    });

    // This barrier represents the fresh status/directory/probe collection:
    // the production lease is already durable before the competing
    // lifecycle write is allowed to run.
    barrier.wait();
    assert_eq!(
        removal.join().expect("concurrent removal attempt"),
        0,
        "membership removal must not cross a lease-protected preflight"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_node_removal_attempts",
                [],
                |row| { row.get::<_, i64>(0) }
            )
            .expect("removal attempt count"),
        0
    );

    connection
        .execute("DELETE FROM cluster_operation_leases", [])
        .expect("finish planned outage");
    assert_eq!(
        connection
            .execute(
                &begin_removal_attempt_sql(false),
                rusqlite::params!["node-b", "post-preflight-removal", 50_i64],
            )
            .expect("removal resumes after lease release"),
        1
    );
    drop(connection);
    std::fs::remove_file(path).expect("remove membership race fixture");
}

#[test]
fn previous_release_lifecycle_writes_cannot_cross_an_outage_lease() {
    let source = production_source();
    let heartbeat_source = source
        .split_once("async fn commit_heartbeat(")
        .expect("production heartbeat")
        .1
        .split_once(concat!("async fn refresh_local_", "maintenance("))
        .expect("production heartbeat end")
        .0;
    assert!(
            !heartbeat_source.contains("DELETE FROM cluster_operation_leases"),
            "the current heartbeat UPDATE owns expiry through the schema trigger; only the preceding release needs the ignored compatibility DELETE",
        );
    let mut connection = rusqlite::Connection::open_in_memory().expect("sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_operation_leases (\
                   singleton INTEGER PRIMARY KEY, node_id TEXT NOT NULL, \
                   operation TEXT NOT NULL, claim_id TEXT NOT NULL, expires_at INTEGER NOT NULL); \
                 CREATE TABLE cluster_operation_lease_releases (claim_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_operation_lease_intents (claim_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_maintenance (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, last_seen_at INTEGER NOT NULL); \
                 CREATE TABLE cluster_node_removal_attempts (\
                   node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, \
                   PRIMARY KEY(node_id, attempt_id)); \
                 CREATE TABLE cluster_node_promotions (\
                   node_id TEXT PRIMARY KEY, attempt_id TEXT NOT NULL, \
                   barrier_index INTEGER, started_at INTEGER NOT NULL); \
                 CREATE TABLE cluster_join_tokens (\
                   token_hash TEXT PRIMARY KEY, state TEXT NOT NULL); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY);",
        )
        .expect("previous-release lifecycle fixture");
    for trigger in [
        EXPIRE_OPERATION_LEASE_FROM_HEARTBEAT_SQL,
        REQUIRE_OPERATION_LEASE_INSERT_INTENT_SQL,
        REQUIRE_OPERATION_LEASE_UPDATE_INTENT_SQL,
        REQUIRE_OPERATION_LEASE_DELETE_INTENT_SQL,
        PROTECT_OPERATION_LEASE_FROM_REMOVAL_SQL,
        PROTECT_OPERATION_LEASE_FROM_PROMOTION_SQL,
        PROTECT_OPERATION_LEASE_FROM_JOIN_RESERVATION_SQL,
        PROTECT_OPERATION_LEASE_FROM_JOIN_STAGING_SQL,
    ] {
        connection
            .execute_batch(trigger)
            .expect("install lease guard");
    }
    connection
        .execute_batch(
            "INSERT INTO cluster_join_tokens VALUES ('old-join', 'issued'); \
                 INSERT INTO cluster_nodes VALUES ('old-voter', 100); \
                 INSERT INTO cluster_operation_lease_intents VALUES ('claim-a'); \
                 INSERT INTO cluster_operation_leases VALUES \
                   (1, 'node-a', 'restart', 'claim-a', 1000); \
                 DELETE FROM cluster_operation_lease_intents;",
        )
        .expect("active outage lease");

    // These are the lifecycle-begin write shapes used by the preceding
    // release, which has no lease predicate of its own.
    assert!(connection
        .execute(
            "INSERT INTO cluster_node_removal_attempts VALUES ($1, $2)",
            rusqlite::params!["node-c", "old-removal"],
        )
        .is_err());
    assert!(connection
        .execute(
            "INSERT INTO cluster_node_promotions VALUES ($1, $2, NULL, $3)",
            rusqlite::params!["node-c", "old-promotion", 1_i64],
        )
        .is_err());
    assert!(connection
        .execute(
            "UPDATE cluster_join_tokens SET state = 'redeeming' \
                 WHERE token_hash = 'old-join' AND state = 'issued'",
            [],
        )
        .is_err());
    assert!(connection
        .execute(
            "INSERT INTO cluster_node_join_staging VALUES ('old-join')",
            [],
        )
        .is_err());

    // Execute the preceding release's exact leading DELETE + heartbeat
    // UPDATE shape under the complete final trigger set. The guard ignores
    // the unproved DELETE, then the UPDATE-owned trigger records a receipt
    // and deletes the expired claim in this same transaction.
    let heartbeat = connection.transaction().expect("old heartbeat transaction");
    assert_eq!(
        heartbeat
            .execute(
                "DELETE FROM cluster_operation_leases WHERE expires_at <= $1",
                rusqlite::params![1001_i64],
            )
            .expect("legacy leading expiry delete is ignored"),
        0
    );
    heartbeat
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = 1001 \
                 WHERE node_id = 'old-voter'",
            [],
        )
        .expect("previous-release heartbeat clears expired lease");
    heartbeat
        .commit()
        .expect("commit previous-release heartbeat");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM cluster_operation_leases", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("expired lease count"),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_operation_lease_releases \
                     WHERE claim_id = 'claim-a'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("expired claim receipt"),
        1
    );
    assert_eq!(
        connection
            .execute(
                "INSERT INTO cluster_node_removal_attempts VALUES ($1, $2)",
                rusqlite::params!["node-c", "old-removal"],
            )
            .expect("removal resumes after release"),
        1
    );
    assert_eq!(
        connection
            .execute(
                "UPDATE cluster_join_tokens SET state = 'redeeming' \
                     WHERE token_hash = 'old-join' AND state = 'issued'",
                [],
            )
            .expect("join resumes after release"),
        1
    );
}

#[test]
fn delayed_acquisition_cannot_extend_the_local_fence_past_the_lease() {
    let requested = Duration::from_secs(60);
    let replicated_expiry = 65_000_i64;
    let after_slow_consensus = 5_800_i64;
    let local_expiry =
        bounded_local_operation_expiry(after_slow_consensus, requested, replicated_expiry)
            .expect("remaining lease time");
    assert_eq!(
        local_expiry,
        u64::try_from(replicated_expiry).expect("positive fixture expiry")
    );
    assert!(
        bounded_local_operation_expiry(replicated_expiry, requested, replicated_expiry).is_none()
    );
}

#[test]
fn maintenance_exit_is_one_atomic_current_process_and_drain_proof() {
    let mut connection = production_membership_tables();
    // `media_sessions` belongs to the store schema, not this module's;
    // the stand-in carries the three columns the statement reads.
    connection
            .execute_batch(
                "CREATE TABLE media_sessions (owner_node_id TEXT, state TEXT, lease_expires_at_ms INTEGER); \
                 INSERT INTO cluster_node_maintenance VALUES ('node-a', 90, NULL); \
                 INSERT INTO cluster_nodes (node_id, raft_id, raft_address, api_address, last_seen_at) \
                   VALUES ('node-a', 1, 'node-a:32401', 'node-a:32402', 100); \
                 INSERT INTO cluster_node_progress (node_id, current_term, apply_lag_entries, \
                   bounded_read_ready, voter_storage_ready, voter_role_persisted, observed_at) \
                   VALUES ('node-a', 1, 0, 1, 1, 1, 100); \
                 INSERT INTO cluster_node_capabilities VALUES ('node-a', 'node_maintenance_v1', 100);",
            )
            .expect("maintenance exit fixture");
    let clear = |connection: &rusqlite::Connection| {
        connection
            .execute(
                EXIT_MAINTENANCE_SQL,
                rusqlite::params!["node-a", 95, NODE_MAINTENANCE_CAPABILITY, 100],
            )
            .expect("maintenance exit")
    };

    assert_eq!(clear(&connection), 0, "acknowledgement is mandatory");
    connection
        .execute(
            "UPDATE cluster_node_maintenance SET acknowledged_at = 100",
            [],
        )
        .expect("acknowledge");
    connection
        .execute("UPDATE cluster_nodes SET last_seen_at = 90", [])
        .expect("make heartbeat stale");
    assert_eq!(
        clear(&connection),
        0,
        "a stopped or stale process is refused"
    );
    connection
        .execute("UPDATE cluster_nodes SET last_seen_at = 100", [])
        .expect("restore heartbeat");
    connection
        .execute(
            "INSERT INTO media_sessions VALUES ('node-a', 'active', 101)",
            [],
        )
        .expect("active media");
    assert_eq!(clear(&connection), 0, "active replicated media is refused");
    connection
        .execute("UPDATE media_sessions SET state = 'ended'", [])
        .expect("drain media");

    let transaction = connection.transaction().expect("exit transaction");
    assert_eq!(clear(&transaction), 1, "all current proofs permit exit");
    transaction.rollback().expect("simulate failed commit");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM cluster_node_maintenance", [], |row| {
                row.get::<_, i64>(0)
            },)
            .expect("maintenance row"),
        1,
        "a failed transaction leaves the durable fence intact"
    );
}

#[test]
fn every_membership_lifecycle_begin_excludes_planned_outages() {
    assert!(begin_removal_attempt_sql(false)
        .contains("NOT EXISTS (SELECT 1 FROM cluster_node_maintenance)"));
    assert!(begin_removal_attempt_sql(false)
        .contains("NOT EXISTS (SELECT 1 FROM cluster_operation_leases)"));
    let source = production_source();
    let redeem = source
        .split_once("async fn redeem_for_role(")
        .expect("join redemption")
        .1
        .split_once("async fn upsert_hostname(")
        .expect("join redemption end")
        .0;
    assert!(
        redeem
            .matches("NOT EXISTS (SELECT 1 FROM cluster_node_maintenance)")
            .count()
            >= 5,
        "every token reservation/proof branch must carry the atomic maintenance exclusion"
    );
    assert!(
        redeem
            .matches("NOT EXISTS (SELECT 1 FROM cluster_operation_leases)")
            .count()
            >= 5,
        "every token reservation/proof branch must carry the atomic outage-lease exclusion"
    );
    let promotion = source
        .split_once("pub async fn promote_learner(")
        .expect("learner promotion")
        .1
        .split_once("async fn promotion_target(")
        .expect("learner promotion end")
        .0;
    assert!(promotion.contains("NOT EXISTS (SELECT 1 FROM cluster_node_maintenance)"));
    assert!(promotion.contains("NOT EXISTS (SELECT 1 FROM cluster_operation_leases)"));
    assert!(promotion.contains("MembershipError::MaintenanceConflict"));
}

#[test]
fn maintenance_rejects_a_heartbeat_without_the_current_binary_intent() {
    let connection = rusqlite::Connection::open_in_memory().expect("sqlite");
    connection
            .execute_batch(
                "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, last_seen_at INTEGER NOT NULL); \
                 CREATE TABLE cluster_node_maintenance (node_id TEXT PRIMARY KEY, requested_at INTEGER NOT NULL, acknowledged_at INTEGER); \
                 CREATE TABLE cluster_node_maintenance_heartbeat_intents (node_id TEXT PRIMARY KEY, last_seen_at INTEGER NOT NULL);",
            )
            .expect("maintenance tables");
    let trigger = MEMBERSHIP_SCHEMA
        .iter()
        .find(|statement| statement.contains("cluster_node_maintenance_heartbeat_guard"))
        .expect("maintenance heartbeat trigger");
    connection.execute_batch(trigger).expect("guard trigger");
    connection
        .execute_batch(
            "INSERT INTO cluster_nodes VALUES ('node-a', 100); \
                 INSERT INTO cluster_node_maintenance VALUES ('node-a', 100, 100);",
        )
        .expect("maintained node");
    let old = connection.execute(
        "UPDATE cluster_nodes SET last_seen_at = 200 WHERE node_id = 'node-a'",
        [],
    );
    assert!(
        old.is_err(),
        "a binary without the intent must not heartbeat through maintenance"
    );
    connection
        .execute_batch(
            "INSERT INTO cluster_node_maintenance_heartbeat_intents VALUES ('node-a', 200); \
                 UPDATE cluster_nodes SET last_seen_at = 200 WHERE node_id = 'node-a';",
        )
        .expect("current heartbeat");
}

#[test]
fn maintenance_acknowledgement_follows_the_target_local_read() {
    let source = production_source();
    let heartbeat = source
        .split_once("async fn commit_heartbeat")
        .expect("heartbeat")
        .1
        .split_once("async fn refresh_local_maintenance")
        .expect("end heartbeat")
        .0;
    let local_read = heartbeat
        .find("let maintenance_requested = self.refresh_local_maintenance")
        .expect("local maintenance read");
    let guard = heartbeat
        .find("if maintenance_requested")
        .expect("acknowledgement guard");
    let acknowledgement = heartbeat
        .find("UPDATE cluster_node_maintenance SET acknowledged_at")
        .expect("acknowledgement statement");
    assert!(local_read < guard && guard < acknowledgement, "{heartbeat}");
}

#[test]
fn maintenance_and_election_refusals_are_typed_for_operator_recovery() {
    assert_eq!(
        MembershipError::MaintenanceResumeUnsafe("node-b".to_owned()).code(),
        "maintenance_resume_unsafe"
    );
    assert_eq!(
        MembershipError::ElectionQuorumUnavailable.code(),
        "election_quorum_unavailable"
    );
    assert!(!production_source().contains("force_reconfigure"));
}

#[test]
fn passive_membership_metrics_distinguish_unavailable_from_fresh_cluster_state() {
    let unavailable = PassiveMembershipMetrics::default().snapshot();
    assert!(!unavailable.replicated);
    assert!(!unavailable.valid);
    assert!(unavailable.sample.is_none());

    let metrics = PassiveMembershipMetrics::replicated();
    let sample = MembershipMetricsSample {
        voters: 4,
        learners: 0,
        heartbeat_fresh_voters: 3,
        heartbeat_stale_voters: 1,
        removals_pending: 0,
        local_is_voter: true,
    };
    metrics.record(sample);
    let view = metrics.snapshot();
    assert!(view.replicated);
    assert!(view.valid);
    assert_eq!(view.age_seconds, Some(0));
    assert_eq!(view.sample, Some(sample));
    assert_eq!(view.errors, 0);

    metrics.record_error();
    assert_eq!(metrics.snapshot().errors, 1);
}

#[tokio::test(start_paused = true)]
async fn heartbeat_gate_pins_commit_window_and_retries_failures() {
    let gate = HeartbeatWriteGate::default();
    let commits = std::sync::atomic::AtomicUsize::new(0);

    assert!(gate
        .run(async {
            commits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        })
        .await
        .expect("leading heartbeat"));
    tokio::time::advance(HEARTBEAT_COALESCE_WINDOW - Duration::from_millis(1)).await;
    assert!(!gate
        .run(async {
            commits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        })
        .await
        .expect("inside-window heartbeat"));
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(gate
        .run(async {
            commits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        })
        .await
        .expect("boundary heartbeat"));
    assert_eq!(commits.load(std::sync::atomic::Ordering::Relaxed), 2);

    tokio::time::advance(HEARTBEAT_COALESCE_WINDOW).await;
    let failed = gate
        .run(async {
            Err(MembershipError::Internal(
                "injected heartbeat failure".to_owned(),
            ))
        })
        .await;
    assert!(failed.is_err());
    assert!(gate
        .run(async {
            commits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        })
        .await
        .expect("heartbeat retry"));
    assert_eq!(commits.load(std::sync::atomic::Ordering::Relaxed), 3);
}

#[test]
fn reachability_expires_immediately_after_the_allowed_missed_beats() {
    assert_eq!(
        NODE_REACHABLE_WINDOW_MS as u128,
        HEARTBEAT_INTERVAL.as_millis() * 3,
        "reachability must remain explicitly coupled to heartbeat cadence"
    );
    let last_seen_at = 1_000_000;
    assert!(node_is_reachable(
        last_seen_at + NODE_REACHABLE_WINDOW_MS,
        last_seen_at
    ));
    assert!(!node_is_reachable(
        last_seen_at + NODE_REACHABLE_WINDOW_MS + 1,
        last_seen_at
    ));
    assert_eq!(
        reachable_after(last_seen_at + NODE_REACHABLE_WINDOW_MS + 1),
        last_seen_at + 1
    );
}

#[test]
fn activity_key_cache_replaces_one_peer_without_exceeding_its_bound() {
    let mut keys = (0..MAX_ACTIVITY_PEERS)
        .map(|index| (format!("node-{index:03}"), vec![index as u8; 32]))
        .collect::<BTreeMap<_, _>>();

    let evicted =
        insert_bounded_activity_public_key(&mut keys, "node-064".to_owned(), vec![0x64; 32]);

    assert_eq!(keys.len(), MAX_ACTIVITY_PEERS);
    assert_eq!(evicted.as_deref(), Some("node-000"));
    assert!(!keys.contains_key("node-000"));
    assert_eq!(keys.get("node-064"), Some(&vec![0x64; 32]));
}

#[test]
fn activity_key_lookup_admission_reopens_after_one_window() {
    let started = Instant::now();
    let mut admission = ActivityAuthAdmission {
        window_started: started,
        checks: 0,
    };

    for _ in 0..MAX_ACTIVITY_KEY_LOOKUPS_PER_SECOND {
        assert!(admission.admit(started, MAX_ACTIVITY_KEY_LOOKUPS_PER_SECOND));
    }
    assert!(!admission.admit(started, MAX_ACTIVITY_KEY_LOOKUPS_PER_SECOND));
    assert!(admission.admit(
        started + Duration::from_secs(1),
        MAX_ACTIVITY_KEY_LOOKUPS_PER_SECOND
    ));
}

#[test]
fn activity_authentication_metrics_have_fixed_privacy_safe_cardinality() {
    let metrics = prometheus_cluster_activity_authority();
    assert!(metrics.contains("plurx_cluster_activity_auth_admission_refusals_total "));
    assert!(metrics.contains("plurx_cluster_activity_key_lookup_refusals_total "));
    assert!(!metrics.contains("node_id="));
    assert!(!metrics.contains("hostname="));
    assert!(!metrics.contains("url="));
}

#[test]
fn internal_media_read_preverification_has_a_bounded_burst() {
    let started = Instant::now();
    let mut admission = InternalReadAdmission {
        window_started: started,
        checks: 0,
    };
    for _ in 0..MAX_INTERNAL_READ_PREVERIFY_PER_SECOND {
        assert!(admission.admit(started));
    }
    assert!(!admission.admit(started));
    assert!(admission.admit(started + Duration::from_secs(1)));
}

#[test]
fn exact_request_preverification_has_a_hard_global_rate() {
    let admission = Mutex::new(ActivityAuthAdmission {
        window_started: Instant::now(),
        checks: 0,
    });
    for _ in 0..MAX_INTERNAL_PREVERIFY_PER_SECOND {
        assert!(admit_internal_preverification(&admission).expect("admission"));
    }
    assert!(!admit_internal_preverification(&admission).expect("saturation"));
}

#[test]
fn exact_internal_replay_is_rejected_for_the_entire_auth_window() {
    let started = Instant::now();
    let mut replays = InternalAuthReplayWindow::default();

    assert!(replays.admit("nonce-a", started));
    assert!(!replays.admit("nonce-a", started + Duration::from_secs(29)));
    assert!(replays.admit(
        "nonce-a",
        started + Duration::from_millis(ACTIVITY_AUTH_WINDOW_MS as u64 + 1)
    ));
}

#[test]
fn exact_internal_replay_allows_distinct_nonces_at_the_same_timestamp() {
    let started = Instant::now();
    let mut replays = InternalAuthReplayWindow::default();

    assert!(replays.admit("nonce-a", started));
    assert!(replays.admit("nonce-b", started));
}

#[test]
fn exact_internal_replay_cache_rejects_instead_of_evicting_live_proofs() {
    let started = Instant::now();
    let mut replays = InternalAuthReplayWindow::default();
    for index in 0..MAX_INTERNAL_REPLAYS_PER_PEER {
        assert!(replays.admit(&format!("nonce-{index}"), started));
    }
    assert!(!replays.admit("overflow", started));
    assert!(!replays.admit("nonce-0", started));
}

#[test]
fn internal_read_replay_is_rejected_for_the_entire_read_window() {
    let started = Instant::now();
    let mut replays = InternalReadReplayWindow::default();

    assert!(replays.admit("nonce-a", started));
    assert!(!replays.admit("nonce-a", started + Duration::from_millis(4_999)));
    assert!(replays.admit(
        "nonce-a",
        started + Duration::from_millis(INTERNAL_READ_AUTH_WINDOW_MS as u64 + 1)
    ));
}

#[test]
fn internal_read_replay_cache_rejects_instead_of_evicting_live_proofs() {
    let started = Instant::now();
    let mut replays = InternalReadReplayWindow::default();
    for index in 0..MAX_INTERNAL_READ_REPLAYS_PER_PEER {
        assert!(replays.admit(&format!("nonce-{index}"), started));
    }
    assert!(!replays.admit("overflow", started));
    assert!(!replays.admit("nonce-0", started));
}

#[test]
fn every_post_send_membership_error_keeps_the_removal_fence() {
    for status in [
        reqwest::StatusCode::REQUEST_TIMEOUT,
        reqwest::StatusCode::CONFLICT,
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        assert!(matches!(
            membership_response_failure(status),
            MembershipChangeFailure::Ambiguous(_)
        ));
    }
}

#[tokio::test]
async fn removal_transition_preserves_proposal_outcomes() {
    use lifecycle::{RemovalEffect, RemovalProposalOutcome, RemovalTransition};
    for path in [
        RemovalPath::Voter,
        RemovalPath::Learner,
        RemovalPath::SelfLeave,
    ] {
        for (proposal_kind, survivor, expected) in [
            (
                RemovalProposalOutcome::Rejected,
                MembershipChangeOutcome::Indeterminate,
                vec!["rollback:node-a:attempt-a"],
            ),
            (
                RemovalProposalOutcome::Accepted,
                MembershipChangeOutcome::Indeterminate,
                vec!["finalize:node-a"],
            ),
            (
                RemovalProposalOutcome::Ambiguous,
                MembershipChangeOutcome::Removed,
                vec!["reconcile", "finalize:node-a"],
            ),
            (
                RemovalProposalOutcome::Ambiguous,
                MembershipChangeOutcome::Indeterminate,
                vec!["reconcile"],
            ),
            (
                RemovalProposalOutcome::Ambiguous,
                MembershipChangeOutcome::Promoted,
                vec!["reconcile"],
            ),
        ] {
            let events = Mutex::new(Vec::<String>::new());
            let events = &events;
            let error = MembershipError::Internal("original proposal error".to_owned());
            let proposal = match proposal_kind {
                RemovalProposalOutcome::Accepted => Ok(()),
                RemovalProposalOutcome::Rejected => Err(MembershipChangeFailure::Rejected(error)),
                RemovalProposalOutcome::Ambiguous => Err(MembershipChangeFailure::Ambiguous(error)),
            };
            let result = dispatch_removal_outcome(
                path,
                "node-a",
                "attempt-a",
                proposal,
                |node, attempt, error| async move {
                    events
                        .lock()
                        .expect("events")
                        .push(format!("rollback:{node}:{attempt}"));
                    error
                },
                || async move {
                    events.lock().expect("events").push("reconcile".to_owned());
                    survivor
                },
                |node| async move {
                    events
                        .lock()
                        .expect("events")
                        .push(format!("finalize:{node}"));
                },
            )
            .await;
            assert_eq!(*events.lock().expect("events"), expected);
            match proposal_kind {
                RemovalProposalOutcome::Accepted => assert!(result.is_ok()),
                RemovalProposalOutcome::Rejected => assert!(
                    matches!(result, Err(MembershipError::Internal(ref message)) if message == "original proposal error")
                ),
                RemovalProposalOutcome::Ambiguous
                    if survivor == MembershipChangeOutcome::Removed =>
                {
                    assert!(result.is_ok())
                }
                // Each path keeps the answer it gave before the paths shared
                // the transition: a learner removal is 409 pending, a voter
                // removal and a self-leave are internal errors.
                RemovalProposalOutcome::Ambiguous => match path {
                    RemovalPath::Voter => assert!(
                        matches!(result, Err(MembershipError::Internal(ref message)) if message == "voter removal outcome is indeterminate after cluster membership operation failed: original proposal error; the target remains fenced")
                    ),
                    RemovalPath::Learner => assert!(
                        matches!(result, Err(MembershipError::RemovalPending(ref message)) if message == "learner removal outcome is indeterminate after cluster membership operation failed: original proposal error")
                    ),
                    RemovalPath::SelfLeave => assert!(
                        matches!(result, Err(MembershipError::Internal(ref message)) if message == "self-removal outcome is indeterminate after cluster membership operation failed: original proposal error; this voter remains fenced")
                    ),
                },
            }
            let transition = RemovalTransition::proposal("node-a", "attempt-a", proposal_kind);
            assert_eq!(
                (transition.node_id, transition.attempt_id),
                ("node-a", "attempt-a")
            );
            if proposal_kind == RemovalProposalOutcome::Ambiguous {
                assert_eq!(transition.effect, RemovalEffect::ReconcileSurvivors);
                assert_eq!(
                    transition.survivors(false).expect("proof pending").effect,
                    RemovalEffect::RetainFence
                );
            } else {
                assert!(transition.survivors(true).is_none());
            }
        }
    }
}

/// S-14: each real removal binds the shared transition with its own path
/// and survivor proof. A learner removal reconciles member removal (the
/// learner leaves the member set); the voter paths reconcile the voter
/// change; only the self-leave bounds its final tombstone. Swapping any of
/// these would type-check, so the wiring is pinned from source.
#[test]
fn removal_call_sites_bind_their_own_path_and_reconcile() {
    let source = include_str!("../membership.rs");
    for (signature, path, reconcile, finalize) in [
        (
            "async fn remove_learner_captured(",
            "RemovalPath::Learner",
            "|| reconcile_member_removal(&inner.secrets.api, target_raft_id, &membership_nodes)",
            "|finalize_node| self.finalize_node_removal(finalize_node)",
        ),
        (
            "async fn remove_voter_captured(",
            "RemovalPath::Voter",
            "|| reconcile_membership_change(&inner.secrets.api, target_raft_id, &membership_nodes)",
            "|finalize_node| self.finalize_node_removal(finalize_node)",
        ),
        (
            "async fn leave_voter_captured(",
            "RemovalPath::SelfLeave",
            "reconcile_membership_change(",
            "finalize_self_leave_bounded(",
        ),
    ] {
        let body = method_body(source, signature);
        let dispatch = body
            .split_once("dispatch_removal_outcome(")
            .unwrap_or_else(|| panic!("{signature} dispatches its outcome"))
            .1;
        assert_eq!(
            body.matches("dispatch_removal_outcome(").count(),
            1,
            "{signature}"
        );
        assert!(
            dispatch.trim_start().starts_with(&format!("{path},")),
            "{signature} binds {path}"
        );
        assert!(
            dispatch.contains(reconcile),
            "{signature} reconciles via {reconcile}"
        );
        assert!(
            dispatch.contains(finalize),
            "{signature} finalizes via {finalize}"
        );
        for other in [
            "RemovalPath::Learner",
            "RemovalPath::Voter",
            "RemovalPath::SelfLeave",
        ] {
            assert_eq!(body.contains(other), other == path, "{signature}: {other}");
        }
    }
    let learner = method_body(source, "async fn remove_learner_captured(");
    assert!(!learner.contains("reconcile_membership_change("));
    assert!(!learner.contains("finalize_self_leave_bounded("));
    let voter = method_body(source, "async fn remove_voter_captured(");
    assert!(!voter.contains("reconcile_member_removal("));
    assert!(!voter.contains("finalize_self_leave_bounded("));
    let leave = method_body(source, "async fn leave_voter_captured(");
    assert!(!leave.contains("reconcile_member_removal("));
    assert!(leave.contains("inner.identity.raft_id,"));
}

/// A self-leave whose final tombstone write never completes still
/// succeeds once `FINAL_TOMBSTONE_WAIT` elapses; one that completes is
/// awaited. The pending-removal row, not the tombstone, is the fence.
#[tokio::test(start_paused = true)]
async fn self_leave_finalize_is_bounded_and_succeeds_on_timeout() {
    let started = tokio::time::Instant::now();
    let result = dispatch_removal_outcome(
        RemovalPath::SelfLeave,
        "node-a",
        "attempt-a",
        Ok(()),
        |_, _, error| async move { error },
        || async { MembershipChangeOutcome::Indeterminate },
        |node| finalize_self_leave_bounded(node, std::future::pending::<()>()),
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(started.elapsed(), FINAL_TOMBSTONE_WAIT);

    let finalized = AtomicBool::new(false);
    let finalized = &finalized;
    finalize_self_leave_bounded("node-a", async move {
        finalized.store(true, Ordering::SeqCst);
    })
    .await;
    assert!(finalized.load(Ordering::SeqCst));
}

#[test]
fn removal_wire_request_is_a_node_delta() {
    assert_eq!(
        serde_json::to_value(RemoveVoterRequest { remove_voter: 7 })
            .expect("serialize removal delta"),
        serde_json::json!({"remove_voter": 7})
    );
}

#[test]
fn a_uniform_new_quorum_proves_the_target_was_removed() {
    let voters = BTreeSet::from([2, 3, 4]);
    assert!(quorum_confirms_removal(
        1,
        &[
            (2, Some(voters.clone())),
            (3, Some(voters.clone())),
            (1, Some(BTreeSet::from([1, 2, 3]))),
        ]
    ));
    assert!(!quorum_confirms_removal(
        1,
        &[
            (2, Some(voters.clone())),
            (3, Some(BTreeSet::from([2, 3, 5]))),
            (4, None),
        ]
    ));
    assert!(!quorum_confirms_removal(
        1,
        &[
            (2, Some(BTreeSet::from([1, 2, 3]))),
            (3, Some(BTreeSet::from([1, 2, 3]))),
        ]
    ));
}

#[test]
fn even_voter_leaders_commit_without_a_pre_handoff() {
    assert_eq!(
        leader_self_leave_sequence(3),
        LeaderSelfLeaveSequence::HandoffThenCommit
    );
    assert_eq!(
        leader_self_leave_sequence(4),
        LeaderSelfLeaveSequence::CommitDirectly
    );
    assert_eq!(
        leader_self_leave_sequence(5),
        LeaderSelfLeaveSequence::HandoffThenCommit
    );
    assert_eq!(
        leader_self_leave_sequence(6),
        LeaderSelfLeaveSequence::CommitDirectly
    );
}

/// Seed a three-voter cluster whose nodes have all heartbeated once, on an
/// unactivated `cluster_meta` range. `ready` names the nodes whose current
/// binary proved the learner protocol.
fn protocol_fixture(ready: &[&str]) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_meta (\
                   singleton INTEGER PRIMARY KEY, schema_version INTEGER, \
                   protocol_min INTEGER, protocol_max INTEGER, migrated_at INTEGER); \
                 CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, raft_id INTEGER, \
                   last_seen_at INTEGER, removed_at INTEGER, role TEXT); \
                 CREATE TABLE cluster_node_capabilities (\
                   node_id TEXT, capability TEXT, last_seen_at INTEGER, \
                   PRIMARY KEY(node_id, capability)); \
                 CREATE TABLE cluster_node_heartbeat_intents (\
                   node_id TEXT PRIMARY KEY, last_seen_at INTEGER); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_join_tokens (node_id TEXT, state TEXT); \
                 CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY, started_at INT); \
                 CREATE TABLE cluster_node_removal_attempts (\
                   node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, \
                   PRIMARY KEY(node_id, attempt_id)); \
                 CREATE TABLE cluster_cache_admin_revocation_leases (\
                   singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
                   node_id TEXT NOT NULL, claim_id TEXT NOT NULL UNIQUE, \
                   expires_at INTEGER NOT NULL, \
                   retain_release_receipt INTEGER NOT NULL); \
                 CREATE TABLE cluster_credential_guard_activation (\
                   singleton INTEGER PRIMARY KEY CHECK (singleton = 1)); \
                 INSERT INTO cluster_meta VALUES (1, 11, 4, 4, 0); \
                 INSERT INTO cluster_nodes VALUES ('node-a', 1, 100, NULL, 'voter'); \
                 INSERT INTO cluster_nodes VALUES ('node-b', 2, 200, NULL, NULL); \
                 INSERT INTO cluster_nodes VALUES ('node-c', 3, 300, NULL, 'voter');",
        )
        .expect("seed a three-voter cluster on the unactivated range");
    for node_id in ready {
        connection
            .execute(
                "INSERT INTO cluster_node_capabilities (node_id, capability, last_seen_at) \
                     SELECT node_id, ?2, last_seen_at FROM cluster_nodes WHERE node_id = ?1",
                rusqlite::params![node_id, LEARNER_PROTOCOL_CAPABILITY],
            )
            .expect("couple the capability to that node's current heartbeat");
    }
    connection
}

fn active_range(connection: &rusqlite::Connection) -> (i64, i64) {
    connection
        .query_row(
            "SELECT protocol_min, protocol_max FROM cluster_meta WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read the active protocol range")
}

fn unready_nodes(connection: &rusqlite::Connection) -> Vec<String> {
    let sql = capability_unready_nodes_sql(LEARNER_PROTOCOL_CAPABILITY);
    let mut statement = connection
        .prepare(&sql)
        .expect("prepare the unready roster");
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("run the unready roster")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect the unready roster");
    rows
}

/// Every node in [`protocol_fixture`] heartbeated at or before 300, so a
/// cutoff below that treats all of them as present. Tests that want the
/// absence rule to bite pass their own.
const FIXTURE_PRESENT_CUTOFF: i64 = 1;

fn activate_at(connection: &rusqlite::Connection, cutoff: i64) -> usize {
    connection
        .execute(
            &narrow_protocol_range_sql(Some(activation_guard_predicate())),
            rusqlite::params![AUTH_LEARNER_PROTOCOL, AUTH_PROTOCOL_MIN, cutoff],
        )
        .expect("run the activation statement")
}

fn activate(connection: &rusqlite::Connection) -> usize {
    activate_at(connection, FIXTURE_PRESENT_CUTOFF)
}

fn deactivate(connection: &rusqlite::Connection) -> usize {
    connection
        .execute(
            &narrow_protocol_range_sql(Some(no_admitted_learner_predicate().to_owned())),
            rusqlite::params![AUTH_PROTOCOL_MIN, AUTH_LEARNER_PROTOCOL],
        )
        .expect("run the deactivation statement")
}

fn absent_nodes(connection: &rusqlite::Connection, cutoff: i64) -> Vec<String> {
    let mut statement = connection
        .prepare(absent_nodes_sql())
        .expect("prepare the absent roster");
    let rows = statement
        .query_map(rusqlite::params![cutoff], |row| row.get::<_, String>(0))
        .expect("run the absent roster")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect the absent roster");
    rows
}

fn joining_nodes(connection: &rusqlite::Connection) -> Vec<String> {
    let mut statement = connection
        .prepare(join_in_flight_nodes_sql())
        .expect("prepare the join-in-flight roster");
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("run the join-in-flight roster")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect the join-in-flight roster");
    rows
}

fn admitted_learners(connection: &rusqlite::Connection) -> Vec<String> {
    let mut statement = connection
        .prepare(admitted_learner_nodes_sql())
        .expect("prepare the learner roster");
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("run the learner roster")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect the learner roster");
    rows
}

/// The role column is additive and nullable, so every row an older
/// coordinator wrote reads back as the voter it is. Only an explicit
/// `'learner'` names a node this cluster admitted under protocol 5.
#[test]
fn a_membership_row_without_a_role_is_a_voter() {
    let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
    assert!(
        admitted_learners(&connection).is_empty(),
        "a NULL role must not be read as a learner"
    );
    assert_eq!(
        ClusterRole::from_stored(None).expect("null role"),
        ClusterRole::Voter
    );
    assert_eq!(
        ClusterRole::from_stored(Some("voter")).expect("voter role"),
        ClusterRole::Voter
    );
    assert_eq!(
        ClusterRole::from_stored(Some("learner")).expect("learner role"),
        ClusterRole::Learner
    );
    // Not a guess and not a default: a value this binary does not
    // understand is a state it must not act on.
    assert!(ClusterRole::from_stored(Some("observer")).is_err());
}

/// The refusal that keeps deactivation from stranding a learner rides in
/// the committing statement, not only in a preflight an admission can win
/// a race against.
#[test]
fn deactivation_is_refused_by_its_own_statement_while_a_learner_is_admitted() {
    let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
    assert_eq!(activate(&connection), 1);

    connection
        .execute(
            "INSERT INTO cluster_nodes VALUES ('node-learner', 4, 400, NULL, 'learner')",
            [],
        )
        .expect("admit a learner");
    assert_eq!(
        admitted_learners(&connection),
        vec!["node-learner".to_owned()]
    );
    assert_eq!(
        deactivate(&connection),
        0,
        "an admitted learner blocks deactivation"
    );
    assert_eq!(active_range(&connection), (5, 5));

    // Removal tombstones the row rather than deleting it, so the guard has
    // to read `removed_at` too — otherwise a cluster could never roll back
    // once it had ever admitted a learner.
    connection
        .execute(
            "UPDATE cluster_nodes SET removed_at = 500 WHERE node_id = 'node-learner'",
            [],
        )
        .expect("remove the learner");
    assert!(admitted_learners(&connection).is_empty());
    assert_eq!(
        deactivate(&connection),
        1,
        "a removed learner strands nothing"
    );
    assert_eq!(active_range(&connection), (4, 4));
}

/// The committing statement — not just the preflight — carries the
/// capability precondition, and it is idempotent in both directions.
#[test]
fn change_refuses_unbounded_clock() {
    use crate::cluster::clock::{ClusterClockGuard, PeerClockOffset};

    let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
    let guard = ClusterClockGuard::new(true);
    // The operator-enabled guard; advisory mode never refuses this SQL.
    guard.set_enforced(true);
    let publish = |guard: &ClusterClockGuard| {
        let roster = guard.roster(&["node-b".to_owned(), "node-c".to_owned()]);
        assert!(guard.publish(
            roster,
            ["node-b", "node-c"]
                .into_iter()
                .map(|peer| (
                    peer.to_owned(),
                    PeerClockOffset::Bounded {
                        offset_us: 0,
                        uncertainty_us: 1_000,
                        observed_at: Instant::now(),
                    }
                ))
                .collect()
        ));
    };
    let submit = |prepared| -> Result<usize, MembershipError> {
        let admission = guard
            .admit_for(ClockDecision::MembershipChange, prepared)
            .map_err(MembershipError::ClockUnbounded)?;
        guard
            .revalidate_for(ClockDecision::MembershipChange, &admission)
            .map_err(MembershipError::ClockUnbounded)?;
        Ok(connection
            .execute(
                &narrow_protocol_range_sql(Some(activation_guard_predicate())),
                rusqlite::params![
                    AUTH_LEARNER_PROTOCOL,
                    AUTH_PROTOCOL_MIN,
                    admission
                        .now_ms()
                        .saturating_sub(PROTOCOL_CHANGE_ABSENCE_WINDOW_MS)
                ],
            )
            .expect("actual guarded protocol SQL"))
    };
    let unknown = submit(guard.acquire()).expect_err("unknown capture refuses SQL");
    assert_eq!(unknown.code(), "cluster_clock_unbounded");
    assert_eq!(active_range(&connection), (4, 4));
    publish(&guard);
    let before_read = guard.acquire();
    guard.roster_failed();
    publish(&guard);
    assert!(matches!(
        submit(before_read),
        Err(MembershipError::ClockUnbounded(
            ClockRefusal::GenerationChanged
        ))
    ));
    assert_eq!(active_range(&connection), (4, 4));
    let original = guard.acquire().expect("current bound");
    connection
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = $1",
            [original.now_ms()],
        )
        .expect("fresh fixture nodes");
    connection
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at = $1",
            [original.now_ms()],
        )
        .expect("fresh fixture capabilities");
    assert_eq!(
        submit(Ok(original)).expect("same proof and actual statement"),
        1
    );
    assert_eq!(active_range(&connection), (5, 5));

    // These are call-site wiring checks, not a claim of real Raft/fleet
    // acceptance. The preceding section executes the actual SQL primitive.
    let source = include_str!("../membership.rs");
    for signature in [
        "async fn redeem_for_role(",
        "pub async fn promote_learner(",
        "pub async fn activate_learner_protocol(",
    ] {
        let body = method_body(source, signature);
        assert!(
            body.find("let prepared_admission = clock.acquire();")
                .expect("entry capture")
                < body.find(".await").expect("awaited preparation")
        );
        if signature == "pub async fn promote_learner(" {
            // Coverage excuses an unobserved learner only while it holds
            // no vote; promoting one requires its own bounded clock.
            assert!(body.contains(".admit_promotion_for(target_raft_id, prepared_admission)"));
            assert!(body.contains(".revalidate_promotion_for(target_raft_id, &mut admission)"));
            assert!(!body.contains(".admit_for(") && !body.contains(".revalidate_for("));
            continue;
        }
        assert!(body.contains(".admit_for(ClockDecision::MembershipChange, prepared_admission)"));
        assert!(body.contains(".revalidate_for(ClockDecision::MembershipChange, &admission)"));
    }
    let redeem = method_body(source, "async fn redeem_for_role(");
    let repair = redeem
        .split_once("JoinEffect::RepairPublishedNode =>")
        .expect("repair")
        .1
        .split_once("JoinEffect::PublishStagedNode")
        .expect("new publication")
        .0;
    assert!(repair.contains("return Ok(())") && !repair.contains("admit_for"));
    assert!(
        redeem.find(".revalidate_for(").expect("fence")
            < redeem
                .find("inner.client.txn(statements).await")
                .expect("submission")
    );
    let promotion = method_body(source, "pub async fn promote_learner(");
    assert!(
        promotion
            .find("reconcile_promotion_change(")
            .expect("reconcile")
            < promotion
                .find(".admit_promotion_for(")
                .expect("new authority")
    );
    assert!(promotion.contains("let started_at = admission.now_ms();"));
    let activation = method_body(source, "pub async fn activate_learner_protocol(");
    assert!(
        activation
            .find("return Ok(ProtocolChange")
            .expect("already active")
            < activation.find(".admit_for(").expect("new activation")
    );
}

#[test]
fn activation_commits_once_and_only_with_every_voter_proven() {
    let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
    assert!(unready_nodes(&connection).is_empty());
    assert_eq!(active_range(&connection), (4, 4));

    assert_eq!(activate(&connection), 1, "a proven cluster activates");
    assert_eq!(active_range(&connection), (5, 5));

    // Idempotent: the second call matches no row precisely because the
    // range has already moved, so a retried request is not a second write.
    assert_eq!(activate(&connection), 0, "activation is idempotent");
    assert_eq!(active_range(&connection), (5, 5));

    assert_eq!(deactivate(&connection), 1, "the reversible case rolls back");
    assert_eq!(active_range(&connection), (4, 4));
    assert_eq!(deactivate(&connection), 0, "deactivation is idempotent");
    assert_eq!(active_range(&connection), (4, 4));
}

/// One voter that has never proven the capability blocks activation, and
/// the roster the refusal is built from names exactly that voter.
#[test]
fn one_unproven_voter_blocks_activation_and_is_named() {
    let connection = protocol_fixture(&["node-a", "node-c"]);
    assert_eq!(unready_nodes(&connection), vec!["node-b".to_owned()]);
    assert_eq!(activate(&connection), 0, "an unproven voter blocks it");
    assert_eq!(active_range(&connection), (4, 4));

    connection
        .execute(
            "INSERT INTO cluster_node_capabilities (node_id, capability, last_seen_at) \
                 SELECT node_id, ?2, last_seen_at FROM cluster_nodes WHERE node_id = ?1",
            rusqlite::params!["node-b", LEARNER_PROTOCOL_CAPABILITY],
        )
        .expect("the last voter heartbeats on the new binary");
    assert!(unready_nodes(&connection).is_empty());
    assert_eq!(activate(&connection), 1);
}

/// A node that redeemed a join token and has not yet proven anything
/// blocks activation, and is named as *joining* rather than as behind.
///
/// This is the lockout: `redeem` publishes the staging row and the
/// `cluster_nodes` row before the joiner has started Raft or run a single
/// compatibility check, so a node that redeems on an older binary and is
/// then interrupted is a committed voter with no capability row that the
/// pending roster deliberately cannot see. Activating past it leaves a
/// voter that counts for quorum and can never open its store again — on a
/// 3→4 growth that takes fault tolerance to zero.
///
/// The roster stays honest — a mid-join node is genuinely not "behind" —
/// and the *commit* is what refuses.
#[test]
fn a_join_in_flight_blocks_activation_without_making_the_pending_roster_lie() {
    let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
    connection
        .execute_batch(
            "INSERT INTO cluster_nodes VALUES ('node-joining', 4, 300, NULL, 'voter'); \
                 INSERT INTO cluster_node_join_staging VALUES ('node-joining');",
        )
        .expect("redeem a join token for a fourth node");

    assert!(
        unready_nodes(&connection).is_empty(),
        "a mid-join node has not heartbeated, so it is not 'behind'"
    );
    assert_eq!(joining_nodes(&connection), vec!["node-joining".to_owned()]);
    assert_eq!(
        activate(&connection),
        0,
        "the committing statement refuses while a join is in flight"
    );
    assert_eq!(active_range(&connection), (4, 4));

    // The joiner's first heartbeat clears the staging row; that is the
    // moment it becomes visible to the capability rule instead.
    connection
        .execute(
            "DELETE FROM cluster_node_join_staging WHERE node_id = 'node-joining'",
            [],
        )
        .expect("the joiner heartbeats");
    assert!(joining_nodes(&connection).is_empty());
    assert_eq!(
        unready_nodes(&connection),
        vec!["node-joining".to_owned()],
        "and now it is the capability rule that holds activation"
    );
    assert_eq!(activate(&connection), 0);

    connection
        .execute(
            "INSERT INTO cluster_node_capabilities (node_id, capability, last_seen_at) \
                 SELECT node_id, ?2, last_seen_at FROM cluster_nodes WHERE node_id = ?1",
            rusqlite::params!["node-joining", LEARNER_PROTOCOL_CAPABILITY],
        )
        .expect("the joined node proves the protocol");
    assert_eq!(activate(&connection), 1);
}

/// A join that is never coming back must not hold activation for good.
///
/// Two exits, because a refusal with no exit is a lockout of its own. The
/// operator can tombstone the node, and time can answer for them: a staged
/// node's `last_seen_at` is its redemption timestamp and stops advancing
/// the moment the join is abandoned, so it becomes *absent* — refused with
/// "start it or remove it" rather than "wait for the join".
#[test]
fn an_abandoned_join_stops_holding_activation() {
    let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
    connection
        .execute_batch(
            "INSERT INTO cluster_nodes VALUES ('node-abandoned', 4, 300, NULL, 'voter'); \
                 INSERT INTO cluster_node_join_staging VALUES ('node-abandoned');",
        )
        .expect("redeem a token for a node that never arrives");
    assert_eq!(activate(&connection), 0);

    // Exit one: time. Nothing about this node has advanced since it
    // redeemed, and the refusal that names it says what to do about that.
    connection
        .execute_batch(
            "UPDATE cluster_nodes SET last_seen_at = 400 WHERE node_id != 'node-abandoned'; \
                 UPDATE cluster_node_capabilities SET last_seen_at = 400;",
        )
        .expect("the three real voters keep heartbeating and proving");
    assert_eq!(
        absent_nodes(&connection, 301),
        vec!["node-abandoned".to_owned()]
    );
    assert_eq!(
        activate_at(&connection, 301),
        0,
        "still refused — but now as an absent node, not as a join to wait for"
    );

    // Exit two: the operator removes it. Neither rule names a tombstoned
    // node, and the leftover staging row does not resurrect it.
    connection
        .execute(
            "UPDATE cluster_nodes SET removed_at = 999 WHERE node_id = 'node-abandoned'",
            [],
        )
        .expect("tombstone the abandoned join");
    assert!(joining_nodes(&connection).is_empty());
    assert!(absent_nodes(&connection, 301).is_empty());
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_node_join_staging",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count staging rows"),
        1,
        "the staging row is still there; the tombstone is what stops it counting"
    );
    assert_eq!(activate(&connection), 1);
    assert_eq!(active_range(&connection), (5, 5));
}

/// A node that proved the capability and then stopped is not a proof.
///
/// The capability equality says which binary wrote the *last* heartbeat.
/// It says nothing about whether that node still exists, so a node that
/// was powered off after proving stays "ready" forever — and a binary
/// rolled back while the node is down never writes the desynchronising
/// heartbeat the whole rule leans on. It simply comes back up unable to
/// speak protocol 5 in a cluster that has already narrowed onto it.
///
/// A wall-clock window is a weak proof of presence. It is a perfectly good
/// proof of absence, and absence is all this has to establish.
#[test]
fn a_node_that_proved_the_protocol_and_then_stopped_blocks_activation() {
    let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
    // Every node is proven and the fixture's timestamps are all current:
    // the earliest heartbeat is 100, so a cutoff of 100 leaves nobody
    // strictly behind it.
    assert!(unready_nodes(&connection).is_empty());
    assert!(absent_nodes(&connection, 100).is_empty());

    // node-c last heartbeated at 300 and the others later; a cutoff of 301
    // is "node-c has said nothing for the whole window".
    connection
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = 400 WHERE node_id IN ('node-a','node-b')",
            [],
        )
        .expect("the two live nodes keep heartbeating");
    connection
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at = 400 \
                 WHERE node_id IN ('node-a','node-b')",
            [],
        )
        .expect("and keep proving the protocol as they do");

    assert_eq!(
        absent_nodes(&connection, 301),
        vec!["node-c".to_owned()],
        "the stopped node is named, so the refusal can say to start or remove it"
    );
    assert!(
        unready_nodes(&connection).is_empty(),
        "its capability row still looks like a proof; only the clock disproves it"
    );
    assert_eq!(
        activate_at(&connection, 301),
        0,
        "a member that has gone silent blocks the commit"
    );
    assert_eq!(active_range(&connection), (4, 4));

    // Start it again and it proves itself the ordinary way.
    connection
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = 500 WHERE node_id = 'node-c'",
            [],
        )
        .expect("node-c comes back");
    connection
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at = 500 \
                 WHERE node_id = 'node-c'",
            [],
        )
        .expect("on a binary that still knows the protocol");
    assert!(absent_nodes(&connection, 301).is_empty());
    assert_eq!(activate_at(&connection, 301), 1);
    assert_eq!(active_range(&connection), (5, 5));
}

/// Removing the node instead of starting it also unblocks activation, and
/// a tombstoned node never counts as absent.
#[test]
fn a_removed_node_is_not_an_absent_one() {
    let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
    connection
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = 1000 \
                 WHERE node_id IN ('node-a','node-b')",
            [],
        )
        .expect("the survivors keep heartbeating");
    connection
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at = 1000 \
                 WHERE node_id IN ('node-a','node-b')",
            [],
        )
        .expect("and keep proving the protocol");
    connection
        .execute(
            "UPDATE cluster_nodes SET removed_at = 999 WHERE node_id = 'node-c'",
            [],
        )
        .expect("remove the stopped node");
    assert!(
        absent_nodes(&connection, 1_000).is_empty(),
        "a removed node is nobody's problem, however long ago it spoke"
    );
    assert_eq!(activate_at(&connection, 1_000), 1);
}

/// The absence window is coupled to the heartbeat cadence rather than
/// picked, and is deliberately much wider than the reachability window a
/// human reads: this decides a one-way narrowing, not a health badge.
#[test]
fn the_absence_window_is_a_stated_multiple_of_the_heartbeat_interval() {
    assert_eq!(
        PROTOCOL_CHANGE_ABSENCE_WINDOW_MS as u128,
        HEARTBEAT_INTERVAL.as_millis() * 12
    );
    const {
        assert!(
            PROTOCOL_CHANGE_ABSENCE_WINDOW_MS > NODE_REACHABLE_WINDOW_MS,
            "a node that is merely unreachable must not block activation"
        );
    }
}

/// An old staged learner redemption still blocks rollback.
///
/// `redeem` commits `role = 'learner'` before the joiner has joined Raft
/// at all, so a redemption that then failed — port conflict, crash,
/// operator ^C — leaves a row for an authorized node that may resume.
///
/// A staging row plus age is not proof of abandonment: the authorized
/// process may be paused after redemption and resume after any wall-clock
/// cutoff. Until an explicit learner removal atomically cancels and
/// tombstones the admission, rollback must fail closed.
#[test]
fn a_staged_learner_redemption_blocks_rollback_regardless_of_age() {
    let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
    assert_eq!(activate(&connection), 1);

    connection
        .execute_batch(
            "INSERT INTO cluster_nodes VALUES ('node-abandoned', 4, 400, NULL, 'learner'); \
                 INSERT INTO cluster_node_join_staging VALUES ('node-abandoned');",
        )
        .expect("redeem a learner token for a node that never arrives");

    assert_eq!(
        admitted_learners(&connection),
        vec!["node-abandoned".to_owned()],
        "the durable admission is protected"
    );
    assert_eq!(deactivate(&connection), 0);
    assert_eq!(active_range(&connection), (5, 5));

    connection
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = -999999999 \
                 WHERE node_id = 'node-abandoned'",
            [],
        )
        .expect("make the staged admission arbitrarily old");
    assert_eq!(admitted_learners(&connection), vec!["node-abandoned"]);
    assert_eq!(deactivate(&connection), 0);
    assert_eq!(active_range(&connection), (5, 5));
}

/// A learner that actually joined keeps blocking rollback however long it
/// has been quiet, because it is a real member holding real state.
#[test]
fn a_learner_that_finished_joining_blocks_rollback_however_stale_it_is() {
    let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
    assert_eq!(activate(&connection), 1);
    connection
        .execute(
            "INSERT INTO cluster_nodes VALUES ('node-learner', 4, 400, NULL, 'learner')",
            [],
        )
        .expect("admit a learner whose first heartbeat cleared its staging row");
    assert_eq!(
        admitted_learners(&connection),
        vec!["node-learner".to_owned()],
        "no staging row means the admission finished; staleness is irrelevant"
    );
    assert_eq!(deactivate(&connection), 0);
    assert_eq!(active_range(&connection), (5, 5));
}

/// The additive columns land together, because the window between them
/// breaks the whole join surface rather than one statement in it.
///
/// Both `ALTER`s were separate Raft commands. Between them
/// `cluster_nodes.role` existed and `cluster_join_tokens.role` did not —
/// and every statement on the join surface names both, so for the length
/// of that window nothing on it could even *prepare*: SQLite reports `no
/// such column: role` at preparation, not at execution.
///
/// This reconstructs the half-applied state and shows that the surface is
/// unusable in it, that the probe finds exactly the missing column, and
/// that applying the remainder as one statement list closes it.
#[test]
fn a_half_applied_additive_column_breaks_the_join_surface_it_is_not_on() {
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, last_seen_at INTEGER); \
                 CREATE TABLE cluster_join_tokens (token_hash TEXT PRIMARY KEY, node_id TEXT);",
        )
        .expect("seed the pre-upgrade tables");

    let missing = |connection: &rusqlite::Connection| {
        MEMBERSHIP_ADDITIVE_COLUMNS
            .iter()
            .filter(|column| {
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
                        rusqlite::params![column.table, column.column],
                        |row| row.get::<_, i64>(0),
                    )
                    .expect("probe the column")
                    == 0
            })
            .map(|column| format!("{}.{}", column.table, column.column))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        missing(&connection),
        vec![
            "cluster_nodes.role".to_owned(),
            "cluster_join_tokens.role".to_owned()
        ],
        "the probe has to be able to see both, or it cannot decide anything"
    );

    // The half-applied state: statement [0] committed, [1] did not.
    connection
        .execute(MEMBERSHIP_ADDITIVE_COLUMNS[0].statement, [])
        .expect("apply only the first column");
    assert_eq!(
        missing(&connection),
        vec!["cluster_join_tokens.role".to_owned()],
        "the probe names exactly what is left, so the remainder is applyable"
    );

    // And the surface is not merely degraded — it will not prepare.
    let refused = connection
        .prepare("SELECT raft_id, expires_at, state, node_id, role FROM cluster_join_tokens")
        .expect_err("the token read must not prepare against a half-applied schema")
        .to_string();
    assert!(refused.contains("role"), "{refused}");

    // Applying the remainder — which is what one transaction does — closes
    // it, and the probe is then satisfied.
    for column in MEMBERSHIP_ADDITIVE_COLUMNS.iter().skip(1) {
        connection
            .execute(column.statement, [])
            .expect("apply the rest in one go");
    }
    assert!(missing(&connection).is_empty());
    connection
        .prepare("SELECT node_id, role FROM cluster_join_tokens")
        .expect("the join surface prepares once every column exists");
}

/// The fail-closed branch of the job-authority decision: refuse, and say
/// so somewhere a machine can read.
///
/// `metrics_db()` borrows a local watch channel and cannot fail, so no real
/// daemon reaches this arm today and no fixture can force it. That is the
/// reason it needs both a named function and a counter rather than only a
/// log line: a counter at zero is the standing evidence that the arm is
/// still unreachable, and one that moves is a node quietly declining the
/// cluster's scheduled work.
#[test]
fn unreadable_membership_declines_cluster_work_and_is_counted() {
    let before = CLUSTER_JOB_AUTHORITY_UNREADABLE.load(Ordering::Relaxed);
    assert!(
        decide_cluster_job_authority(Ok(true)),
        "a committed voter runs the cluster's singleton work"
    );
    assert!(!decide_cluster_job_authority(Ok(false)));
    assert_eq!(
        CLUSTER_JOB_AUTHORITY_UNREADABLE.load(Ordering::Relaxed),
        before,
        "an answered question is not an unreadable one"
    );

    assert!(
        !decide_cluster_job_authority(Err(MembershipError::Unavailable)),
        "membership that cannot be read must fail closed"
    );
    assert_eq!(
        CLUSTER_JOB_AUTHORITY_UNREADABLE.load(Ordering::Relaxed),
        before + 1
    );
    assert!(prometheus_cluster_job_authority()
        .contains("plurx_cluster_job_authority_unreadable_total "));
}

/// `finalize`'s role assertion, in every state committed membership can
/// be in.
///
/// Both arms survived mutation before this existed. Weakening the learner
/// arm to `is_member` let a *voter* finalize a learner token; weakening
/// the voter arm to `is_member` let a voter finalize while it was still
/// Raft's intermediate learner — before it had the vote the cluster was
/// already counting on. Neither state is reachable from a one-voter
/// replicated fixture, which is exactly why both survived: the live test
/// only ever produces the two states that pass.
#[test]
fn finalizing_requires_the_membership_the_token_actually_admits() {
    // (is_member, is_voter) → what each role may finalize on.
    let cases = [
        // Not in committed membership at all: nothing finalizes.
        (false, false, false, false),
        // A committed member with no vote: a learner, and only a learner.
        (true, false, false, true),
        // A committed voter: a voter, and only a voter. `is_member` is
        // true here too, which is what made the weakened arms pass.
        (true, true, true, false),
    ];
    for (is_member, is_voter, voter_ok, learner_ok) in cases {
        assert_eq!(
            role_is_admitted(ClusterRole::Voter, is_member, is_voter),
            voter_ok,
            "voter token with member={is_member} voter={is_voter}"
        );
        assert_eq!(
            role_is_admitted(ClusterRole::Learner, is_member, is_voter),
            learner_ok,
            "learner token with member={is_member} voter={is_voter}"
        );
    }

    // Stated as the two mutations rather than only as a table, so the
    // failure message names what went wrong.
    assert!(
        !role_is_admitted(ClusterRole::Voter, true, false),
        "a voter must not finalize while it is still Raft's intermediate \
             learner and carries no vote"
    );
    assert!(
        !role_is_admitted(ClusterRole::Learner, true, true),
        "a node in the voter set was not admitted by the learner protocol"
    );
}

#[test]
fn a_transient_joining_voter_is_not_reported_as_read_worker_capacity() {
    assert!(counts_as_ready_read_worker(&NodeRole::Learner, false, true));
    assert!(
        !counts_as_ready_read_worker(&NodeRole::Voter, false, true),
        "a voter-token join is only transiently non-voting"
    );
    assert!(!counts_as_ready_read_worker(&NodeRole::Learner, true, true));
}

/// The learner operations act on the learner protocol, named as itself.
///
/// `AUTH_PROTOCOL_MAX` and `AUTH_LEARNER_PROTOCOL` are the same number
/// today, and `AUTH_LEARNER_PROTOCOL` exists precisely because the first
/// moves with every future protocol and the second must not. No
/// behavioural test can tell them apart while they agree, so the call
/// sites are pinned here instead: the day protocol 6 lands, an
/// `AUTH_PROTOCOL_MAX` left in any of these would silently retarget
/// activation, deactivation, and the status projection at it.
#[test]
fn the_learner_operations_name_the_learner_protocol_not_the_newest_one() {
    let source = production_source();

    let projection = method_body(&source, "async fn protocol_projection(");
    assert!(
            projection.contains("learner_protocol_active: (active_min, active_max)\n                == (AUTH_LEARNER_PROTOCOL, AUTH_LEARNER_PROTOCOL)"),
            "the status projection must compare against the learner protocol: {projection}"
        );
    // `binary_max` genuinely is "the newest protocol this build knows".
    assert!(projection.contains("binary_max: AUTH_PROTOCOL_MAX"));

    for (name, body) in [
        (
            "activate_learner_protocol",
            method_body(&source, "pub async fn activate_learner_protocol("),
        ),
        (
            "deactivate_learner_protocol",
            method_body(&source, "pub async fn deactivate_learner_protocol("),
        ),
    ] {
        assert!(
            !body.contains("AUTH_PROTOCOL_MAX"),
            "{name} must name AUTH_LEARNER_PROTOCOL, not the moving maximum: {body}"
        );
        assert!(body.contains("AUTH_LEARNER_PROTOCOL"), "{name}: {body}");
    }
}

/// A member with no vote is not "not found".
///
/// `DELETE /cluster/nodes/{id}` used to fall through to `NodeNotFound` and
/// answer 404 for a node the roster lists — which reads as "you typed the
/// wrong id" for a node the operator is looking straight at. The
/// voter-only compatibility method keeps a typed refusal; the public
/// `remove_node` dispatcher now selects learner removal instead.
#[test]
fn removing_a_member_with_no_vote_is_refused_by_name_rather_than_as_not_found() {
    let refusal = MembershipError::NonVoterRemovalUnsupported("node-learner".to_owned());
    assert_eq!(refusal.code(), "cluster_non_voter_removal_unsupported");
    assert_ne!(refusal.code(), MembershipError::NodeNotFound.code());
    assert!(refusal.to_string().contains("node-learner"), "{refusal}");
}

#[test]
fn learner_removal_uses_the_shipped_hiqlite_delete_route() {
    let request = production_source()
        .split_once("async fn request_learner_removal(")
        .expect("learner removal request")
        .1
        .split_once("fn membership_response_failure(")
        .expect("end of learner removal request")
        .0
        .to_owned();

    assert!(
        request.contains(".delete(format!(\"https://{leader_api}/cluster/membership/sqlite\"))")
    );
    assert!(!request.contains("/cluster/leave/"));
}

/// A join refused by the cluster's own rules is not a migration failure.
///
/// The refusal reached operators as `schema migration failed:
/// learner_protocol_inactive: …`, which points at the database when the
/// actual answer is "run the activation command".
#[test]
fn a_join_the_cluster_refuses_is_not_reported_as_a_migration_failure() {
    let refused = crate::error::StoreError::JoinRefused(
        "learner_protocol_inactive: activate it first".to_owned(),
    )
    .to_string();
    assert!(
        !refused.contains("migration"),
        "a policy refusal must not be dressed as a schema failure: {refused}"
    );
    assert!(refused.contains("learner_protocol_inactive"), "{refused}");
}

/// An admission is a compare-and-swap on the protocol range, not a read
/// followed by a hopeful write.
///
/// `redeem` reads the active range and refuses a learner on an unactivated
/// cluster, but there are at least two more linearizable round-trips
/// between that read and the admitting transaction —
/// `deactivate_learner_protocol` can narrow the range to `4..=4` in that
/// window and have the learner row commit anyway. That learner is admitted
/// into a cluster that has just rolled back and cannot restart until an
/// explicit removal. The code's own comment beside
/// `narrow_protocol_range_sql` states this principle; the join did not
/// apply it.
///
/// Both directions matter, so this is equality on min *and* max: a
/// protocol-4-only binary must not be admitted into a cluster that
/// activated while its request was in flight either.
#[test]
fn an_admission_is_rolled_back_when_the_protocol_range_moves_under_it() {
    let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
    let guarded = format!(
        "INSERT INTO cluster_nodes (node_id, raft_id, last_seen_at, removed_at, role) \
             SELECT 'node-joining', 4, 400, NULL, 'learner' WHERE {}",
        unchanged_protocol_range_predicate(1, 2)
    );
    let undo = || {
        connection
            .execute(
                "DELETE FROM cluster_nodes WHERE node_id = 'node-joining'",
                [],
            )
            .expect("undo the admission");
    };

    // Authorized against 4..=4, and the range has not moved.
    assert_eq!(
        connection
            .execute(&guarded, rusqlite::params![4_i64, 4_i64])
            .expect("run the guarded admission"),
        1
    );
    undo();

    // A concurrent activation narrowed it: this join was authorized
    // against a range that no longer exists.
    assert_eq!(activate(&connection), 1);
    assert_eq!(active_range(&connection), (5, 5));
    assert_eq!(
        connection
            .execute(&guarded, rusqlite::params![4_i64, 4_i64])
            .expect("run the guarded admission"),
        0,
        "an admission authorized against the old range must not commit"
    );

    // And the mirror case: authorized against 5..=5, deactivated under it.
    assert_eq!(
        connection
            .execute(&guarded, rusqlite::params![5_i64, 5_i64])
            .expect("run the guarded admission"),
        1
    );
    undo();
    assert_eq!(deactivate(&connection), 1);
    assert_eq!(
        connection
            .execute(&guarded, rusqlite::params![5_i64, 5_i64])
            .expect("run the guarded admission"),
        0,
        "a learner admitted into a rolled-back cluster is unrecoverable"
    );
}

/// Every statement that can publish a join carries the range guard.
///
/// The predicate above only helps where it is used, and `redeem` has three
/// admitting shapes — with a public HTTP origin, without one, and the
/// legacy partial-redemption resume. Each builds the *first* statement of
/// the transaction, which every later statement consumes, so the guard
/// belongs on all three or the transaction can half-commit.
#[test]
fn every_admitting_statement_carries_the_protocol_range_guard() {
    let source = production_source();
    let redeem = source
        .split_once("pub async fn redeem(")
        .expect("redeem")
        .1
        .split_once("async fn upsert_hostname(")
        .expect("the method after redeem")
        .0;
    assert_eq!(
        redeem
            .matches("unchanged_protocol_range_predicate(")
            .count(),
        3,
        "each of redeem's three admitting shapes must carry the range guard"
    );
}

/// The case the heartbeat coupling exists to catch, constructed on purpose.
///
/// A node was upgraded, wrote its capability, and was then rolled back to a
/// binary that still heartbeats but knows nothing about protocol 5. The
/// capability row survives with its old timestamp while `cluster_nodes`
/// moves on. Any rule that only asked "does a capability row exist?" would
/// activate onto a cluster containing a binary that cannot participate.
#[test]
fn a_capability_stranded_by_a_rollback_is_not_a_current_proof() {
    let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
    assert_eq!(
        activate(&connection),
        1,
        "the proven cluster would activate"
    );
    connection
        .execute(
            "UPDATE cluster_meta SET protocol_min = 4, protocol_max = 4",
            [],
        )
        .expect("rewind the fixture to the unactivated range");

    // The rolled-back binary heartbeats: cluster_nodes advances, the
    // protocol-5 capability row it does not know about does not.
    connection
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = last_seen_at + 1 \
                 WHERE node_id = 'node-b'",
            [],
        )
        .expect("old binary heartbeat");

    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_node_capabilities \
                     WHERE node_id = 'node-b' AND capability = ?1",
                rusqlite::params![LEARNER_PROTOCOL_CAPABILITY],
                |row| row.get::<_, i64>(0),
            )
            .expect("count the stranded capability row"),
        1,
        "the stale row is still present; only its timestamp disproves it"
    );
    assert_eq!(unready_nodes(&connection), vec!["node-b".to_owned()]);
    assert_eq!(
        activate(&connection),
        0,
        "a capability older than its node's heartbeat is not a current proof"
    );
    assert_eq!(active_range(&connection), (4, 4));
}

#[test]
fn cache_revocation_capability_covers_the_exact_committed_roster_and_rollback() {
    let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
    for node_id in ["node-a", "node-b", "node-c"] {
        connection
            .execute(
                "INSERT INTO cluster_node_capabilities (node_id, capability, last_seen_at) \
                     SELECT node_id, ?2, last_seen_at FROM cluster_nodes WHERE node_id = ?1",
                rusqlite::params![node_id, CACHE_ADMIN_REVOCATION_CAPABILITY],
            )
            .expect("write cache revocation capability with the heartbeat");
    }
    let ready = |members: &str| {
        connection
            .query_row(
                &committed_capability_ready_sql(CACHE_ADMIN_REVOCATION_CAPABILITY),
                rusqlite::params![members],
                |row| row.get::<_, i64>(0),
            )
            .expect("read exact committed capability verdict")
            == 0
    };

    assert!(ready("[1,2,3]"));
    assert!(
        !ready("[1,2,3,4]"),
        "a committed member with no applied identity row is not silently omitted"
    );

    connection
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = last_seen_at + 1 \
                 WHERE node_id = 'node-b'",
            [],
        )
        .expect("rolled-back binary heartbeat");
    assert!(
        !ready("[1,2,3]"),
        "an older binary advancing its heartbeat invalidates the proof"
    );
    assert!(
        ready("[1,3]"),
        "a member no longer in the committed Raft configuration cannot hold the gate closed"
    );
}

#[test]
fn active_cache_revocation_exclusion_blocks_readiness_without_wall_clock_expiry() {
    let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
    for node_id in ["node-a", "node-b", "node-c"] {
        connection
            .execute(
                "INSERT INTO cluster_node_capabilities (node_id, capability, last_seen_at) \
                     SELECT node_id, ?2, last_seen_at FROM cluster_nodes WHERE node_id = ?1",
                rusqlite::params![node_id, CACHE_ADMIN_REVOCATION_CAPABILITY],
            )
            .expect("write cache revocation capability with the heartbeat");
    }
    connection
        .execute(
            "INSERT INTO cluster_credential_guard_activation VALUES (1)",
            [],
        )
        .expect("activate the fully upgraded roster");
    let ready = || {
        connection
            .query_row(
                &committed_cache_admin_revocation_ready_sql(),
                rusqlite::params!["[1,2,3]"],
                |row| row.get::<_, i64>(0),
            )
            .expect("read cache revocation readiness")
            == 0
    };
    assert!(ready());

    connection
        .execute(
            "INSERT INTO cluster_cache_admin_revocation_leases \
                 (singleton, node_id, claim_id, expires_at, retain_release_receipt) \
                 VALUES (1, 'node-a', 'delayed-store', -9223372036854775808, 1)",
            [],
        )
        .expect("install an exclusion older than any possible local clock");
    assert!(
        !ready(),
        "restart readiness must remain closed while the row can authorize delayed Store"
    );
    connection
        .execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, password_hash TEXT); \
                            INSERT INTO users VALUES (1, 'old');",
        )
        .expect("seed delayed credential mutation target");
    assert_eq!(
        connection
            .execute(
                "UPDATE users SET password_hash = 'committed' \
                     WHERE id = 1 AND EXISTS (\
                       SELECT 1 FROM cluster_cache_admin_revocation_leases \
                       WHERE claim_id = 'delayed-store')",
                [],
            )
            .expect("run claim-authorized delayed Store mutation"),
        1,
        "wall-clock expiry alone does not revoke the Store claim"
    );

    connection
        .execute(
            "DELETE FROM cluster_cache_admin_revocation_leases \
                 WHERE claim_id = 'delayed-store'",
            [],
        )
        .expect("linearize exact exclusion release");
    assert!(
        ready(),
        "only replicated release, not elapsed local time or skew, reopens readiness"
    );
    assert_eq!(
        connection
            .execute(
                "UPDATE users SET password_hash = 'too-late' \
                     WHERE id = 1 AND EXISTS (\
                       SELECT 1 FROM cluster_cache_admin_revocation_leases \
                       WHERE claim_id = 'delayed-store')",
                [],
            )
            .expect("reject Store proposal after exact release"),
        0
    );
}

#[test]
fn cache_revocation_capability_keeps_a_joiner_closed_until_self_is_committed() {
    assert!(!cache_admin_roster_admits_local(&BTreeSet::new(), 4));
    assert!(!cache_admin_roster_admits_local(
        &[1_u64, 2, 3].into_iter().collect(),
        4,
    ));
    assert!(cache_admin_roster_admits_local(
        &[1_u64, 2, 3, 4].into_iter().collect(),
        4,
    ));
}

#[test]
fn rollback_heartbeat_cannot_republish_retired_cache_revocation_capabilities() {
    for (capability, insert_trigger, update_trigger) in [
        (
            LEGACY_CACHE_ADMIN_REVOCATION_V1_CAPABILITY,
            RETIRE_CACHE_ADMIN_V1_INSERT_SQL,
            RETIRE_CACHE_ADMIN_V1_UPDATE_SQL,
        ),
        (
            LEGACY_CACHE_ADMIN_REVOCATION_V2_CAPABILITY,
            RETIRE_CACHE_ADMIN_V2_INSERT_SQL,
            RETIRE_CACHE_ADMIN_V2_UPDATE_SQL,
        ),
    ] {
        let mut connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
        connection
            .execute_batch(
                "CREATE TABLE cluster_nodes (\
                       node_id TEXT PRIMARY KEY, last_seen_at INTEGER NOT NULL); \
                     CREATE TABLE cluster_node_capabilities (\
                       node_id TEXT NOT NULL, capability TEXT NOT NULL, \
                       last_seen_at INTEGER NOT NULL, PRIMARY KEY(node_id, capability)); \
                     INSERT INTO cluster_nodes VALUES ('node-a', 100);",
            )
            .expect("construct previous capability state");
        assert_eq!(
            connection
                .execute(
                    "INSERT INTO cluster_node_capabilities VALUES ('node-a', ?1, 100)",
                    rusqlite::params![capability],
                )
                .expect("seed previous capability"),
            1
        );
        connection
            .execute_batch(insert_trigger)
            .expect("retire legacy insert");
        connection
            .execute_batch(update_trigger)
            .expect("retire legacy update");

        let transaction = connection.transaction().expect("old heartbeat transaction");
        transaction
            .execute(
                "UPDATE cluster_nodes SET last_seen_at = last_seen_at + 1 \
                     WHERE node_id = 'node-a'",
                [],
            )
            .expect("old heartbeat still advances node state");
        transaction
            .execute(
                "INSERT INTO cluster_node_capabilities (node_id, capability, last_seen_at) \
                     SELECT node_id, $2, last_seen_at FROM cluster_nodes WHERE node_id = $1 \
                     ON CONFLICT(node_id, capability) DO UPDATE SET \
                       last_seen_at = excluded.last_seen_at",
                rusqlite::params!["node-a", capability],
            )
            .expect("retired capability statement is an ignored no-op");
        transaction.commit().expect("old heartbeat commits");

        let (node_seen, capability_seen) = connection
            .query_row(
                "SELECT node.last_seen_at, capability.last_seen_at \
                     FROM cluster_nodes node JOIN cluster_node_capabilities capability \
                       ON capability.node_id = node.node_id \
                     WHERE node.node_id = 'node-a' AND capability.capability = $1",
                rusqlite::params![capability],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .expect("read rollback capability freshness");
        assert!(
            node_seen > capability_seen,
            "the rollback heartbeat remains live while {capability} stays stale"
        );
    }
}

#[test]
fn rollback_credential_mutation_is_rejected_before_readiness_refresh() {
    let mut connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON; \
                 CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, last_seen_at INTEGER NOT NULL); \
                 CREATE TABLE cluster_node_capabilities (\
                   node_id TEXT NOT NULL, capability TEXT NOT NULL, \
                   last_seen_at INTEGER NOT NULL, PRIMARY KEY(node_id, capability)); \
                 CREATE TABLE cluster_credential_mutation_intents (\
                   singleton INTEGER PRIMARY KEY CHECK(singleton = 1)); \
                 CREATE TABLE cluster_credential_guard_activation (\
                   singleton INTEGER PRIMARY KEY CHECK(singleton = 1)); \
                 CREATE TABLE users (\
                   id INTEGER PRIMARY KEY, password_hash TEXT NOT NULL, \
                   is_admin INTEGER NOT NULL); \
                 CREATE TABLE tokens (\
                   token_hash TEXT PRIMARY KEY, user_id INTEGER NOT NULL \
                     REFERENCES users(id) ON DELETE CASCADE); \
                 INSERT INTO cluster_nodes VALUES ('node-a', 100); \
                 INSERT INTO cluster_node_capabilities VALUES \
                   ('node-a', 'cache_admin_revocation_v4', 100); \
                 INSERT INTO cluster_credential_guard_activation VALUES (1); \
                 INSERT INTO users VALUES (1, 'old-hash', 1); \
                 INSERT INTO tokens VALUES ('old-token', 1);",
        )
        .expect("construct current credential state");
    for trigger in [
        RETIRE_CACHE_ADMIN_V2_INSERT_SQL,
        RETIRE_CACHE_ADMIN_V2_UPDATE_SQL,
        REQUIRE_CURRENT_CREDENTIAL_USER_UPDATE_SQL,
        REQUIRE_CURRENT_CREDENTIAL_USER_DELETE_SQL,
        REQUIRE_CURRENT_CREDENTIAL_TOKEN_INSERT_SQL,
        REQUIRE_CURRENT_CREDENTIAL_TOKEN_DELETE_SQL,
    ] {
        connection
            .execute_batch(trigger)
            .expect("install rollback credential guard");
    }

    let transaction = connection.transaction().expect("legacy heartbeat");
    transaction
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = 101 WHERE node_id = 'node-a'",
            [],
        )
        .expect("legacy heartbeat advances node state");
    transaction
        .execute(
            "INSERT INTO cluster_node_capabilities (node_id, capability, last_seen_at) \
                 VALUES ('node-a', 'cache_admin_revocation_v2', 101) \
                 ON CONFLICT(node_id, capability) DO UPDATE SET \
                   last_seen_at = excluded.last_seen_at",
            [],
        )
        .expect("retired capability write is ignored");
    transaction.commit().expect("legacy heartbeat commits");

    for (sql, label) in [
        (
            "UPDATE users SET password_hash = 'legacy-reset' WHERE id = 1",
            "password reset",
        ),
        (
            "UPDATE users SET is_admin = 0 WHERE id = 1",
            "admin mutation",
        ),
        (
            "INSERT INTO tokens VALUES ('legacy-token', 1)",
            "token creation",
        ),
        (
            "DELETE FROM tokens WHERE token_hash = 'old-token'",
            "token revocation",
        ),
        ("DELETE FROM users WHERE id = 1", "user deletion"),
    ] {
        let error = connection
            .execute(sql, [])
            .expect_err("legacy credential mutation must fail closed");
        assert!(
            error.to_string().contains("requires current binary"),
            "unexpected {label} rejection: {error}"
        );
    }

    let transaction = connection
        .transaction()
        .expect("current credential mutation");
    transaction
        .execute(
            "INSERT INTO cluster_credential_mutation_intents VALUES (1)",
            [],
        )
        .expect("publish current transaction intent");
    transaction
        .execute(
            "UPDATE users SET password_hash = 'current-reset' WHERE id = 1",
            [],
        )
        .expect("allow current credential mutation");
    transaction
        .execute(
            "DELETE FROM cluster_credential_mutation_intents WHERE singleton = 1",
            [],
        )
        .expect("consume current transaction intent");
    transaction.commit().expect("commit current mutation");
    assert_eq!(
        connection
            .query_row("SELECT password_hash FROM users WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            },)
            .expect("read current password"),
        "current-reset"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_credential_mutation_intents",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count transaction intents"),
        0
    );
}

#[test]
fn three_voter_rolling_upgrade_activates_credential_guard_only_after_full_roster() {
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON; \
                 CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, raft_id INTEGER NOT NULL UNIQUE, \
                   last_seen_at INTEGER NOT NULL, removed_at INTEGER); \
                 CREATE TABLE cluster_node_capabilities (\
                   node_id TEXT NOT NULL, capability TEXT NOT NULL, \
                   last_seen_at INTEGER NOT NULL, PRIMARY KEY(node_id, capability)); \
                 CREATE TABLE cluster_cache_admin_revocation_leases (\
                   singleton INTEGER PRIMARY KEY CHECK(singleton = 1), \
                   claim_id TEXT NOT NULL UNIQUE); \
                 CREATE TABLE cluster_credential_mutation_intents (\
                   singleton INTEGER PRIMARY KEY CHECK(singleton = 1)); \
                 CREATE TABLE cluster_credential_guard_activation (\
                   singleton INTEGER PRIMARY KEY CHECK(singleton = 1)); \
                 CREATE TABLE users (\
                   id INTEGER PRIMARY KEY, password_hash TEXT NOT NULL, \
                   is_admin INTEGER NOT NULL); \
                 CREATE TABLE tokens (\
                   token_hash TEXT PRIMARY KEY, user_id INTEGER NOT NULL \
                     REFERENCES users(id) ON DELETE CASCADE); \
                 INSERT INTO cluster_nodes VALUES \
                   ('node-a', 1, 100, NULL), \
                   ('node-b', 2, 100, NULL), \
                   ('node-c', 3, 100, NULL); \
                 INSERT INTO cluster_cache_admin_revocation_leases \
                   VALUES (1, 'activation-claim'); \
                 INSERT INTO users VALUES (1, 'old-hash', 1);",
        )
        .expect("construct three-voter rolling-upgrade state");
    for trigger in [
        REQUIRE_CURRENT_CREDENTIAL_USER_UPDATE_SQL,
        REQUIRE_CURRENT_CREDENTIAL_USER_DELETE_SQL,
        REQUIRE_CURRENT_CREDENTIAL_TOKEN_INSERT_SQL,
        REQUIRE_CURRENT_CREDENTIAL_TOKEN_DELETE_SQL,
    ] {
        connection
            .execute_batch(trigger)
            .expect("install credential guard");
    }

    let publish_capability = |node_id: &str| {
        connection
            .execute(
                "INSERT INTO cluster_node_capabilities \
                     SELECT node_id, ?2, last_seen_at FROM cluster_nodes WHERE node_id = ?1",
                rusqlite::params![node_id, CACHE_ADMIN_REVOCATION_CAPABILITY],
            )
            .expect("publish current v4 heartbeat capability");
    };
    let activate = || {
        connection
            .execute(
                &activate_credential_guard_sql(),
                rusqlite::params!["[1,2,3]", "activation-claim"],
            )
            .expect("attempt exact-roster guard activation")
    };
    let activation_count = || {
        connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_credential_guard_activation",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count guard activation")
    };

    connection
        .execute(
            "INSERT INTO cluster_node_capabilities \
                 SELECT node_id, 'cache_admin_revocation_v3', last_seen_at \
                 FROM cluster_nodes WHERE node_id = 'node-b'",
            [],
        )
        .expect("publish old unsigned-response capability");
    publish_capability("node-a");
    assert_eq!(
        connection
            .execute(
                &activate_credential_guard_sql(),
                rusqlite::params!["[1,2,3]", "stale-or-foreign-claim"],
            )
            .expect("reject a non-owned activation claim"),
        0
    );
    assert_eq!(
        activate(),
        0,
        "a current v3 peer must not satisfy the signed-response protocol"
    );
    assert_eq!(activation_count(), 0);
    assert_eq!(
        connection
            .execute("INSERT INTO tokens VALUES ('legacy-token', 1)", [])
            .expect("partial rollout keeps legacy login usable"),
        1
    );
    assert_eq!(
        connection
            .execute(
                "UPDATE users SET password_hash = 'legacy-reset' WHERE id = 1",
                [],
            )
            .expect("partial rollout keeps legacy password reset usable"),
        1
    );

    publish_capability("node-b");
    assert_eq!(activate(), 0);
    publish_capability("node-c");
    assert_eq!(activate(), 1);
    assert_eq!(activation_count(), 1);

    connection
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = 101 WHERE node_id = 'node-b'",
            [],
        )
        .expect("roll voter b back to a legacy heartbeat");
    connection
        .execute(
            "INSERT INTO cluster_node_capabilities \
                 VALUES ('node-b', 'cache_admin_revocation_v3', 101) \
                 ON CONFLICT(node_id, capability) DO UPDATE SET last_seen_at = 101",
            [],
        )
        .expect("v3 rollback publishes its own current capability");
    assert_eq!(activate(), 0, "activation is permanent and idempotent");
    for (sql, label) in [
        (
            "UPDATE users SET password_hash = 'rollback-reset' WHERE id = 1",
            "password reset",
        ),
        (
            "INSERT INTO tokens VALUES ('rollback-token', 1)",
            "login token creation",
        ),
        (
            "DELETE FROM tokens WHERE token_hash = 'legacy-token'",
            "logout token deletion",
        ),
    ] {
        let error = connection
            .execute(sql, [])
            .expect_err("post-activation rollback mutation must fail closed");
        assert!(
            error.to_string().contains("requires current binary"),
            "unexpected {label} rejection: {error}"
        );
    }
    assert_eq!(activation_count(), 1);
}

#[test]
fn activated_guard_with_live_mutation_lease_is_not_an_activation_candidate() {
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, raft_id INTEGER NOT NULL UNIQUE, \
                   last_seen_at INTEGER NOT NULL, removed_at INTEGER); \
                 CREATE TABLE cluster_node_capabilities (\
                   node_id TEXT NOT NULL, capability TEXT NOT NULL, \
                   last_seen_at INTEGER NOT NULL, PRIMARY KEY(node_id, capability)); \
                 CREATE TABLE cluster_cache_admin_revocation_leases (\
                   singleton INTEGER PRIMARY KEY CHECK(singleton = 1), \
                   claim_id TEXT NOT NULL UNIQUE); \
                 CREATE TABLE cluster_credential_guard_activation (\
                   singleton INTEGER PRIMARY KEY CHECK(singleton = 1)); \
                 INSERT INTO cluster_nodes VALUES \
                   ('node-a', 1, 100, NULL), ('node-b', 2, 100, NULL), \
                   ('node-c', 3, 100, NULL); \
                 INSERT INTO cluster_node_capabilities \
                   SELECT node_id, 'cache_admin_revocation_v4', last_seen_at \
                   FROM cluster_nodes; \
                 INSERT INTO cluster_credential_guard_activation VALUES (1); \
                 INSERT INTO cluster_cache_admin_revocation_leases \
                   VALUES (1, 'ordinary-credential-mutation');",
        )
        .expect("construct activated mutation state");
    let needed = connection
        .query_row(
            &committed_cache_admin_revocation_activation_needed_sql(),
            rusqlite::params!["[1,2,3]"],
            |row| row.get::<_, i64>(0),
        )
        .expect("read activation preflight");
    assert_eq!(
        needed, 0,
        "an ordinary active credential lease after activation must not cause reactivation"
    );
}

/// A binary old enough to predate heartbeat intents is caught one step
/// earlier: the replicated trigger drops every capability that node holds,
/// including the protocol-5 one, the moment it writes a heartbeat.
#[test]
fn a_pre_intent_binary_heartbeat_invalidates_the_learner_capability() {
    let connection = protocol_fixture(&["node-a", "node-b", "node-c"]);
    connection
        .execute_batch(MARK_LEGACY_NODE_HEARTBEAT_DURING_REMOVAL_SQL)
        .expect("install the replicated legacy heartbeat marker");
    assert!(unready_nodes(&connection).is_empty());

    connection
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = 999 WHERE node_id = 'node-c'",
            [],
        )
        .expect("a binary that writes no heartbeat intent");

    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_node_capabilities WHERE node_id = 'node-c'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count node-c capabilities"),
        0,
        "the trigger invalidates every capability the legacy writer's node held"
    );
    assert_eq!(unready_nodes(&connection), vec!["node-c".to_owned()]);
    assert_eq!(activate(&connection), 0);
}

// A mid-join node's effect on the pending roster *and* on the commit is
// pinned by `a_join_in_flight_blocks_activation_without_making_the_pending_
// roster_lie` above. The staging exclusion keeps the roster honest; it is
// deliberately no longer permission to activate.

/// The roster route answers during quorum loss; that is when an operator
/// most needs it. Adding the protocol block to it must not turn it into a
/// leader read, and the paths that commit a change must not become local
/// ones. The separate-process membership scenario proves the first half
/// end to end; this pins both halves where they are decided.
#[test]
fn the_roster_projection_reads_locally_and_the_decisions_do_not() {
    let source = production_source();

    let status = method_body(&source, "pub async fn status(&self)");
    assert!(
        !status.contains("query_consistent_map"),
        "the roster must stay readable without a quorum"
    );
    let projection = method_body(&source, "pub async fn protocol_status(&self)");
    assert!(
        projection.contains("Read::Local") && !projection.contains("Read::Quorum"),
        "the roster's protocol block must read the applied local state"
    );

    for decision in [
        method_body(&source, "pub async fn activate_learner_protocol("),
        method_body(&source, "pub async fn deactivate_learner_protocol("),
    ] {
        assert!(
            !decision.contains("Read::Local"),
            "a protocol change must never be decided from unapplied local state"
        );
        assert!(
            decision.contains("active_protocol_range") && decision.contains("Read::Quorum"),
            "a protocol change reads the range and its roster through the leader"
        );
    }
}

/// Deactivation must refuse before it can strand a member that only exists
/// under the protocol being removed.
///
/// This covers the half of the refusal that comes from committed Raft
/// membership, which still runs as the preflight that names what is in the
/// way — including a member whose durable row predates the role column.
/// The replicated half rides in the committing statement and is covered by
/// `deactivation_is_refused_by_its_own_statement_while_a_learner_is_admitted`;
/// the end-to-end version, against a real admitted learner, is in
/// `cluster::migration`.
#[test]
fn deactivation_is_refused_for_any_committed_member_without_a_vote() {
    let voters = BTreeSet::from([1_u64, 2, 3]);
    assert!(
        non_voting_members(&voters, [1_u64, 2, 3].into_iter()).is_empty(),
        "a voter-only cluster strands nothing"
    );
    assert_eq!(
        non_voting_members(&voters, [1_u64, 2, 3, 9, 7, 9].into_iter()),
        vec![7, 9],
        "every committed member without a vote is named, once, in order"
    );
    let refusal = MembershipError::LearnerProtocolInUse {
        admitted: Vec::new(),
        non_voting: vec!["7".to_owned(), "9".to_owned()],
    };
    assert_eq!(refusal.code(), "learner_protocol_in_use");
    assert!(refusal.to_string().contains("7, 9"), "{refusal}");
    assert_ne!(
        refusal.code(),
        MembershipError::LearnerProtocolUpgradeRequired(Vec::new()).code(),
        "an operator must be able to tell the two refusals apart"
    );
}

/// The two rosters name two different things, and the message has to keep
/// them apart.
///
/// The vendored client joins a voter with `add_learner` and only then
/// `become_member`, so an ordinary voter is a committed non-voter for a
/// moment. Unioning the rosters told the operator that voter had been
/// "admitted as a learner" and that they should remove it — advice that
/// would have destroyed a healthy join.
#[test]
fn a_mid_join_voter_is_not_reported_as_an_admitted_learner() {
    let mid_join = MembershipError::LearnerProtocolInUse {
        admitted: Vec::new(),
        non_voting: vec!["4".to_owned()],
    };
    let message = mid_join.to_string();
    assert!(
        !message.contains("admitted as learners"),
        "a mid-join voter must not be labelled an admitted learner: {message}"
    );
    assert!(message.contains("mid-join"), "{message}");
    assert!(message.contains('4'), "{message}");

    let learner = MembershipError::LearnerProtocolInUse {
        admitted: vec!["node-learner".to_owned()],
        non_voting: Vec::new(),
    };
    let message = learner.to_string();
    assert!(message.contains("admitted as learners"), "{message}");
    assert!(
        !message.contains("mid-join"),
        "an empty roster must leave no dangling label: {message}"
    );

    let both = MembershipError::LearnerProtocolInUse {
        admitted: vec!["node-learner".to_owned()],
        non_voting: vec!["4".to_owned()],
    };
    let message = both.to_string();
    assert!(message.contains("node-learner"), "{message}");
    assert!(message.contains("mid-join"), "{message}");
}

/// A lost compare-and-swap must not tell an operator to "upgrade these
/// nodes: []". When the re-read roster comes back empty, what actually
/// happened is that the range moved.
///
/// The variant is asserted first, and then the two call sites that have to
/// reach it. The call sites are pinned as text because the branch is a lost
/// race between two linearizable writes on one leader: there is no fixture
/// that loses it on demand, and asserting the variant alone left the whole
/// finding revertible — putting `LearnerProtocolUpgradeRequired(Vec::new())`
/// back kept every test green.
#[test]
fn a_lost_protocol_race_names_the_race_rather_than_an_empty_roster() {
    let refusal = MembershipError::ProtocolRangeChanged;
    assert_eq!(refusal.code(), "cluster_protocol_range_changed");
    let message = refusal.to_string();
    assert!(message.contains("retry"), "{message}");
    assert!(
        !message.contains("[]"),
        "an empty roster must never reach an operator: {message}"
    );

    let source = production_source();
    for (name, body) in [
        (
            "activate_learner_protocol",
            method_body(&source, "pub async fn activate_learner_protocol("),
        ),
        (
            "deactivate_learner_protocol",
            method_body(&source, "pub async fn deactivate_learner_protocol("),
        ),
    ] {
        assert!(
            body.contains("MembershipError::ProtocolRangeChanged"),
            "{name} must say the range moved when its re-read names nobody: {body}"
        );
        assert!(
            !body.contains("LearnerProtocolUpgradeRequired")
                && !body.contains("LearnerProtocolInUse"),
            "{name} must build its named refusals in the read-only pass, never inline with \
                 a roster it has not re-read: {body}"
        );
    }
}

/// Making the capability predicate generic must not have changed the
/// removal capability's meaning; this pins the removal predicate's text
/// against the literal that shipped before the refactor.
#[test]
fn the_generic_capability_predicate_preserves_the_removal_rule() {
    const SHIPPED: &str = "NOT EXISTS (SELECT 1 FROM cluster_nodes AS active \
       WHERE active.removed_at IS NULL \
         AND NOT EXISTS (SELECT 1 FROM cluster_node_join_staging AS staged \
           WHERE staged.node_id = active.node_id) \
         AND NOT EXISTS (SELECT 1 FROM cluster_node_capabilities AS capability \
           WHERE capability.node_id = active.node_id \
             AND capability.capability = 'membership_removal_attempt_refs_v1' \
             AND capability.last_seen_at = active.last_seen_at))";
    assert_eq!(
        capability_ready_predicate(REMOVAL_ATTEMPT_CAPABILITY),
        SHIPPED
    );
    assert_ne!(
        capability_ready_predicate(LEARNER_PROTOCOL_CAPABILITY),
        SHIPPED,
        "the two capabilities must be proven independently"
    );
}

/// The heartbeat is where the proof is made. If the capability write ever
/// leaves that transaction, an upgraded-then-rolled-back node can look
/// current, so pin the coupling itself.
#[test]
fn the_learner_capability_is_written_inside_the_heartbeat_transaction() {
    let commit_heartbeat = production_source()
        .split_once("async fn commit_heartbeat")
        .expect("commit_heartbeat")
        .1
        .split_once("\n    /// Publish the public half")
        .expect("end of commit_heartbeat")
        .0
        .to_owned();
    assert!(
        commit_heartbeat.contains("LEARNER_PROTOCOL_CAPABILITY"),
        "the learner capability must be written by the heartbeat itself"
    );
    assert!(
        commit_heartbeat.contains("CACHE_ADMIN_REVOCATION_CAPABILITY"),
        "cache revocation capability must be written by the heartbeat itself"
    );
    // One statement list, submitted once. The heartbeat builds its
    // statements before it submits them so the capability write can be
    // omitted by a validation build, but every statement it builds still
    // reaches the same single Raft transaction.
    assert_eq!(
        commit_heartbeat.matches(".txn(").count(),
        1,
        "and inside the heartbeat's single Raft transaction"
    );
    assert!(
        commit_heartbeat.contains(".txn(statements)"),
        "which must be the statement list the heartbeat just built"
    );
    assert_eq!(
        commit_heartbeat
            .matches("LEARNER_PROTOCOL_CAPABILITY")
            .count(),
        1,
        "exactly one place may stamp this capability with a heartbeat time"
    );
}

/// The heartbeat may skip its capability proof only in a build the daemon
/// never produces. A production `plurx-core` compiles a constant `false`
/// instead, so there is no environment variable to set and no branch to
/// take — which is what keeps the emulation from becoming a way for a real
/// node to look current while running an old binary.
#[test]
fn the_capability_proof_can_only_be_skipped_by_a_validation_build() {
    let source = production_source();
    assert_eq!(
        source
            .matches("fn emulate_pre_learner_protocol_heartbeat")
            .count(),
        2,
        "one gated definition and one production constant"
    );
    assert_eq!(
        source
            .matches("if !emulate_pre_learner_protocol_heartbeat() {")
            .count(),
        1,
        "and exactly one caller, in the heartbeat"
    );
    assert!(source.contains(
        "#[cfg(feature = \"cluster-validation\")]\nfn emulate_pre_learner_protocol_heartbeat("
    ));
    assert!(source.contains(
        "#[cfg(not(feature = \"cluster-validation\"))]\nconst fn \
             emulate_pre_learner_protocol_heartbeat() -> bool {\n    false\n}"
    ));
    assert_eq!(
        source
            .matches("PLURX_VALIDATION_PRE_LEARNER_HEARTBEAT")
            .count(),
        1,
        "the emulation is reachable only through its own gated definition"
    );
}

/// This module's own source, so a text-shape assertion cannot be satisfied
/// by the string literal that states it.
fn production_source() -> String {
    include_str!("../membership.rs")
        .split_once("\n#[cfg(test)]\nmod tests;")
        .expect("the test module")
        .0
        .to_owned()
}

/// An in-memory database holding the production declarations of the
/// replicated auth tables (`settings`, `users`, `tokens`) and of every
/// replicated membership table:
/// `MEMBERSHIP_SCHEMA`'s table statements plus its additive columns. A
/// SQL fixture built on it fails the way the deployed cluster does when a
/// statement names a column production does not have. Triggers are left
/// out because they need the coordinator's intent rows; a test that
/// exercises one installs it itself.
fn production_membership_tables() -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().expect("sqlite");
    connection
        .execute_batch(crate::store::replicated_auth_schema())
        .expect("replicated auth tables");
    for statement in MEMBERSHIP_SCHEMA
        .iter()
        .filter(|statement| statement.starts_with("CREATE TABLE"))
    {
        connection.execute(statement, []).expect("membership table");
    }
    for column in MEMBERSHIP_ADDITIVE_COLUMNS {
        connection
            .execute(column.statement, [])
            .expect("additive column");
    }
    connection
}

fn production_columns(connection: &rusqlite::Connection, table: &str) -> BTreeSet<String> {
    connection
        .prepare("SELECT name FROM pragma_table_info(?1)")
        .expect("table info")
        .query_map([table], |row| row.get(0))
        .expect("table columns")
        .collect::<Result<_, _>>()
        .expect("table column names")
}

/// A Rust string literal's value, from its opening quote: escapes and
/// `\`-newline continuations resolved the way rustc resolves them.
fn rust_string_literal(source: &str) -> Option<String> {
    let mut characters = source.strip_prefix('"')?.chars().peekable();
    let mut value = String::new();
    while let Some(character) = characters.next() {
        match character {
            '"' => return Some(value),
            '\\' => match characters.next()? {
                '\n' => {
                    while characters.peek().is_some_and(|next| next.is_whitespace()) {
                        characters.next();
                    }
                }
                'n' => value.push('\n'),
                't' => value.push('\t'),
                other => value.push(other),
            },
            other => value.push(other),
        }
    }
    None
}

/// The statement that fires `trigger`, for preparing it: SQLite compiles
/// a trigger's `WHEN` clause and body into each statement that fires it,
/// and resolves their names then, never when the trigger is created.
fn trigger_firing_statement(trigger: &str, connection: &rusqlite::Connection) -> String {
    let tokens = trigger
        .split_whitespace()
        .map(|token| token.trim_end_matches(','))
        .collect::<Vec<_>>();
    let event = tokens
        .iter()
        .position(|token| {
            ["INSERT", "UPDATE", "DELETE"].contains(&token.to_ascii_uppercase().as_str())
        })
        .unwrap_or_else(|| panic!("trigger without an event: {trigger}"));
    let on = event
        + tokens[event..]
            .iter()
            .position(|token| token.eq_ignore_ascii_case("ON"))
            .unwrap_or_else(|| panic!("trigger without a table: {trigger}"));
    let table = tokens[on + 1];
    match tokens[event].to_ascii_uppercase().as_str() {
        "INSERT" => format!("INSERT INTO {table} DEFAULT VALUES"),
        "DELETE" => format!("DELETE FROM {table}"),
        _ => {
            let column = if tokens[event + 1].eq_ignore_ascii_case("OF") {
                tokens[event + 2].to_owned()
            } else {
                production_columns(connection, table)
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| panic!("trigger on an unknown table: {trigger}"))
            };
            format!("UPDATE {table} SET {column} = {column}")
        }
    }
}

/// CLUSTER-DRILLS-2026-10-11 F2: every SQL constant this module defines
/// must compile against the production schema: the membership tables,
/// the replicated auth tables and `media_sessions`, all from their
/// production declarations. A reference read that selected
/// `node.last_applied_index` from `cluster_nodes` passed its unit test
/// (whose hand-built table had that column) while every voter removal on
/// the deployed schema answered `no such column`. A trigger is installed
/// on its own and the statement that fires it is prepared, because only
/// that resolves the trigger's `WHEN` clause and body.
#[test]
fn membership_sql_constants_compile_against_the_production_schema() {
    let connection = production_membership_tables();
    connection
        .execute_batch(crate::store::replicated_media_sessions_schema())
        .expect("replicated media sessions table");
    let source = production_source();
    let mut compiled = 0;
    let mut triggers = 0;
    for (offset, _) in source.match_indices("_SQL: &str =") {
        let line_start = source[..offset].rfind('\n').map_or(0, |at| at + 1);
        let name = source[line_start..offset]
            .rsplit(' ')
            .next()
            .expect("constant name")
            .to_owned()
            + "_SQL";
        let initializer = source[offset + "_SQL: &str =".len()..].trim_start();
        let Some(sql) = rust_string_literal(initializer) else {
            continue;
        };
        if sql.trim_start().starts_with("CREATE TRIGGER") {
            connection
                .execute_batch("SAVEPOINT trigger_check")
                .expect("savepoint");
            let fired = connection.execute_batch(&sql).and_then(|()| {
                connection
                    .prepare(&trigger_firing_statement(&sql, &connection))
                    .map(drop)
            });
            connection
                .execute_batch("ROLLBACK TO trigger_check; RELEASE trigger_check")
                .expect("roll back the trigger");
            if let Err(error) = fired {
                panic!("{name} does not compile on the production schema: {error}");
            }
            triggers += 1;
            continue;
        }
        if let Err(error) = connection.prepare(&sql) {
            panic!("{name} does not compile on the production schema: {error}");
        }
        compiled += 1;
    }
    assert!(
        compiled >= 30,
        "the constant scan found only {compiled} statements"
    );
    assert!(
        triggers >= 20,
        "the constant scan found only {triggers} triggers"
    );
}

/// The hand-built fixtures in this module may declare a subset of a
/// membership table's production columns, never a column production
/// does not have (the F2 fixture invented `cluster_nodes.last_applied_index`).
#[test]
fn membership_fixtures_declare_only_production_columns() {
    let production = production_membership_tables();
    assert!(include_str!("../membership.rs").contains("\n#[cfg(test)]\nmod tests;"));
    let tests = include_str!("tests.rs");
    let needle = concat!("CREATE TABLE ", "cluster_");
    let mut checked = 0;
    let mut rest = tests;
    while let Some(at) = rest.find(needle) {
        let declaration = &rest[at + "CREATE TABLE ".len()..];
        let open = declaration.find('(').expect("fixture column list");
        let table = declaration[..open].trim();
        let mut depth = 0usize;
        let mut close = None;
        for (index, character) in declaration[open..].char_indices() {
            match character {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close.expect("closed fixture column list");
        rest = &declaration[close..];
        let columns = production_columns(&production, table);
        if columns.is_empty() {
            // Another module's table (the sharing schema's, say).
            continue;
        }
        let body = declaration[open + 1..close].replace('\\', " ");
        let mut depth = 0usize;
        let mut definitions = vec![String::new()];
        for character in body.chars() {
            match character {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    definitions.push(String::new());
                    continue;
                }
                _ => {}
            }
            definitions.last_mut().expect("definition").push(character);
        }
        for definition in definitions {
            let Some(name) = definition.split_whitespace().next() else {
                continue;
            };
            if ["PRIMARY", "UNIQUE", "CHECK", "FOREIGN", "CONSTRAINT"]
                .contains(&name.to_ascii_uppercase().as_str())
            {
                continue;
            }
            assert!(
                columns.contains(name),
                "a fixture declares {table}.{name}, which production does not have"
            );
        }
        checked += 1;
    }
    assert!(checked >= 20, "the fixture scan found only {checked}");
}

/// CLUSTER-DRILLS-2026-10-11 F2: the applied index a removal proof waits
/// on is the target's heartbeat progress, joined from
/// `cluster_node_progress`. A target that has not published progress yet
/// is still bound but reads as unknown, never as applied.
#[test]
fn reduction_reference_reads_applied_index_from_heartbeat_progress() {
    use rusqlite::OptionalExtension;
    let node = "00000000-0000-0000-0000-000000000002";
    let attempt = "00000000-0000-0000-0000-000000000010";
    let reference = hiqlite::ReductionFenceReference {
        version: 1,
        target_node_id: node.into(),
        target_raft_id: 2,
        attempt_id: attempt.into(),
        barrier_index: 41,
    };
    let key = reduction_reference_key(&reference);
    let db = production_membership_tables();
    db.execute(
        "INSERT INTO cluster_nodes (node_id, raft_id, raft_address, api_address, last_seen_at) \
             VALUES (?1, 2, 'node-2:32401', 'node-2:32402', 100)",
        [node],
    )
    .expect("target");
    db.execute(
        "INSERT INTO cluster_node_removals (node_id, started_at) VALUES (?1, 100)",
        [node],
    )
    .expect("route fence");
    db.execute(
        "INSERT INTO cluster_node_removal_attempts (node_id, attempt_id) VALUES (?1, ?2)",
        [node, attempt],
    )
    .expect("owned reference");
    db.execute(
        "INSERT INTO settings (key, value, updated_at) VALUES (?1, '1', 100)",
        [removed_job_owner_key(node)],
    )
    .expect("owner fence");
    db.execute(
        "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, 100)",
        [
            key.clone(),
            serde_json::to_string(&reference).expect("json"),
        ],
    )
    .expect("frozen reference");
    let read = || {
        db.query_row(
            READ_REDUCTION_REFERENCE_SQL,
            rusqlite::params![key, node, attempt, removed_job_owner_key(node)],
            |row| row.get::<_, Option<i64>>("last_applied_index"),
        )
        .optional()
        .expect("the production read runs on the production schema")
    };
    assert_eq!(
        read(),
        Some(None),
        "a target without published progress is bound but not applied"
    );
    db.execute(
        "INSERT INTO cluster_node_progress (node_id, current_term, last_applied_index, \
               bounded_read_ready, voter_storage_ready, voter_role_persisted, observed_at) \
             VALUES (?1, 1, 41, 1, 1, 1, 100)",
        [node],
    )
    .expect("target progress");
    assert_eq!(read(), Some(Some(41)));
}

/// The cluster-wide exclusion row and two members, on the production
/// tables, with a counter standing in for this replica's applied index:
/// every write below is one applied entry. One database is every replica
/// at once, so the catch-up before the refusal read is immediate.
struct SqlCacheAdminSlot {
    db: Arc<Mutex<rusqlite::Connection>>,
    applied: Arc<tokio::sync::watch::Sender<u64>>,
    seen: tokio::sync::watch::Receiver<u64>,
    node: &'static str,
    claim: String,
    expires_at: i64,
    offers: usize,
}

type SqlCacheAdminFixture = (
    Arc<Mutex<rusqlite::Connection>>,
    Arc<tokio::sync::watch::Sender<u64>>,
);

impl SqlCacheAdminSlot {
    fn fixture() -> SqlCacheAdminFixture {
        let db = production_membership_tables();
        db.execute_batch(
                "INSERT INTO cluster_nodes (node_id, raft_id, raft_address, api_address, last_seen_at) \
                   VALUES ('node-a', 1, 'node-a:32401', 'node-a:32402', 100), \
                          ('node-b', 2, 'node-b:32401', 'node-b:32402', 100);",
            )
            .expect("two members");
        (
            Arc::new(Mutex::new(db)),
            Arc::new(tokio::sync::watch::channel(0).0),
        )
    }

    fn new(fixture: &SqlCacheAdminFixture, node: &'static str) -> Self {
        Self {
            db: Arc::clone(&fixture.0),
            applied: Arc::clone(&fixture.1),
            seen: fixture.1.subscribe(),
            node,
            claim: uuid::Uuid::new_v4().to_string(),
            expires_at: 4_000_000_000_000,
            offers: 0,
        }
    }

    fn write(fixture: &SqlCacheAdminFixture, sql: &str) {
        fixture
            .0
            .lock()
            .expect("fixture database")
            .execute_batch(sql)
            .expect("fixture write");
        fixture.1.send_modify(|index| *index += 1);
    }

    fn release(&self) {
        self.db
            .lock()
            .expect("fixture database")
            .execute(
                RELEASE_CACHE_ADMIN_REVOCATION_LEASE_SQL,
                rusqlite::params![self.node, self.claim],
            )
            .expect("exact release");
        self.applied.send_modify(|index| *index += 1);
    }

    fn holder(&self) -> Option<String> {
        use rusqlite::OptionalExtension;
        self.db
            .lock()
            .expect("fixture database")
            .query_row(
                "SELECT node_id FROM cluster_cache_admin_revocation_leases",
                [],
                |row| row.get(0),
            )
            .optional()
            .expect("exclusion row")
    }
}

impl CacheAdminExclusionSlot for SqlCacheAdminSlot {
    async fn offer(
        &mut self,
        _deadline: tokio::time::Instant,
    ) -> Result<CacheAdminOffer, MembershipError> {
        self.offers += 1;
        let db = self.db.lock().expect("fixture database");
        let changed = db
            .execute(
                &acquire_cache_admin_revocation_lease_sql(),
                rusqlite::params![self.node, self.claim, self.expires_at, 1_i64],
            )
            .expect("production acquire");
        self.applied.send_modify(|index| *index += 1);
        if changed == 1 {
            return Ok(CacheAdminOffer::Acquired);
        }
        let (fenced, holder) = db
            .query_row(
                &cache_admin_refusal_sql()
                    .replace("$1", "?1")
                    .replace("$2", "?2"),
                rusqlite::params![self.node, self.claim],
                |row| Ok((row.get::<_, i64>("fenced")? != 0, row.get("holder")?)),
            )
            .expect("production refusal read");
        Ok(cache_admin_offer_refusal(fenced, holder))
    }

    async fn holder_changed_by(
        &mut self,
        holder: &str,
        deadline: tokio::time::Instant,
    ) -> Result<bool, MembershipError> {
        use rusqlite::OptionalExtension;
        loop {
            let observed = *self.seen.borrow();
            let current: Option<String> = self
                .db
                .lock()
                .expect("fixture database")
                .query_row(CACHE_ADMIN_EXCLUSION_HOLDER_SQL, [], |row| row.get(0))
                .optional()
                .expect("exclusion hint");
            if current.as_deref() != Some(holder) {
                return Ok(true);
            }
            match tokio::time::timeout_at(deadline, self.seen.wait_for(|index| *index != observed))
                .await
            {
                Err(_) => return Ok(false),
                Ok(Err(_)) => return Err(MembershipError::Internal("watch closed".into())),
                Ok(Ok(_)) => {}
            }
        }
    }

    fn fresh_claim(&mut self) -> Result<(), MembershipError> {
        self.claim = uuid::Uuid::new_v4().to_string();
        self.expires_at += 20_000;
        Ok(())
    }
}

/// CLUSTER-DRILLS-2026-10-11 F6: two users sign out at the same moment
/// through two different nodes. The exclusion is one cluster-wide row,
/// so the second node's acquire changes zero rows; it waits inside its
/// own admission deadline for the first to release and then offers once
/// more, rather than answering 503 with its user's token still valid.
#[tokio::test(start_paused = true)]
async fn concurrent_sign_outs_on_two_nodes_both_take_the_exclusion_inside_the_deadline() {
    let fixture = SqlCacheAdminSlot::fixture();
    let start = tokio::time::Instant::now();
    let deadline = start + Duration::from_millis(2_500);
    let sign_out = |node| {
        let mut slot = SqlCacheAdminSlot::new(&fixture, node);
        async move {
            let admitted = admit_cache_admin_exclusion(&mut slot, deadline)
                .await
                .expect("admission");
            let held_from = tokio::time::Instant::now();
            assert_eq!(slot.holder().as_deref(), Some(node), "exclusion holder");
            // The revocation's own two fanouts and Store mutation.
            tokio::time::sleep(Duration::from_millis(600)).await;
            slot.release();
            (admitted, held_from - start, slot.offers)
        }
    };
    let ((first, first_at, first_offers), (second, second_at, second_offers)) =
        tokio::join!(sign_out("node-a"), sign_out("node-b"));
    assert_eq!(first, CacheAdminExclusionAdmission::Acquired);
    assert_eq!(second, CacheAdminExclusionAdmission::Acquired);
    assert_eq!(first_at, Duration::ZERO);
    assert_eq!(second_at, Duration::from_millis(600));
    assert_eq!((first_offers, second_offers), (1, 2));
}

/// A holder that never releases still ends a waiter at its deadline,
/// with nothing of the waiter's claim in the replicated state. The
/// entries this replica applies meanwhile (heartbeats) do not change the
/// row, so they cause no further offer.
#[tokio::test(start_paused = true)]
async fn a_held_exclusion_still_refuses_at_the_admission_deadline() {
    let fixture = SqlCacheAdminSlot::fixture();
    let mut holder = SqlCacheAdminSlot::new(&fixture, "node-a");
    let start = tokio::time::Instant::now();
    assert_eq!(
        holder.offer(start).await.expect("holder offer"),
        CacheAdminOffer::Acquired
    );
    let heartbeats = {
        let applied = Arc::clone(&fixture.1);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                applied.send_modify(|index| *index += 1);
            }
        })
    };
    let mut waiter = SqlCacheAdminSlot::new(&fixture, "node-b");
    let deadline = start + Duration::from_millis(2_500);
    assert_eq!(
        admit_cache_admin_exclusion(&mut waiter, deadline)
            .await
            .expect("admission"),
        CacheAdminExclusionAdmission::DeadlinePassed
    );
    assert_eq!(tokio::time::Instant::now(), deadline);
    assert_eq!(waiter.offers, 1, "heartbeats must not re-offer");
    assert_eq!(waiter.holder().as_deref(), Some("node-a"));
    heartbeats.abort();
}

/// A standing condition refuses every claim and outlives any admission
/// window, so the refusal answers at once after exactly one offer, as a
/// single offer always did: a maintenance row, a planned-outage lease
/// (rolling deploys hold one), or either of them while another claim
/// also holds the row.
#[tokio::test(start_paused = true)]
async fn a_fenced_exclusion_refuses_at_once_after_one_offer() {
    for (fence, also_held) in [
        (
            "INSERT INTO cluster_node_maintenance (node_id, requested_at) VALUES ('node-b', 1)",
            false,
        ),
        (
            "INSERT INTO cluster_operation_leases (singleton, node_id, operation, claim_id, \
                 expires_at) VALUES (1, 'node-b', 'restart', 'outage-claim', 4000000000000)",
            false,
        ),
        (
            "INSERT INTO cluster_node_maintenance (node_id, requested_at) VALUES ('node-b', 1)",
            true,
        ),
    ] {
        let fixture = SqlCacheAdminSlot::fixture();
        if also_held {
            let mut holder = SqlCacheAdminSlot::new(&fixture, "node-b");
            let now = tokio::time::Instant::now();
            assert_eq!(
                holder.offer(now).await.expect("holder offer"),
                CacheAdminOffer::Acquired
            );
        }
        SqlCacheAdminSlot::write(&fixture, fence);
        let mut slot = SqlCacheAdminSlot::new(&fixture, "node-a");
        let start = tokio::time::Instant::now();
        let refused =
            admit_cache_admin_exclusion(&mut slot, start + Duration::from_millis(2_500)).await;
        assert!(
            matches!(refused, Err(MembershipError::ClusterOperationPending)),
            "{fence}: {refused:?}"
        );
        assert_eq!(slot.offers, 1, "{fence}");
        assert_eq!(tokio::time::Instant::now(), start, "{fence}");
    }
}

/// A refusal of this claim alone, here the release watermark parked past
/// its expiry, is answered by one fresh claim with its own expiry, which
/// the same offer then admits.
#[tokio::test(start_paused = true)]
async fn a_refusal_of_this_claim_alone_is_retried_once_with_a_fresh_claim() {
    let fixture = SqlCacheAdminSlot::fixture();
    SqlCacheAdminSlot::write(
        &fixture,
        "INSERT INTO cluster_cache_admin_revocation_release_watermark \
             (singleton, expires_through) VALUES (1, 4000000010000)",
    );
    let mut slot = SqlCacheAdminSlot::new(&fixture, "node-a");
    let start = tokio::time::Instant::now();
    assert_eq!(
        admit_cache_admin_exclusion(&mut slot, start + Duration::from_millis(2_500))
            .await
            .expect("admission"),
        CacheAdminExclusionAdmission::Acquired
    );
    assert_eq!(slot.offers, 2);
    assert_eq!(slot.holder().as_deref(), Some("node-a"));
}

/// One method's body, ending where the method does.
///
/// Every source-shape assertion in this module used to slice to a landmark
/// further down the file — the next method's doc comment, a sibling's
/// declaration. Such a slice widens silently the moment anything is
/// inserted between the two, and the assertions over it then read code the
/// test never meant to cover. That is not hypothetical: two
/// cache-revocation methods were added after `status`, one of them reading
/// consistently for its own good reasons, and
/// `the_roster_projection_reads_locally_and_the_decisions_do_not` failed on
/// a `query_consistent_map` a hundred lines outside the method it is about.
///
/// Brace matching from the signature ends at the method's own closing
/// brace, so a neighbour can never be read as part of it again. Take a
/// method's text from here rather than splitting on whatever happens to
/// follow it.
fn method_body(source: &str, signature: &str) -> String {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("{signature} is missing"));
    let open = source[start..]
        .find('{')
        .unwrap_or_else(|| panic!("{signature} has no body"))
        + start;
    let mut depth = 0usize;
    for (offset, character) in source[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return source[open..=open + offset].to_owned();
                }
            }
            _ => {}
        }
    }
    panic!("{signature} has an unbalanced body");
}

#[test]
fn removal_attempt_and_active_media_session_are_mutually_exclusive() {
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_node_removal_attempts (\
                   node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, \
                   PRIMARY KEY(node_id, attempt_id)); \
                 CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, last_seen_at INTEGER, removed_at INTEGER); \
                 CREATE TABLE cluster_node_capabilities (\
                   node_id TEXT, capability TEXT, last_seen_at INTEGER, \
                   PRIMARY KEY(node_id, capability)); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_promotions (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_maintenance (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_operation_leases (singleton INTEGER PRIMARY KEY); \
                 CREATE TABLE media_sessions (owner_node_id TEXT, state TEXT, \
                   lease_expires_at_ms INTEGER); \
                 INSERT INTO cluster_nodes VALUES ('node-a', 10, NULL); \
                 INSERT INTO cluster_node_capabilities VALUES (\
                   'node-a', 'membership_removal_attempt_refs_v1', 10); \
                 INSERT INTO media_sessions VALUES ('node-a', 'active', 100);",
        )
        .expect("seed active media owner");

    assert_eq!(
        connection
            .execute(
                &begin_removal_attempt_sql(false),
                rusqlite::params!["node-a", "blocked-attempt", 50],
            )
            .expect("refuse removal while media is active"),
        0
    );
    connection
        .execute(
            "UPDATE media_sessions SET state = 'ended' WHERE owner_node_id = 'node-a'",
            [],
        )
        .expect("drain media owner");
    assert_eq!(
        connection
            .execute(
                &begin_removal_attempt_sql(false),
                rusqlite::params!["node-a", "admitted-attempt", 50],
            )
            .expect("admit removal after media drains"),
        1
    );
    connection
        .execute("DELETE FROM cluster_node_removal_attempts", [])
        .expect("reset removal attempt");
    connection
        .execute("INSERT INTO cluster_node_maintenance VALUES ('node-b')", [])
        .expect("begin maintenance");
    assert_eq!(
        connection
            .execute(
                &begin_removal_attempt_sql(true),
                rusqlite::params!["node-a", "maintenance-blocked"],
            )
            .expect("maintenance blocks removal"),
        0
    );
    connection
        .execute("DELETE FROM cluster_node_maintenance", [])
        .expect("finish maintenance");
    connection
        .execute("DELETE FROM cluster_node_removal_attempts", [])
        .expect("reset removal attempt");
    connection
        .execute(
            "UPDATE media_sessions SET state = 'active' WHERE owner_node_id = 'node-a'",
            [],
        )
        .expect("restore active media owner");
    assert_eq!(
        connection
            .execute(
                &begin_removal_attempt_sql(true),
                rusqlite::params!["node-a", "draining-attempt"],
            )
            .expect("draining removal binds only the placeholders it emits"),
        1
    );
}

#[test]
fn removal_media_fence_first_writes_replaced_and_blocks_terminal_projection() {
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_node_removal_attempts (\
                   node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, \
                   PRIMARY KEY(node_id, attempt_id)); \
                 CREATE TABLE media_sessions (\
                   session_id TEXT PRIMARY KEY, owner_node_id TEXT NOT NULL, state TEXT NOT NULL, \
                   terminal_reason TEXT, publication_ready_at_ms INTEGER NOT NULL, \
                   lease_expires_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL); \
                 INSERT INTO cluster_node_removal_attempts VALUES ('node-a', 'attempt-a'); \
                 INSERT INTO media_sessions VALUES \
                   ('new-cause', 'node-a', 'active', NULL, 700, 900, 100), \
                   ('prior-cause', 'node-a', 'active', 'admin_stop', 800, 900, 100), \
                   ('other-owner', 'node-b', 'active', NULL, 900, 900, 100);",
        )
        .expect("seed removal media fence");

    assert_eq!(
        connection
            .execute(
                BEGIN_REMOVAL_MEDIA_FENCE_SQL,
                rusqlite::params![
                    500,
                    crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
                    "node-a",
                    "attempt-a",
                ],
            )
            .expect("apply exact removal media fence"),
        2
    );

    let route = |session_id: &str| {
        connection
            .query_row(
                "SELECT state, terminal_reason, publication_ready_at_ms, \
                            lease_expires_at_ms, updated_at_ms \
                       FROM media_sessions WHERE session_id = ?1",
                [session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .expect("read fenced media route")
    };
    assert_eq!(
        route("new-cause"),
        (
            "ended".to_owned(),
            Some("replaced".to_owned()),
            crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            500,
            500
        )
    );
    assert_eq!(
        route("prior-cause"),
        (
            "ended".to_owned(),
            Some("admin_stop".to_owned()),
            crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            500,
            500,
        )
    );
    assert_eq!(
        route("other-owner"),
        ("active".to_owned(), None, 900, 900, 100)
    );
}

#[test]
fn one_attempt_cannot_roll_back_a_concurrent_removal_fence() {
    fn rollback(connection: &mut rusqlite::Connection, node_id: &str, attempt_id: &str) {
        let transaction = connection.transaction().expect("rollback transaction");
        transaction
            .execute(
                &rollback_removal_attempt_sql(),
                rusqlite::params![node_id, attempt_id],
            )
            .expect("release attempt when rollout permits");
        transaction
            .execute(
                ROLLBACK_REMOVAL_OWNER_FENCE_SQL,
                rusqlite::params![removed_job_owner_key(node_id), node_id],
            )
            .expect("conditionally release owner fence");
        transaction
            .execute(ROLLBACK_REMOVAL_FENCE_SQL, rusqlite::params![node_id])
            .expect("conditionally release removal fence");
        transaction.commit().expect("commit rollback");
    }

    let mut connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY, started_at INTEGER); \
                 CREATE TABLE cluster_node_removal_attempts (\
                   node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, \
                   PRIMARY KEY(node_id, attempt_id)); \
                 CREATE TABLE cluster_node_removal_intents (\
                   node_id TEXT PRIMARY KEY, attempt_id TEXT NOT NULL); \
                 CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, raft_id INTEGER, \
                   last_seen_at INTEGER, removed_at INTEGER); \
                 CREATE TABLE cluster_node_capabilities (\
                   node_id TEXT, capability TEXT, last_seen_at INTEGER, \
                   PRIMARY KEY(node_id, capability)); \
                 CREATE TABLE cluster_node_heartbeat_intents (\
                   node_id TEXT PRIMARY KEY, last_seen_at INTEGER); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_promotions (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_node_maintenance (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_operation_leases (singleton INTEGER PRIMARY KEY); \
                 CREATE TABLE cluster_join_tokens (node_id TEXT, state TEXT); \
                 CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT, updated_at INTEGER); \
                 CREATE TABLE job_leases (owner_node_id TEXT, expires_at_ms INTEGER, \
                   revision INTEGER, updated_at_ms INTEGER); \
                 CREATE TABLE media_sessions (owner_node_id TEXT, state TEXT, \
                   lease_expires_at_ms INTEGER); \
                 INSERT INTO cluster_nodes VALUES ('node-a', 1, 10, NULL); \
                 INSERT INTO cluster_nodes VALUES ('node-old', 2, 20, NULL); \
                 INSERT INTO cluster_nodes VALUES ('staged-join', 99, 30, NULL); \
                 INSERT INTO cluster_node_join_staging VALUES ('staged-join'); \
                 INSERT INTO cluster_join_tokens VALUES ('staged-join', 'redeeming'); \
                 INSERT INTO cluster_node_capabilities VALUES (\
                   'node-a', 'membership_removal_attempt_refs_v1', 10); \
                 INSERT INTO cluster_node_removals VALUES ('node-a', 1); \
                 INSERT INTO cluster_node_removal_attempts VALUES ('node-a', 'attempt-a'); \
                 INSERT INTO cluster_node_removal_attempts VALUES ('node-a', 'attempt-b'); \
                 INSERT INTO settings VALUES (\
                   'internal.cluster_job_owner_removed.node-a', '1', 0); \
                 INSERT INTO job_leases VALUES ('blocked', 100, 1, 0);",
        )
        .expect("seed concurrent attempts");
    connection
        .execute_batch(PROTECT_REMOVAL_FENCE_DELETE_SQL)
        .expect("install legacy removal guard");
    connection
        .execute_batch(PROTECT_REMOVAL_OWNER_FENCE_DELETE_SQL)
        .expect("install legacy owner guard");
    connection
        .execute_batch(MARK_LEGACY_NODE_INSERT_DURING_REMOVAL_SQL)
        .expect("install legacy insert marker");
    connection
        .execute_batch(MARK_LEGACY_NODE_HEARTBEAT_DURING_REMOVAL_SQL)
        .expect("install legacy heartbeat marker");
    connection
        .execute_batch(MARK_LEGACY_NODE_FINALIZE_DURING_REMOVAL_SQL)
        .expect("install legacy finalize marker");

    // A rolling cluster refuses a fresh removal before it creates any
    // attempt or shared fence. The staged join is excluded; node-old is
    // the active finalized node that lacks the capability.
    let transaction = connection
        .transaction()
        .expect("mixed-version begin transaction");
    assert_eq!(
        transaction
            .execute(
                &begin_removal_attempt_sql(false),
                rusqlite::params!["blocked", "blocked-attempt", 0],
            )
            .expect("gate mixed-version begin"),
        0
    );
    transaction
        .execute(
            BEGIN_REMOVAL_INTENT_SQL,
            rusqlite::params!["blocked", "blocked-attempt"],
        )
        .expect("guard mixed-version removal intent");
    transaction
        .execute(
            BEGIN_REMOVAL_FENCE_SQL,
            rusqlite::params!["blocked", 40, "blocked-attempt"],
        )
        .expect("guard mixed-version removal fence");
    transaction
        .execute(
            BEGIN_REMOVAL_OWNER_FENCE_SQL,
            rusqlite::params![
                "internal.cluster_job_owner_removed.blocked",
                40,
                "blocked",
                "blocked-attempt"
            ],
        )
        .expect("guard mixed-version owner fence");
    transaction
        .execute(
            BEGIN_REMOVAL_JOB_FENCE_SQL,
            rusqlite::params![40, "blocked", "blocked-attempt"],
        )
        .expect("guard mixed-version job fence");
    transaction
        .execute(
            CLEAR_REMOVAL_INTENT_SQL,
            rusqlite::params!["blocked", "blocked-attempt"],
        )
        .expect("consume mixed-version removal intent");
    transaction.commit().expect("commit refused begin");
    let blocked_side_effects: i64 = connection
        .query_row(
            "SELECT \
                   (SELECT COUNT(*) FROM cluster_node_removal_attempts \
                     WHERE node_id = 'blocked') + \
                   (SELECT COUNT(*) FROM cluster_node_removal_intents \
                     WHERE node_id = 'blocked') + \
                   (SELECT COUNT(*) FROM cluster_node_removals WHERE node_id = 'blocked') + \
                   (SELECT COUNT(*) FROM settings \
                     WHERE key = 'internal.cluster_job_owner_removed.blocked') + \
                   (SELECT COUNT(*) FROM job_leases \
                     WHERE owner_node_id != 'blocked' OR revision != 1)",
            [],
            |row| row.get(0),
        )
        .expect("count refused begin side effects");
    assert_eq!(blocked_side_effects, 0);

    // An old binary's unconditional rollback cannot erase a fence that a
    // reference-aware attempt currently protects.
    assert_eq!(
        connection
            .execute(
                "DELETE FROM cluster_node_removals WHERE node_id = 'node-a'",
                [],
            )
            .expect("legacy removal delete"),
        0
    );
    assert_eq!(
        connection
            .execute(
                "DELETE FROM settings WHERE key = \
                     'internal.cluster_job_owner_removed.node-a'",
                [],
            )
            .expect("legacy owner-fence delete"),
        0
    );

    connection
        .execute(
            "INSERT INTO cluster_node_capabilities VALUES ($1, $2, $3)",
            rusqlite::params!["node-old", REMOVAL_ATTEMPT_CAPABILITY, 20],
        )
        .expect("finish capability rollout");

    // The exact current heartbeat sequence carries a transaction-local
    // intent, so the legacy trigger neither deletes its capability nor
    // pins the active removal.
    let transaction = connection
        .transaction()
        .expect("current heartbeat transaction");
    transaction
        .execute(
            "INSERT INTO cluster_node_heartbeat_intents VALUES ($1, $2) \
                 ON CONFLICT(node_id) DO UPDATE SET last_seen_at = excluded.last_seen_at",
            rusqlite::params!["node-a", 11],
        )
        .expect("publish heartbeat intent");
    transaction
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = 11 WHERE node_id = 'node-a'",
            [],
        )
        .expect("apply current heartbeat");
    transaction
            .execute(
                "INSERT INTO cluster_node_capabilities VALUES ($1, $2, $3) \
                 ON CONFLICT(node_id, capability) DO UPDATE SET last_seen_at = excluded.last_seen_at",
                rusqlite::params!["node-a", REMOVAL_ATTEMPT_CAPABILITY, 11],
            )
            .expect("refresh current capability");
    transaction
        .execute(
            "DELETE FROM cluster_node_join_staging WHERE node_id = 'node-a'",
            [],
        )
        .expect("clear current staging marker");
    transaction
        .execute(
            "DELETE FROM cluster_node_heartbeat_intents WHERE node_id = 'node-a' \
                 AND last_seen_at = 11",
            [],
        )
        .expect("consume heartbeat intent");
    transaction.commit().expect("commit current heartbeat");
    let legacy_refs: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM cluster_node_removal_attempts \
                 WHERE node_id = 'node-a' AND attempt_id = 'internal.legacy-writer'",
            [],
            |row| row.get(0),
        )
        .expect("count current-heartbeat legacy refs");
    assert_eq!(legacy_refs, 0);

    rollback(&mut connection, "node-a", "attempt-a");
    let shared_fence: i64 = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM cluster_node_removals WHERE node_id = 'node-a') + \
                        (SELECT COUNT(*) FROM settings WHERE key = \
                          'internal.cluster_job_owner_removed.node-a')",
            [],
            |row| row.get(0),
        )
        .expect("count concurrent shared fence");
    assert_eq!(shared_fence, 2);
    rollback(&mut connection, "node-a", "attempt-b");

    let remaining_attempts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM cluster_node_removal_attempts WHERE node_id = 'node-a'",
            [],
            |row| row.get(0),
        )
        .expect("count remaining attempts");
    let fences: i64 = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM cluster_node_removals WHERE node_id = 'node-a') + \
                        (SELECT COUNT(*) FROM settings WHERE key = \
                          'internal.cluster_job_owner_removed.node-a')",
            [],
            |row| row.get(0),
        )
        .expect("count remaining fences");
    assert_eq!(remaining_attempts, 0);
    assert_eq!(fences, 0);

    // If a legacy heartbeat appears after begin, the replicated trigger
    // invalidates its capability and pins an unreleasable reference before
    // that process can send an untracked removal proposal.
    connection
        .execute_batch(
            "INSERT INTO cluster_node_removals VALUES ('node-gap', 2); \
                 INSERT INTO cluster_node_removal_attempts VALUES ('node-gap', 'gap-attempt'); \
                 INSERT INTO settings VALUES (\
                   'internal.cluster_job_owner_removed.node-gap', '1', 0);",
        )
        .expect("seed capability-gap attempt");
    connection
        .execute(
            "UPDATE cluster_nodes SET last_seen_at = 21 WHERE node_id = 'node-old'",
            [],
        )
        .expect("apply legacy heartbeat");
    connection
            .execute(
                "INSERT INTO cluster_node_capabilities VALUES ($1, $2, $3) \
                 ON CONFLICT(node_id, capability) DO UPDATE SET last_seen_at = excluded.last_seen_at",
                rusqlite::params!["node-old", REMOVAL_ATTEMPT_CAPABILITY, 21],
            )
            .expect("upgrade legacy heartbeat writer");
    rollback(&mut connection, "node-gap", "gap-attempt");

    let remaining_attempts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM cluster_node_removal_attempts \
                 WHERE node_id = 'node-gap'",
            [],
            |row| row.get(0),
        )
        .expect("count final attempts");
    let fences: i64 = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM cluster_node_removals \
                          WHERE node_id = 'node-gap') + \
                        (SELECT COUNT(*) FROM settings WHERE key = \
                          'internal.cluster_job_owner_removed.node-gap')",
            [],
            |row| row.get(0),
        )
        .expect("count final fences");
    assert_eq!(remaining_attempts, 1);
    assert_eq!(fences, 2);
}

#[test]
fn legacy_join_finalize_pins_an_active_removal_fence() {
    let mut connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY, started_at INTEGER); \
                 CREATE TABLE cluster_node_removal_attempts (\
                   node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, \
                   PRIMARY KEY(node_id, attempt_id)); \
                 CREATE TABLE cluster_nodes (\
                   node_id TEXT PRIMARY KEY, last_seen_at INTEGER, removed_at INTEGER); \
                 CREATE TABLE cluster_node_capabilities (\
                   node_id TEXT, capability TEXT, last_seen_at INTEGER, \
                   PRIMARY KEY(node_id, capability)); \
                 CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY); \
                 CREATE TABLE cluster_join_tokens (node_id TEXT, state TEXT); \
                 CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT, updated_at INTEGER); \
                 INSERT INTO cluster_nodes VALUES ('legacy-join', 30, NULL); \
                 INSERT INTO cluster_node_join_staging VALUES ('legacy-join'); \
                 INSERT INTO cluster_join_tokens VALUES ('legacy-join', 'redeeming'); \
                 INSERT INTO cluster_node_removals VALUES ('target', 1); \
                 INSERT INTO cluster_node_removal_attempts VALUES ('target', 'attempt'); \
                 INSERT INTO settings VALUES (\
                   'internal.cluster_job_owner_removed.target', '1', 0);",
        )
        .expect("seed staged legacy finalize");
    connection
        .execute_batch(MARK_LEGACY_NODE_FINALIZE_DURING_REMOVAL_SQL)
        .expect("install legacy finalize marker");

    connection
        .execute(
            "UPDATE cluster_join_tokens SET state = 'redeemed' \
                 WHERE node_id = 'legacy-join'",
            [],
        )
        .expect("finalize staged legacy join");
    let staging_and_legacy_ref: (i64, i64) = connection
        .query_row(
            "SELECT \
                   (SELECT COUNT(*) FROM cluster_node_join_staging \
                     WHERE node_id = 'legacy-join'), \
                   (SELECT COUNT(*) FROM cluster_node_removal_attempts \
                     WHERE node_id = 'target' \
                       AND attempt_id = 'internal.legacy-writer')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read finalize protection");
    assert_eq!(staging_and_legacy_ref, (0, 1));

    let transaction = connection.transaction().expect("rollback transaction");
    transaction
        .execute(
            &rollback_removal_attempt_sql(),
            rusqlite::params!["target", "attempt"],
        )
        .expect("release normal attempt");
    transaction
        .execute(
            ROLLBACK_REMOVAL_OWNER_FENCE_SQL,
            rusqlite::params![removed_job_owner_key("target"), "target"],
        )
        .expect("retain shared owner fence");
    transaction
        .execute(ROLLBACK_REMOVAL_FENCE_SQL, rusqlite::params!["target"])
        .expect("retain shared removal fence");
    transaction.commit().expect("commit rollback");

    let fences: i64 = connection
        .query_row(
            "SELECT \
                   (SELECT COUNT(*) FROM cluster_node_removals \
                     WHERE node_id = 'target') + \
                   (SELECT COUNT(*) FROM settings WHERE key = \
                     'internal.cluster_job_owner_removed.target')",
            [],
            |row| row.get(0),
        )
        .expect("count pinned shared fences");
    assert_eq!(fences, 2);
}

#[test]
fn replicated_guard_refuses_legacy_removal_and_backfills_existing_fences() {
    let mut connection = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
    connection
        .execute_batch(
            "CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY, started_at INTEGER); \
                 CREATE TABLE cluster_node_removal_attempts (\
                   node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, \
                   PRIMARY KEY(node_id, attempt_id)); \
                 CREATE TABLE cluster_node_removal_intents (\
                   node_id TEXT PRIMARY KEY, attempt_id TEXT NOT NULL); \
                 INSERT INTO cluster_node_removals VALUES ('already-pending', 1);",
        )
        .expect("seed pre-upgrade removal fence");
    let transaction = connection.transaction().expect("install guard transaction");
    transaction
        .execute(BACKFILL_REMOVAL_ATTEMPT_REFS_SQL, [])
        .expect("backfill pre-upgrade attempt reference");
    transaction
        .execute_batch(REQUIRE_REMOVAL_INTENT_SQL)
        .expect("install removal-intent guard");
    transaction.commit().expect("commit guard installation");

    let backfilled: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM cluster_node_removal_attempts \
                 WHERE node_id = 'already-pending' \
                   AND attempt_id = 'internal.preexisting'",
            [],
            |row| row.get(0),
        )
        .expect("count backfilled reference");
    assert_eq!(backfilled, 1);
    assert!(connection
        .execute(
            "INSERT INTO cluster_node_removals VALUES ('legacy-new', 2)",
            [],
        )
        .is_err());
    assert!(connection
        .execute(
            "INSERT INTO cluster_node_removals VALUES ('already-pending', 2) \
                 ON CONFLICT(node_id) DO NOTHING",
            [],
        )
        .is_err());

    let transaction = connection.transaction().expect("current begin transaction");
    transaction
        .execute(
            "INSERT INTO cluster_node_removal_attempts VALUES ('current', 'uuid-attempt')",
            [],
        )
        .expect("insert current attempt");
    transaction
        .execute(
            BEGIN_REMOVAL_INTENT_SQL,
            rusqlite::params!["current", "uuid-attempt"],
        )
        .expect("insert current intent");
    transaction
        .execute(
            "INSERT INTO cluster_node_removals VALUES ('current', 3)",
            [],
        )
        .expect("insert current removal fence");
    transaction
        .execute(
            CLEAR_REMOVAL_INTENT_SQL,
            rusqlite::params!["current", "uuid-attempt"],
        )
        .expect("consume current intent");
    transaction.commit().expect("commit current begin");

    let current_state: (i64, i64) = connection
        .query_row(
            "SELECT \
                   (SELECT COUNT(*) FROM cluster_node_removals \
                     WHERE node_id = 'current'), \
                   (SELECT COUNT(*) FROM cluster_node_removal_intents \
                     WHERE node_id = 'current')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read current removal state");
    assert_eq!(current_state, (1, 0));
}

fn payload() -> JoinPayload {
    JoinPayload {
        version: JOIN_TOKEN_VERSION,
        cluster_id: "cluster-a".to_owned(),
        raft_id: 2,
        expires_at: 123,
        bootstrap_http: "http://127.0.0.1:32400".to_owned(),
        bootstrap: vec![ClusterPeer {
            raft_id: 1,
            raft_address: "127.0.0.1:32401".to_owned(),
            api_address: "127.0.0.1:32402".to_owned(),
        }],
        secrets: JoinSecretPayload {
            raft: "r".repeat(64),
            api: "a".repeat(64),
            credential_key: "c".repeat(64),
        },
        activation_marker: ActivationMarker {
            marker_version: 1,
            cluster_id: "cluster-a".to_owned(),
            source_backup_sha256: "f".repeat(64),
            source_schema_version: 18,
            replicated_schema_version: AUTH_SCHEMA_VERSION,
            imported_rows: 1,
            table_hashes: vec![crate::store::SqliteImportTableDigest {
                table: "settings".to_owned(),
                row_count: 1,
                sha256: "e".repeat(64),
            }],
            admitted_role: None,
        },
        schema_version: AUTH_SCHEMA_VERSION,
        protocol_version: AUTH_PROTOCOL_MIN,
    }
}

/// The v2 payload for the same fixture cluster, as a learner token.
fn learner_payload() -> JoinPayloadV2 {
    let v1 = payload();
    JoinPayloadV2 {
        version: JOIN_TOKEN_V2_VERSION,
        cluster_id: v1.cluster_id,
        raft_id: v1.raft_id,
        expires_at: v1.expires_at,
        bootstrap_http: v1.bootstrap_http,
        bootstrap: v1.bootstrap,
        secrets: v1.secrets,
        activation_marker: v1.activation_marker,
        schema_version: v1.schema_version,
        protocol_min: AUTH_LEARNER_PROTOCOL,
        protocol_max: AUTH_LEARNER_PROTOCOL,
        role: ClusterRole::Learner,
    }
}

#[test]
fn join_token_round_trips_without_plaintext_payload() {
    let expected = payload();
    let token = encode_join_token(&expected).expect("encode token");
    assert!(token.starts_with(JOIN_TOKEN_PREFIX));
    assert!(!token.contains("cluster-a"));
    assert!(!token.contains(&"r".repeat(64)));
    assert_eq!(
        decode_join_token(&token).expect("decode token"),
        JoinToken::V1(expected)
    );
}

/// The previous release's join-token decoder, transcribed.
///
/// A v1-only coordinator or joiner has to refuse a v2 token rather than
/// misread it, and "it obviously will" is not a proof. This is the exact
/// shipped v1 rule — prefix, then AAD, then version — so the assertions
/// below test the refusal rather than assuming it.
fn decode_join_token_as_the_previous_release(token: &str) -> Result<JoinPayload, MembershipError> {
    let mut parts = token.split(':');
    if parts.next() != Some("plxjoin") || parts.next() != Some("v1") || parts.clone().count() != 2 {
        return Err(MembershipError::InvalidToken);
    }
    let key_bytes = hex::decode(parts.next().ok_or(MembershipError::InvalidToken)?)
        .map_err(|_| MembershipError::InvalidToken)?;
    let encrypted = hex::decode(parts.next().ok_or(MembershipError::InvalidToken)?)
        .map_err(|_| MembershipError::InvalidToken)?;
    if key_bytes.len() != 32 || encrypted.len() <= 24 {
        return Err(MembershipError::InvalidToken);
    }
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key_bytes));
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&encrypted[..24]),
            chacha20poly1305::aead::Payload {
                msg: &encrypted[24..],
                aad: JOIN_TOKEN_AAD,
            },
        )
        .map_err(|_| MembershipError::InvalidToken)?;
    let payload: JoinPayload =
        serde_json::from_slice(&plaintext).map_err(|_| MembershipError::InvalidToken)?;
    if payload.version != JOIN_TOKEN_VERSION || payload.cluster_id.trim().is_empty() {
        return Err(MembershipError::InvalidToken);
    }
    Ok(payload)
}

/// Decrypt a token's payload under an explicit associated data, so a test
/// can assert on the *layer* that refused rather than on a code every
/// layer shares.
///
/// Every refusal in `decode_join_token` is `join_token_invalid`, which is
/// the right thing for an attacker to see and useless for telling three
/// independent defences apart. This reaches past the decoder and asks the
/// AEAD directly.
fn aead_opens(token: &str, aad: &[u8]) -> bool {
    let parts = token.split(':').collect::<Vec<_>>();
    assert_eq!(parts.len(), 4, "token framing changed: {token}");
    let key_bytes = hex::decode(parts[2]).expect("token key");
    let encrypted = hex::decode(parts[3]).expect("token ciphertext");
    XChaCha20Poly1305::new(Key::from_slice(&key_bytes))
        .decrypt(
            XNonce::from_slice(&encrypted[..24]),
            chacha20poly1305::aead::Payload {
                msg: &encrypted[24..],
                aad,
            },
        )
        .is_ok()
}

/// Three independent refusals, each asserted at the layer that produces
/// it.
///
/// The prefix stops the ordinary case. Rewriting the prefix defeats that
/// check and the AEAD's associated data stops it instead. Re-sealing the
/// same payload under v1's key derivation defeats *that*, and the version
/// field stops it. An old build has to fail all three ways, because a
/// single one of them could be lost to a future refactor.
///
/// Asserting `.code() == "join_token_invalid"` three times cannot tell
/// which layer fired — every layer returns it — so collapsing
/// `JOIN_TOKEN_V2_AAD` onto v1's value left this test green while the
/// second defence no longer existed. The AEAD is now asked directly.
#[test]
fn a_previous_release_refuses_a_v2_token_three_separate_ways() {
    let token = encode_join_token_v2(&learner_payload()).expect("encode a v2 token");
    assert!(token.starts_with(JOIN_TOKEN_V2_PREFIX));
    assert_eq!(
        decode_join_token_as_the_previous_release(&token)
            .expect_err("a v1-only build must refuse the v2 prefix")
            .code(),
        "join_token_invalid"
    );

    // 2. Prefix relabelled to v1: the associated data no longer matches,
    //    so the payload cannot even be decrypted.
    let relabelled = token.replacen(JOIN_TOKEN_V2_PREFIX, JOIN_TOKEN_PREFIX, 1);
    // The layer itself, not the shared code the decoder returns: a v2
    // token's ciphertext does not open under v1's associated data, and
    // does open under its own. Two assertions, because only the pair
    // proves the difference is the AAD rather than a broken token.
    assert!(
        !aead_opens(&relabelled, JOIN_TOKEN_AAD),
        "a v2 token must not decrypt under v1's associated data"
    );
    assert!(
        aead_opens(&relabelled, JOIN_TOKEN_V2_AAD),
        "and it must decrypt under its own, or the test above proves nothing"
    );
    assert_ne!(
        JOIN_TOKEN_AAD, JOIN_TOKEN_V2_AAD,
        "the two framings must not share associated data"
    );
    assert_eq!(
        decode_join_token_as_the_previous_release(&relabelled)
            .expect_err("a relabelled v2 token must fail its AEAD check")
            .code(),
        "join_token_invalid"
    );
    // And this build refuses it too: a v1 frame is read with the v1 rules.
    assert_eq!(
        decode_join_token(&relabelled)
            .expect_err("this build must not read a v2 payload through the v1 frame")
            .code(),
        "join_token_invalid"
    );

    // 3. Re-sealed under v1's associated data, so decryption succeeds and
    //    only the version field is left to refuse it.
    let resealed = seal_join_token(&learner_payload(), JOIN_TOKEN_PREFIX, JOIN_TOKEN_AAD)
        .expect("re-seal the learner payload under v1 framing");
    assert_eq!(
        decode_join_token_as_the_previous_release(&resealed)
            .expect_err("version 2 is not a v1 payload")
            .code(),
        "join_token_invalid"
    );
    assert_eq!(
        decode_join_token(&resealed)
            .expect_err("nor is it one for this build")
            .code(),
        "join_token_invalid"
    );

    // The v1 token this build still mints stays readable by that release.
    let voter = encode_join_token(&payload()).expect("encode a v1 token");
    let decoded = decode_join_token_as_the_previous_release(&voter)
        .expect("the previous release still reads a voter token");
    assert_eq!(decoded, payload());
}

/// The v2 frame's own version gate, reached through the production
/// decoder.
///
/// Every v2-version assertion above goes through the v1 arm — a payload
/// re-sealed under v1 framing is read by v1's rules — so the production v2
/// arm's `payload.version != JOIN_TOKEN_V2_VERSION` was never executed and
/// deleting it kept everything green. A payload sealed with the *v2*
/// prefix and the *v2* associated data is the only thing that reaches it:
/// framing and AEAD both pass, and the version field is all that is left.
#[test]
fn the_v2_frame_refuses_a_payload_that_is_not_version_two() {
    let mut wrong_version = learner_payload();
    wrong_version.version = JOIN_TOKEN_V2_VERSION + 1;
    let token = seal_join_token(&wrong_version, JOIN_TOKEN_V2_PREFIX, JOIN_TOKEN_V2_AAD)
        .expect("seal a v2-framed payload carrying the wrong version");

    // The two layers before the version gate both pass, so the refusal
    // below can only come from the gate itself.
    assert!(token.starts_with(JOIN_TOKEN_V2_PREFIX));
    assert!(
        aead_opens(&token, JOIN_TOKEN_V2_AAD),
        "the AEAD must accept this token, or the version gate is unreached"
    );
    assert_eq!(
        decode_join_token(&token)
            .expect_err("a v2 frame must refuse a payload that is not version 2")
            .code(),
        "join_token_invalid"
    );

    // The same shape one version below, so the gate is equality and not a
    // minimum that a future v3 payload would sail through.
    let mut older = learner_payload();
    older.version = JOIN_TOKEN_V2_VERSION - 1;
    let token = seal_join_token(&older, JOIN_TOKEN_V2_PREFIX, JOIN_TOKEN_V2_AAD)
        .expect("seal a v2-framed payload carrying an older version");
    assert!(decode_join_token(&token).is_err());

    // And the honest payload still decodes, so none of the above is
    // passing for an unrelated reason.
    let good = encode_join_token_v2(&learner_payload()).expect("encode a v2 token");
    assert_eq!(
        decode_join_token(&good).expect("a real v2 token decodes"),
        JoinToken::V2(learner_payload())
    );
}

/// A token says what it admits, and a v1 token can only ever say "voter".
#[test]
fn a_decoded_token_reports_its_role_and_the_range_it_was_minted_for() {
    let voter = encode_join_token(&payload()).expect("encode a v1 token");
    let voter = decode_join_token(&voter).expect("decode the v1 token");
    assert_eq!(voter.role(), ClusterRole::Voter);
    assert_eq!(
        voter.declared_protocol_range(),
        (AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN),
        "a v1 token's one scalar is the one-element range it always was"
    );

    let learner = encode_join_token_v2(&learner_payload()).expect("encode a v2 token");
    assert!(
        !learner.contains("learner"),
        "the role must not be plaintext"
    );
    let learner = decode_join_token(&learner).expect("decode the v2 token");
    assert_eq!(learner.role(), ClusterRole::Learner);
    assert_eq!(
        learner.declared_protocol_range(),
        (AUTH_LEARNER_PROTOCOL, AUTH_LEARNER_PROTOCOL)
    );
}

/// A framing this build has never heard of is refused before a key is
/// derived — which is what a joiner relies on to leave nothing on disk.
#[test]
fn an_unknown_token_framing_is_refused() {
    let token = encode_join_token_v2(&learner_payload()).expect("encode a v2 token");
    let future = token.replacen(JOIN_TOKEN_V2_PREFIX, "plxjoin:v3", 1);
    assert_eq!(
        decode_join_token(&future)
            .expect_err("a future framing is not something this build may guess at")
            .code(),
        "join_token_invalid"
    );
}

/// `membership.json` versions and roles are one fact written twice, and a
/// record where the two disagree is refused rather than resolved.
#[test]
fn a_membership_record_version_names_exactly_one_role() {
    assert_eq!(local_membership_version(ClusterRole::Voter), 1);
    assert_eq!(local_membership_version(ClusterRole::Learner), 2);
    assert!(local_membership_version_matches_role(1, ClusterRole::Voter));
    assert!(local_membership_version_matches_role(
        2,
        ClusterRole::Learner
    ));
    // A voter keeps writing version 1 so the previous release can still
    // read it; a learner writes version 2 so that release refuses it.
    assert!(!local_membership_version_matches_role(
        1,
        ClusterRole::Learner
    ));
    assert!(!local_membership_version_matches_role(
        2,
        ClusterRole::Voter
    ));
    assert!(!local_membership_version_matches_role(
        3,
        ClusterRole::Voter
    ));
}

#[test]
fn malformed_and_tampered_tokens_have_one_stable_refusal() {
    assert_eq!(
        decode_join_token("not-a-token")
            .expect_err("malformed")
            .code(),
        "join_token_invalid"
    );
    let token = encode_join_token(&payload()).expect("encode token");
    let mut bytes = token.into_bytes();
    let last = bytes.last_mut().expect("token byte");
    *last = if *last == b'0' { b'1' } else { b'0' };
    let tampered = String::from_utf8(bytes).expect("ascii token");
    assert_eq!(
        decode_join_token(&tampered).expect_err("tampered").code(),
        "join_token_invalid"
    );
}

#[test]
fn stable_membership_refusal_codes_are_operator_visible() {
    assert_eq!(MembershipError::ExpiredToken.code(), "join_token_expired");
    assert_eq!(MembershipError::ReusedToken.code(), "join_token_reused");
    assert_eq!(
        MembershipError::ClusterOperationPending.code(),
        "cluster_operation_pending"
    );
    assert_eq!(
        MembershipError::QuorumLoss.code(),
        "removal_would_lose_quorum"
    );
    // Activation's three refusals are three different operator actions —
    // wait, start-or-remove, upgrade — so they cannot share a code.
    assert_eq!(
        MembershipError::JoinInFlight(vec!["node-joining".to_owned()]).code(),
        "join_in_flight"
    );
    assert_eq!(
        MembershipError::LearnerProtocolNodeAbsent {
            nodes: vec!["node-c".to_owned()],
            minutes: 2,
        }
        .code(),
        "learner_protocol_node_absent"
    );
    assert_eq!(
        MembershipError::LearnerProtocolUpgradeRequired(vec!["node-b".to_owned()]).code(),
        "learner_protocol_upgrade_required"
    );
    // And each names what is in the way.
    assert!(
        MembershipError::JoinInFlight(vec!["node-joining".to_owned()])
            .to_string()
            .contains("node-joining")
    );
    let absent = MembershipError::LearnerProtocolNodeAbsent {
        nodes: vec!["node-c".to_owned()],
        minutes: 2,
    }
    .to_string();
    assert!(absent.contains("node-c"), "{absent}");
    assert!(absent.contains("2 minutes"), "{absent}");
}

#[test]
fn leader_change_remains_typed_without_changing_the_public_error_contract() {
    let error = MembershipError::from(hiqlite::Error::LeaderChange(
        "routing converged after election".into(),
    ));
    assert!(matches!(error, MembershipError::LeaderChanged(_)));
    assert_eq!(error.code(), "membership_internal");
    assert_eq!(
        error.to_string(),
        "cluster membership operation failed: LeaderChange: routing converged after election"
    );
}

#[test]
fn repair_observation_cannot_cross_the_generation_cas_boundary() {
    let source = production_source();
    let observation = source
        .split_once("pub async fn observe_artwork_source_repair")
        .expect("observation method")
        .1
        .split_once("pub async fn claim_artwork_source_repair")
        .expect("claim method after observation")
        .0;
    assert!(observation.contains("query_consistent_map"));
    assert!(!observation.contains(".execute("));
}

#[test]
fn advertised_host_hides_the_port_and_names_loopback() {
    assert_eq!(advertised_host("plurx-a.lan:32402"), "plurx-a.lan");
    assert_eq!(advertised_host("192.0.2.40:32402"), "192.0.2.40");
    assert_eq!(advertised_host("127.0.0.1:32402"), "localhost");
    assert_eq!(advertised_host("[::1]:32402"), "localhost");
    assert_eq!(advertised_host("[2001:db8::40]:32402"), "2001:db8::40");
    assert_eq!(advertised_host("legacy-host"), "legacy-host");
    assert_eq!(advertised_host("legacy:host"), "legacy:host");
}

#[test]
fn hostname_is_short_and_never_an_ip_address() {
    assert_eq!(
        short_hostname("living-room.example.net"),
        Some("living-room".to_owned())
    );
    assert_eq!(short_hostname("lab4.local."), Some("lab4".to_owned()));
    assert_eq!(short_hostname("192.0.2.40"), None);
    assert_eq!(membership_hostname("", "plurx-a.lan:32402"), "plurx-a");
    assert_eq!(membership_hostname("", "127.0.0.1:32402"), "unknown-host");
}

fn hostname_row(node_id: &str, hostname: &str, api_address: &str) -> NodeHostnameRow {
    NodeHostnameRow {
        node_id: node_id.to_owned(),
        hostname: hostname.to_owned(),
        api_address: api_address.to_owned(),
    }
}

#[test]
fn the_roster_names_every_node_it_can_and_omits_the_ones_it_cannot() {
    let named = roster_hostnames(
        vec![
            hostname_row("node-a", "lab3.lan", "10.42.4.7:32402"),
            hostname_row("node-b", "", "lab6.lan:32402"),
            // No reported name, and an address that is a bare loopback IP:
            // nothing here names a machine.
            hostname_row("node-c", "", "127.0.0.1:32402"),
        ],
        "node-a",
        "lab3",
    );
    assert_eq!(named.get("node-a").map(String::as_str), Some("lab3"));
    assert_eq!(named.get("node-b").map(String::as_str), Some("lab6"));
    // The point of omitting it: a caller holding "node-c" shows that id,
    // which at least identifies the machine, rather than a sentinel that
    // names every unnamed node identically.
    assert_eq!(named.get("node-c"), None);
    assert!(!named.values().any(|hostname| hostname == UNKNOWN_HOSTNAME));
}

#[test]
fn the_local_node_is_named_from_memory_before_its_heartbeat_lands() {
    // The local row is written by the heartbeat, so for the first heartbeat
    // interval after start the table has no name for this node at all.
    let named = roster_hostnames(
        vec![hostname_row("node-b", "lab6.lan", "10.42.4.14:32402")],
        "node-a",
        "lab3",
    );
    assert_eq!(named.get("node-a").map(String::as_str), Some("lab3"));
    assert_eq!(named.get("node-b").map(String::as_str), Some("lab6"));
}

#[test]
fn the_local_node_prefers_its_own_name_over_a_stale_replicated_row() {
    let named = roster_hostnames(
        vec![hostname_row("node-a", "old-name", "10.42.4.7:32402")],
        "node-a",
        "lab3",
    );
    assert_eq!(named.get("node-a").map(String::as_str), Some("lab3"));
}

#[test]
fn a_local_node_that_cannot_name_itself_is_omitted_not_sentinelled() {
    // A container whose hostname is its own truncated id, on an address
    // that reverses to nothing. Publishing "unknown-host" here would put
    // that word in the operator's Node column.
    let named = roster_hostnames(
        vec![hostname_row("node-a", "9f2c1b0a4d5e", "127.0.0.1:32402")],
        "node-a",
        UNKNOWN_HOSTNAME,
    );
    assert_eq!(named.get("node-a"), None);
    assert!(named.is_empty());
}

#[test]
fn the_hostname_read_stays_cheap_and_delegates_its_naming_rules() {
    let source = production_source();
    let accessor = source
        .split_once("pub async fn node_hostnames(")
        .expect("node_hostnames accessor")
        .1
        .split_once("\n    /// Resolve the directly observable")
        .expect("method after node_hostnames")
        .0;
    // The roster's own name table, and only the live rows in it.
    assert!(accessor.contains("LEFT JOIN cluster_node_hostnames"));
    assert!(accessor.contains("WHERE node.removed_at IS NULL"));
    // A node whose `removed_at` write did not land is fenced by its
    // pending removal row alone; without this it would be named forever.
    assert!(accessor.contains("FROM cluster_node_removals removal"));
    // The naming rules live in one tested place. An accessor that filtered
    // or defaulted inline would leave `roster_hostnames` asserting nothing
    // about what the cluster actually publishes.
    assert!(accessor.contains("roster_hostnames("));
    assert!(!accessor.contains(UNKNOWN_HOSTNAME));
    assert!(!accessor.contains("membership_hostname("));
    // The activity page polls this every few seconds. `status()` pulls Raft
    // metrics, protocol status and five joins for the same names.
    assert!(!accessor.contains("metrics_db()"));
    assert!(!accessor.contains("protocol_status()"));
    assert!(!accessor.contains("self.status()"));
}

#[test]
fn malformed_redeem_fields_are_rejected_before_any_cluster_lookup() {
    let valid = RedeemJoinRequest {
        token_digest: "a".repeat(64),
        raft_id: 4,
        node_id: "node-d".to_owned(),
        hostname: "node-d.example.net".to_owned(),
        raft_address: "node-d:32401".to_owned(),
        api_address: "node-d:32402".to_owned(),
        http_base: "http://node-d:32400".to_owned(),
        schema_version: AUTH_SCHEMA_VERSION,
        protocol_version: AUTH_PROTOCOL_MIN,
        protocol_min: AUTH_PROTOCOL_MIN,
        protocol_max: AUTH_PROTOCOL_MAX,
        sharing: Default::default(),
        live_tv_v1: true,
    };
    assert_eq!(
        validate_redeem_join_request(&valid).expect("valid request"),
        Some("http://node-d:32400".to_owned())
    );
    for malformed in [
        RedeemJoinRequest {
            token_digest: "not-a-digest".to_owned(),
            ..valid.clone()
        },
        RedeemJoinRequest {
            node_id: "x".repeat(MAX_PEER_NODE_ID_BYTES + 1),
            ..valid.clone()
        },
        RedeemJoinRequest {
            raft_address: "node-d-without-a-port".to_owned(),
            ..valid.clone()
        },
        RedeemJoinRequest {
            api_address: format!("node-d:{}", "9".repeat(MAX_JOIN_SOCKET_ADDRESS_BYTES)),
            ..valid.clone()
        },
    ] {
        assert!(matches!(
            validate_redeem_join_request(&malformed),
            Err(MembershipError::InvalidToken)
        ));
    }
    assert!(matches!(
        validate_redeem_join_request(&RedeemJoinRequest {
            http_base: format!("http://node-d/{}", "x".repeat(MAX_JOIN_HTTP_ORIGIN_BYTES)),
            ..valid
        }),
        Err(MembershipError::InvalidHttpEndpoint)
    ));

    let source = production_source();
    let redeem = source
        .split_once("pub async fn redeem(")
        .expect("redeem method")
        .1
        .split_once("async fn upsert_hostname(")
        .expect("method after redeem")
        .0;
    let cheap = redeem
        .find("validate_redeem_join_request(request)")
        .expect("cheap validation");
    let possession = redeem
        .find("token_record(&request.token_digest)")
        .expect("token lookup");
    let protocol = redeem
        .find("active_protocol_range()")
        .expect("protocol range lookup");
    assert!(cheap < possession && possession < protocol);
}

#[test]
fn docker_container_ids_are_not_machine_hostnames() {
    assert_eq!(short_hostname("1cb4bdb624dc"), None);
    assert_eq!(short_hostname(&"a".repeat(64)), None);
}

#[test]
fn reverse_dns_supplies_a_short_name_for_an_ip_only_node() {
    let hostname = membership_hostname_with_lookup("unknown-host", "10.42.4.7:32402", |address| {
        assert_eq!(address, "10.42.4.7".parse::<IpAddr>().expect("ip"));
        Some("lab3.home.arpa".to_owned())
    });

    assert_eq!(hostname, "lab3");
}

#[test]
fn reported_hostname_wins_without_a_reverse_lookup() {
    let hostname = membership_hostname_with_lookup("lab4.example.net", "10.42.4.8:32402", |_| {
        panic!("reported hostnames must not trigger reverse DNS")
    });

    assert_eq!(hostname, "lab4");
}

#[test]
fn join_credentials_and_cluster_secrets_are_redacted_from_debug() {
    let payload = payload();
    let token = encode_join_token(&payload).expect("encode token");
    let digest = join_token_digest(&token);
    let issued = IssuedJoinToken {
        token: token.clone(),
        expires_at: payload.expires_at,
        raft_id: payload.raft_id,
    };
    let redeem = RedeemJoinRequest {
        token_digest: digest.clone(),
        raft_id: payload.raft_id,
        node_id: "node-b".to_owned(),
        hostname: "node-b.example.net".to_owned(),
        raft_address: "node-b:32401".to_owned(),
        api_address: "node-b:32402".to_owned(),
        http_base: "http://node-b:32400".to_owned(),
        schema_version: payload.schema_version,
        protocol_version: payload.protocol_version,
        protocol_min: AUTH_PROTOCOL_MIN,
        protocol_max: AUTH_PROTOCOL_MAX,
        sharing: Default::default(),
        live_tv_v1: true,
    };
    let finalize = FinalizeJoinRequest {
        token_digest: digest.clone(),
        raft_id: payload.raft_id,
        node_id: "node-b".to_owned(),
    };

    for rendered in [
        format!("{issued:?}"),
        format!("{redeem:?}"),
        format!("{finalize:?}"),
        format!("{payload:?}"),
    ] {
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains(&token), "{rendered}");
        assert!(!rendered.contains(&digest), "{rendered}");
        assert!(!rendered.contains(&"r".repeat(64)), "{rendered}");
        assert!(!rendered.contains(&"a".repeat(64)), "{rendered}");
        assert!(!rendered.contains(&"c".repeat(64)), "{rendered}");
    }
}

#[test]
fn exact_internal_authority_binds_method_path_and_raw_body() {
    let nonce = "123e4567-e89b-42d3-a456-426614174000";
    let message = internal_peer_auth_message(
        "node-a",
        "node-b",
        42,
        nonce,
        "POST",
        "/internal/v1/media/offers",
        b"{}",
    )
    .expect("valid exact route");
    for changed in [
        internal_peer_auth_message(
            "node-c",
            "node-b",
            42,
            nonce,
            "POST",
            "/internal/v1/media/offers",
            b"{}",
        ),
        internal_peer_auth_message(
            "node-a",
            "node-b",
            42,
            nonce,
            "GET",
            "/internal/v1/media/offers",
            b"{}",
        ),
        internal_peer_auth_message(
            "node-a",
            "node-b",
            42,
            nonce,
            "POST",
            "/internal/v1/media/snapshot",
            b"{}",
        ),
        internal_peer_auth_message(
            "node-a",
            "node-b",
            42,
            nonce,
            "POST",
            "/internal/v1/media/offers",
            b"{ }",
        ),
        internal_peer_auth_message(
            "node-a",
            "node-b",
            42,
            "123e4567-e89b-42d3-a456-426614174001",
            "POST",
            "/internal/v1/media/offers",
            b"{}",
        ),
    ] {
        assert_ne!(message, changed.expect("valid mutation"));
    }
    assert!(internal_peer_auth_message(
        "node-a",
        "node-b",
        42,
        nonce,
        "post",
        "/internal/v1/media/offers",
        b"{}"
    )
    .is_none());
    assert!(internal_peer_auth_message(
        "node-a",
        "node-b",
        42,
        nonce,
        "POST",
        "/internal/v1/media/offers?credential=secret",
        b"{}"
    )
    .is_none());
    assert!(internal_peer_auth_message(
        "node-a",
        "node-b",
        42,
        "NOT-A-CANONICAL-UUID",
        "POST",
        "/internal/v1/media/offers",
        b"{}"
    )
    .is_none());
}

/// M7 of TRANSCODE-DECOMPOSITION-PLAN (§3.8): the `NodeLifecycle`
/// projection agrees with the predicates production decides on.
///
/// Each fixture writes one subject node's rows into the production table
/// definitions (`MEMBERSHIP_SCHEMA`'s tables plus the additive columns; its
/// triggers are left out because they need the coordinator's intent rows),
/// reads them back as the projection's row types, and checks:
/// `capability_unready_node_predicate` per node and
/// `capability_ready_predicate` over the roster against
/// `holds_back`/`capability_ready`; `NODE_TOMBSTONED_SQL` (what
/// `node_is_tombstoned` counts) against `is_tombstoned`;
/// `admitted_learner_nodes_sql` against `is_admitted_learner`;
/// `NODE_MAINTENANCE_COUNT_SQL` against a maintenance request and
/// `EXIT_MAINTENANCE_SQL` (run in a rolled-back savepoint with its other
/// preconditions made true) against `Acknowledged` on an active node; the
/// removal attempt set against `ROLLBACK_REMOVAL_FENCE_SQL` (empty),
/// `BEGIN_REMOVAL_INTENT_SQL` (membership, for every attempt id the
/// fixtures write and one they never write) and
/// `EXISTING_REMOVAL_ATTEMPT_SQL` (least attempt);
/// `NODE_PROMOTION_COUNT_SQL` against a promotion row; and, over real
/// `openraft::Membership` values including both joint shapes,
/// `committed_voter_ids!` applied to a `StoredMembership` (the read
/// `local_node_is_committed_voter` makes of Raft metrics) against
/// `is_committed_voter`, plus a hand-written membership table. The
/// promotion `Started`/`Barrier`/`Joint` split has no production reader,
/// so it is checked against §3.8's specification only. The fixture space is the product of every value the existing
/// membership tests write into those columns: role NULL, 'learner' or
/// 'voter'; removed or not; staged or not; the capability current, stale
/// or missing (and the anchor node's current or stale); no removal fence
/// or one with zero, one or two attempt references; no maintenance row,
/// requested or acknowledged; no promotion row, one without a barrier or
/// one with a barrier.
#[test]
fn lifecycle_projection_agrees() {
    use lifecycle::{
        capability_ready, committed_voter, observed_lifecycle, CapabilityRow, ClusterNodeRow,
        Maintenance, MaintenanceRow, Membership, NodeRows, Promotion, PromotionRow,
        RaftMembershipView, Removal, RemovalRow,
    };
    use rusqlite::OptionalExtension;

    const SUBJECT: &str = "subject";
    const ANCHOR: &str = "anchor";
    const SUBJECT_RAFT_ID: u64 = 2;
    const CAPABILITY_A: &str = LIVE_TV_CAPABILITY;
    const CAPABILITY_B: &str = REMOVAL_ATTEMPT_CAPABILITY;
    const TABLES: [&str; 7] = [
        "cluster_nodes",
        "cluster_node_capabilities",
        "cluster_node_join_staging",
        "cluster_node_removals",
        "cluster_node_removal_attempts",
        "cluster_node_promotions",
        "cluster_node_maintenance",
    ];

    #[derive(Clone, Copy, Debug)]
    enum Proof {
        Missing,
        Current,
        Stale,
    }

    let connection = rusqlite::Connection::open_in_memory().expect("sqlite");
    let mut tables = 0;
    for statement in MEMBERSHIP_SCHEMA
        .iter()
        .filter(|statement| statement.starts_with("CREATE TABLE"))
    {
        connection.execute(statement, []).expect("membership table");
        tables += 1;
    }
    for column in MEMBERSHIP_ADDITIVE_COLUMNS {
        connection
            .execute(column.statement, [])
            .expect("additive column");
    }
    assert!(tables >= TABLES.len(), "the schema scan found no tables");
    // `EXIT_MAINTENANCE_SQL` also refuses while the node owns an active
    // media session. That table belongs to the store schema, not this
    // module's; an empty stand-in with the three columns the statement
    // reads keeps that clause true, so the statement's verdict is the
    // maintenance row's.
    connection
        .execute(
            "CREATE TABLE media_sessions (owner_node_id TEXT, state TEXT, \
                 lease_expires_at_ms INTEGER)",
            [],
        )
        .expect("media session stand-in");

    let read_rows = |node_id: &str| {
        let node = connection
            .query_row(
                "SELECT node_id, raft_id, last_seen_at, removed_at, role \
                     FROM cluster_nodes WHERE node_id = ?1",
                [node_id],
                |row| {
                    Ok(ClusterNodeRow {
                        node_id: row.get(0)?,
                        raft_id: row.get::<_, i64>(1)? as u64,
                        last_seen_at: row.get(2)?,
                        removed_at: row.get(3)?,
                        role: row.get(4)?,
                    })
                },
            )
            .expect("node row");
        let mut statement = connection
            .prepare(
                "SELECT capability, last_seen_at FROM cluster_node_capabilities \
                     WHERE node_id = ?1 ORDER BY capability",
            )
            .expect("capability read");
        let capabilities = statement
            .query_map([node_id], |row| {
                Ok(CapabilityRow {
                    capability: row.get(0)?,
                    last_seen_at: row.get(1)?,
                })
            })
            .expect("capabilities")
            .collect::<Result<Vec<_>, _>>()
            .expect("capability rows");
        let staged = connection
            .query_row(
                "SELECT COUNT(*) FROM cluster_node_join_staging WHERE node_id = ?1",
                [node_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("staging")
            == 1;
        let removal = connection
            .query_row(
                "SELECT started_at FROM cluster_node_removals WHERE node_id = ?1",
                [node_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .expect("removal")
            .map(|started_at| {
                let mut statement = connection
                    .prepare(
                        "SELECT attempt_id FROM cluster_node_removal_attempts \
                             WHERE node_id = ?1 ORDER BY attempt_id",
                    )
                    .expect("attempt read");
                let attempt_ids = statement
                    .query_map([node_id], |row| row.get::<_, String>(0))
                    .expect("attempts")
                    .collect::<Result<Vec<_>, _>>()
                    .expect("attempt rows");
                RemovalRow {
                    started_at,
                    attempt_ids,
                }
            });
        let promotion = connection
            .query_row(
                "SELECT attempt_id, barrier_index FROM cluster_node_promotions \
                     WHERE node_id = ?1",
                [node_id],
                |row| {
                    Ok(PromotionRow {
                        attempt_id: row.get(0)?,
                        barrier_index: row.get(1)?,
                    })
                },
            )
            .optional()
            .expect("promotion");
        let maintenance = connection
            .query_row(
                "SELECT requested_at, acknowledged_at FROM cluster_node_maintenance \
                     WHERE node_id = ?1",
                [node_id],
                |row| {
                    Ok(MaintenanceRow {
                        requested_at: row.get(0)?,
                        acknowledged_at: row.get(1)?,
                    })
                },
            )
            .optional()
            .expect("maintenance");
        (node, capabilities, staged, removal, promotion, maintenance)
    };
    let node_holds_back = |node_id: &str, capability: &str| -> bool {
        connection
            .query_row(
                &format!(
                    "SELECT EXISTS (SELECT 1 FROM cluster_nodes AS active \
                         WHERE active.node_id = ?1 AND {})",
                    capability_unready_node_predicate(capability)
                ),
                [node_id],
                |row| row.get(0),
            )
            .expect("per-node readiness verdict")
    };
    let cluster_ready = |capability: &str| -> bool {
        connection
            .query_row(
                &format!("SELECT {}", capability_ready_predicate(capability)),
                [],
                |row| row.get(0),
            )
            .expect("cluster readiness verdict")
    };
    let tombstoned = |node_id: &str| -> bool {
        connection
            .query_row(NODE_TOMBSTONED_SQL, [node_id], |row| row.get::<_, i64>(0))
            .expect("tombstone count")
            == 1
    };
    let learner_roster = || -> BTreeSet<String> {
        let mut statement = connection
            .prepare(admitted_learner_nodes_sql())
            .expect("learner roster");
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("roster")
            .collect::<Result<BTreeSet<_>, _>>()
            .expect("roster rows")
    };
    // Production writes that decide on maintenance and on the removal
    // attempt set, run as production runs them, inside a savepoint that is
    // always rolled back so the fixture is unchanged. Each returns whether
    // the statement changed a row.
    let rolled_back = |write: &dyn Fn() -> usize| -> bool {
        connection
            .execute_batch("SAVEPOINT probe")
            .expect("savepoint");
        let changed = write();
        connection
            .execute_batch("ROLLBACK TO probe; RELEASE probe")
            .expect("roll the probe back");
        changed == 1
    };
    // `EXIT_MAINTENANCE_SQL` deletes only an acknowledged request
    // (`acknowledged_at IS NOT NULL`) of an active node. Every other
    // precondition it checks (fresh heartbeat and progress, zero apply
    // lag, the maintenance capability proven by the running binary, no
    // active media session) is made true first, so the delete answers
    // "acknowledged and active" and nothing else.
    let exit_maintenance_deletes = |node_id: &str| -> bool {
        rolled_back(&|| {
            connection
                .execute(
                    "INSERT INTO cluster_node_progress (node_id, current_term, \
                         apply_lag_entries, bounded_read_ready, voter_storage_ready, \
                         voter_role_persisted, observed_at) VALUES (?1, 1, 0, 1, 1, 1, 0)",
                    [node_id],
                )
                .expect("progress row");
            connection
                .execute(
                    "INSERT INTO cluster_node_capabilities (node_id, capability, \
                         last_seen_at) SELECT node_id, ?2, last_seen_at FROM cluster_nodes \
                         WHERE node_id = ?1",
                    rusqlite::params![node_id, NODE_MAINTENANCE_CAPABILITY],
                )
                .expect("maintenance capability");
            connection
                .execute(
                    EXIT_MAINTENANCE_SQL,
                    rusqlite::params![node_id, 0_i64, NODE_MAINTENANCE_CAPABILITY, 0_i64],
                )
                .expect("exit maintenance")
        })
    };
    let maintenance_requested = |node_id: &str| -> bool {
        connection
            .query_row(NODE_MAINTENANCE_COUNT_SQL, [node_id], |row| {
                row.get::<_, i64>(0)
            })
            .expect("maintenance count")
            == 1
    };
    // `ROLLBACK_REMOVAL_FENCE_SQL` drops the fence only when no attempt
    // reference remains: the empty-set read.
    let fence_rolls_back = |node_id: &str| -> bool {
        rolled_back(&|| {
            connection
                .execute(ROLLBACK_REMOVAL_FENCE_SQL, [node_id])
                .expect("fence rollback")
        })
    };
    // `BEGIN_REMOVAL_INTENT_SQL` records an intent only for an attempt the
    // set holds: the membership read.
    let intent_admits = |node_id: &str, attempt: &str| -> bool {
        rolled_back(&|| {
            connection
                .execute(
                    BEGIN_REMOVAL_INTENT_SQL,
                    rusqlite::params![node_id, attempt],
                )
                .expect("removal intent")
        })
    };
    let existing_attempt = |node_id: &str| -> Option<String> {
        connection
            .query_row(EXISTING_REMOVAL_ATTEMPT_SQL, [node_id], |row| {
                row.get::<_, String>(0)
            })
            .optional()
            .expect("existing attempt")
    };
    let promotion_present = |node_id: &str| -> bool {
        connection
            .query_row(NODE_PROMOTION_COUNT_SQL, [node_id], |row| {
                row.get::<_, i64>(0)
            })
            .expect("promotion count")
            > 0
    };

    // Committed Raft configurations the subject (raft id 2) can be in,
    // built with openraft's own constructor, with the membership each
    // must project to.
    let raft_shapes: Vec<(&str, openraft::Membership<u64, Node>, Membership)> = vec![
        (
            "absent",
            openraft::Membership::new(vec![BTreeSet::from([1])], ()),
            Membership::Absent,
        ),
        (
            "learner",
            openraft::Membership::new(vec![BTreeSet::from([1])], BTreeSet::from([SUBJECT_RAFT_ID])),
            Membership::Learner,
        ),
        (
            "voter",
            openraft::Membership::new(vec![BTreeSet::from([1, SUBJECT_RAFT_ID])], ()),
            Membership::Voter,
        ),
        (
            "joint, entering",
            openraft::Membership::new(
                vec![BTreeSet::from([1]), BTreeSet::from([1, SUBJECT_RAFT_ID])],
                (),
            ),
            Membership::Voter,
        ),
        (
            "joint, leaving",
            openraft::Membership::new(
                vec![BTreeSet::from([1, SUBJECT_RAFT_ID]), BTreeSet::from([1])],
                (),
            ),
            Membership::Voter,
        ),
    ];

    let mut fixtures = 0usize;
    let mut seen_tombstoned = [false; 2];
    let mut seen_holds_back = [false; 2];
    let mut seen_learner = [false; 2];
    let mut seen_acknowledged = [false; 2];
    let mut seen_fence_rollback = [false; 2];
    let mut seen_attempt_admitted = [false; 2];
    let mut seen_promotion = [false; 2];
    for role in [None, Some("learner"), Some("voter")] {
        for removed_at in [None, Some(9_i64)] {
            for staged in [false, true] {
                for subject_proof in [Proof::Missing, Proof::Current, Proof::Stale] {
                    for anchor_proof in [Proof::Current, Proof::Stale] {
                        for attempts in [None, Some(0), Some(1), Some(2)] {
                            for maintenance in [None, Some(None), Some(Some(8_i64))] {
                                for barrier in [None, Some(None), Some(Some(7_i64))] {
                                    for table in TABLES {
                                        connection
                                            .execute(&format!("DELETE FROM {table}"), [])
                                            .expect("reset fixture");
                                    }
                                    connection
                                        .execute(
                                            "INSERT INTO cluster_nodes (node_id, raft_id, \
                                                 raft_address, api_address, last_seen_at, \
                                                 removed_at, role) VALUES \
                                                 (?1, 1, 'r1', 'a1', 3, NULL, NULL), \
                                                 (?2, ?3, 'r2', 'a2', 5, ?4, ?5)",
                                            rusqlite::params![
                                                ANCHOR,
                                                SUBJECT,
                                                SUBJECT_RAFT_ID as i64,
                                                removed_at,
                                                role
                                            ],
                                        )
                                        .expect("node rows");
                                    let anchor_seen = match anchor_proof {
                                        Proof::Current => 3,
                                        _ => 2,
                                    };
                                    for capability in [CAPABILITY_A, CAPABILITY_B] {
                                        connection
                                            .execute(
                                                "INSERT INTO cluster_node_capabilities \
                                                     VALUES (?1, ?2, ?3)",
                                                rusqlite::params![ANCHOR, capability, anchor_seen],
                                            )
                                            .expect("anchor capability");
                                    }
                                    let subject_seen = match subject_proof {
                                        Proof::Missing => None,
                                        Proof::Current => Some(5),
                                        Proof::Stale => Some(4),
                                    };
                                    if let Some(seen) = subject_seen {
                                        connection
                                            .execute(
                                                "INSERT INTO cluster_node_capabilities \
                                                     VALUES (?1, ?2, ?3)",
                                                rusqlite::params![SUBJECT, CAPABILITY_A, seen],
                                            )
                                            .expect("subject capability");
                                    }
                                    if staged {
                                        connection
                                            .execute(
                                                "INSERT INTO cluster_node_join_staging \
                                                     VALUES (?1)",
                                                [SUBJECT],
                                            )
                                            .expect("staging row");
                                    }
                                    if let Some(count) = attempts {
                                        connection
                                            .execute(
                                                "INSERT INTO cluster_node_removals \
                                                     VALUES (?1, 6)",
                                                [SUBJECT],
                                            )
                                            .expect("removal fence");
                                        for attempt in 0..count {
                                            connection
                                                .execute(
                                                    "INSERT INTO \
                                                         cluster_node_removal_attempts \
                                                         VALUES (?1, ?2)",
                                                    rusqlite::params![
                                                        SUBJECT,
                                                        format!("attempt-{attempt}")
                                                    ],
                                                )
                                                .expect("removal attempt");
                                        }
                                    }
                                    if let Some(acknowledged_at) = maintenance {
                                        connection
                                            .execute(
                                                "INSERT INTO cluster_node_maintenance \
                                                     VALUES (?1, 7, ?2)",
                                                rusqlite::params![SUBJECT, acknowledged_at],
                                            )
                                            .expect("maintenance row");
                                    }
                                    if let Some(barrier_index) = barrier {
                                        connection
                                            .execute(
                                                "INSERT INTO cluster_node_promotions \
                                                     VALUES (?1, 'promotion', ?2, 6)",
                                                rusqlite::params![SUBJECT, barrier_index],
                                            )
                                            .expect("promotion row");
                                    }

                                    for (shape, raft, expected) in &raft_shapes {
                                        let view = RaftMembershipView::new(
                                            raft.voter_ids(),
                                            raft.nodes().map(|(id, _)| *id),
                                            raft.get_joint_config().len() > 1,
                                        );
                                        let project = |node_id: &str| {
                                            let (
                                                node,
                                                capabilities,
                                                staged,
                                                removal,
                                                promotion,
                                                maintenance,
                                            ) = read_rows(node_id);
                                            observed_lifecycle(
                                                NodeRows {
                                                    node: &node,
                                                    capabilities: &capabilities,
                                                    staged,
                                                    removal: removal.as_ref(),
                                                    promotion: promotion.as_ref(),
                                                    maintenance: maintenance.as_ref(),
                                                },
                                                &view,
                                            )
                                        };
                                        let views = [project(ANCHOR), project(SUBJECT)];
                                        let context = format!(
                                            "raft {shape}, role {role:?}, removed \
                                                 {removed_at:?}, staged {staged}, subject \
                                                 {subject_proof:?}, anchor {anchor_proof:?}, \
                                                 attempts {attempts:?}, maintenance \
                                                 {maintenance:?}, promotion {barrier:?}: {:?}",
                                            views[1]
                                        );
                                        let roster = learner_roster();
                                        for (index, lifecycle) in views.iter().enumerate() {
                                            for capability in [CAPABILITY_A, CAPABILITY_B] {
                                                let holds = lifecycle.holds_back(capability);
                                                assert_eq!(
                                                    holds,
                                                    node_holds_back(&lifecycle.node_id, capability),
                                                    "{} holds back {capability}: {context}",
                                                    lifecycle.node_id
                                                );
                                                seen_holds_back[usize::from(holds)] = true;
                                            }
                                            let dead = lifecycle.is_tombstoned();
                                            assert_eq!(
                                                dead,
                                                tombstoned(&lifecycle.node_id),
                                                "{} tombstoned: {context}",
                                                lifecycle.node_id
                                            );
                                            seen_tombstoned[usize::from(dead)] = true;
                                            let learner = lifecycle.is_admitted_learner();
                                            assert_eq!(
                                                learner,
                                                roster.contains(&lifecycle.node_id),
                                                "{} admitted learner: {context}",
                                                lifecycle.node_id
                                            );
                                            if index == 1 {
                                                seen_learner[usize::from(learner)] = true;
                                            }
                                            let node_id = lifecycle.node_id.as_str();
                                            assert_eq!(
                                                lifecycle.maintenance != Maintenance::None,
                                                maintenance_requested(node_id),
                                                "{node_id} maintenance requested: {context}"
                                            );
                                            let exits = lifecycle.maintenance
                                                == Maintenance::Acknowledged
                                                && lifecycle.removal != Removal::Tombstoned;
                                            assert_eq!(
                                                exits,
                                                exit_maintenance_deletes(node_id),
                                                "{node_id} maintenance acknowledged: {context}"
                                            );
                                            seen_acknowledged[usize::from(exits)] = true;
                                            // A tombstoned node's attempt references
                                            // are left behind on purpose and no
                                            // removal read consults them again, so
                                            // `Tombstoned` carries no attempt set.
                                            let attempts = match &lifecycle.removal {
                                                Removal::Tombstoned => None,
                                                Removal::None => Some((false, BTreeSet::new())),
                                                Removal::InProgress { attempts } => {
                                                    Some((true, attempts.clone()))
                                                }
                                            };
                                            if let Some((fence, attempts)) = attempts {
                                                let rollback = fence && attempts.is_empty();
                                                assert_eq!(
                                                    rollback,
                                                    fence_rolls_back(node_id),
                                                    "{node_id} removal fence rolls back: \
                                                         {context}"
                                                );
                                                seen_fence_rollback[usize::from(rollback)] = true;
                                                for attempt in [
                                                    "attempt-0",
                                                    "attempt-1",
                                                    "attempt-2",
                                                    "attempt-never-written",
                                                ] {
                                                    let held = attempts.contains(attempt);
                                                    assert_eq!(
                                                        held,
                                                        intent_admits(node_id, attempt),
                                                        "{node_id} holds {attempt}: {context}"
                                                    );
                                                    seen_attempt_admitted[usize::from(held)] = true;
                                                }
                                                assert_eq!(
                                                    attempts.first().cloned(),
                                                    existing_attempt(node_id),
                                                    "{node_id} adopted attempt: {context}"
                                                );
                                            }
                                            let promoting = lifecycle.promotion != Promotion::None;
                                            assert_eq!(
                                                promoting,
                                                promotion_present(node_id),
                                                "{node_id} promotion row: {context}"
                                            );
                                            seen_promotion[usize::from(promoting)] = true;
                                        }
                                        for capability in [CAPABILITY_A, CAPABILITY_B] {
                                            assert_eq!(
                                                capability_ready(&views, capability),
                                                cluster_ready(capability),
                                                "cluster ready for {capability}: {context}"
                                            );
                                        }
                                        let subject = &views[1];
                                        assert_eq!(subject.membership, *expected, "{context}");
                                        // The Raft read production decides on:
                                        // `local_node_is_committed_voter` applies
                                        // `committed_voter_ids!` to the metrics'
                                        // `Arc<StoredMembership>`; the projection
                                        // reads its own `RaftMembershipView`.
                                        let stored = Arc::new(openraft::StoredMembership::new(
                                            None,
                                            raft.clone(),
                                        ));
                                        assert_eq!(
                                            subject.is_committed_voter(),
                                            committed_voter(
                                                committed_voter_ids!(stored),
                                                SUBJECT_RAFT_ID
                                            ),
                                            "committed voter as \
                                                 local_node_is_committed_voter reads it: \
                                                 {context}"
                                        );
                                        // `Started` / `Barrier` / `Joint` have no
                                        // production reader (only the row's existence
                                        // is read, checked above against
                                        // `NODE_PROMOTION_COUNT_SQL`). This is §3.8's
                                        // specification of the split, not an
                                        // agreement with production.
                                        let joint = raft.get_joint_config().len() > 1;
                                        match barrier {
                                            None => assert_eq!(
                                                subject.promotion,
                                                Promotion::None,
                                                "{context}"
                                            ),
                                            Some(_) if joint => assert_eq!(
                                                subject.promotion,
                                                Promotion::Joint,
                                                "the joint interval is never collapsed: \
                                                     {context}"
                                            ),
                                            Some(None) => assert_eq!(
                                                subject.promotion,
                                                Promotion::Started,
                                                "{context}"
                                            ),
                                            Some(Some(index)) => assert_eq!(
                                                subject.promotion,
                                                Promotion::Barrier { index },
                                                "{context}"
                                            ),
                                        }
                                        fixtures += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(fixtures, 3 * 2 * 2 * 3 * 2 * 4 * 3 * 3 * 5);
    assert_eq!(
        (
            seen_holds_back,
            seen_tombstoned,
            seen_learner,
            seen_acknowledged,
            seen_fence_rollback,
            seen_attempt_admitted,
            seen_promotion,
        ),
        ([true; 2], [true; 2], [true; 2], [true; 2], [true; 2], [true; 2], [true; 2]),
        "every verdict has to be reached both ways, or the agreement is vacuous"
    );
}
