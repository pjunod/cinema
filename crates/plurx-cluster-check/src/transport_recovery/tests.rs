use super::*;

fn write(ordinal: u32, at: i64) -> AcknowledgedRecoveryWrite {
    AcknowledgedRecoveryWrite {
        key: format!("cluster.transport-recovery.learner.01.{ordinal:06}"),
        value_sha256: "b".repeat(64),
        acknowledged_at_unix_ms: at,
    }
}

fn role_campaign_with_cycles(
    role: RecoveryRole,
    minimum: u64,
    cycles_required: u32,
) -> RecoveryRoleCampaign {
    let marker = format!("transport-recovery-{}-image-v1", role.label());
    let cycles = (1..=cycles_required)
        .map(|cycle| {
            let writes = vec![
                write(cycle.saturating_mul(3), 1_500),
                write(cycle.saturating_mul(3).saturating_add(1), 2_500),
                write(cycle.saturating_mul(3).saturating_add(2), 3_500),
            ];
            let digest = acknowledged_write_digest(&writes);
            let snapshot = SnapshotFileEvidence {
                snapshot_id: format!("00000000-0000-7000-8000-{cycle:012}"),
                bytes: minimum,
                sha256: "a".repeat(64),
            };
            RecoveryCycleEvidence {
                role,
                cycle,
                recovery_started_at_unix_ms: 2_000,
                recovery_finished_at_unix_ms: 7_000,
                snapshot_index: u64::from(cycle) * 300,
                purged_index: u64::from(cycle) * 300,
                applied_index: u64::from(cycle) * 300 + 1,
                source_snapshot: snapshot.clone(),
                installed_snapshot: snapshot,
                image: RecoveryImageEvidence {
                    marker: marker.clone(),
                    minimum_sqlite_bytes: minimum,
                    rows: 24,
                    payload_bytes: minimum,
                    content_sha256: "c".repeat(64),
                },
                source_outbound: SourceOutboundTransportEvidence {
                    observing_node_id: 1,
                    target_node_id: TARGET_NODE,
                    attempt_id: u64::from(cycle),
                    attempt_started_at_unix_ms: 2_100,
                    completed_at_unix_ms: 6_000,
                    attempted_bytes: minimum,
                    acknowledged_bytes: minimum,
                    total_bytes: minimum,
                    attempts: 1,
                    reconnect_count: 0,
                    retry_count: 0,
                    elapsed_millis: 3_900,
                },
                target_inbound: TargetInboundTransportEvidence {
                    observing_node_id: TARGET_NODE,
                    source_node_id: 1,
                    attempt_id: u64::from(cycle),
                    receive_started_at_unix_ms: 2_200,
                    last_local_receive_at_unix_ms: 5_510,
                    install_started_at_unix_ms: 5_000,
                    install_completed_at_unix_ms: 5_500,
                    received_bytes: minimum,
                    total_bytes: minimum,
                    transfer_millis: 2_800,
                    install_millis: 500,
                },
                recovery_millis: 5_000,
                acknowledged_writes: writes,
                acknowledged_write_digest: digest.clone(),
                target_write_digest: digest,
                node_resources: (1..=TARGET_NODE)
                    .map(|node_id| RecoveryNodeResourceEvidence {
                        node_id,
                        kind: if node_id == TARGET_NODE {
                            RecoveryResourceNodeKind::RestartedTarget
                        } else {
                            RecoveryResourceNodeKind::PersistentSourceVoter
                        },
                        baseline: ProcessResourceCount {
                            threads: 12,
                            sockets: 8,
                            owned_async_tasks: 10,
                        },
                        post_quiescence: ProcessResourceCount {
                            threads: 12,
                            sockets: 8,
                            owned_async_tasks: 10,
                        },
                    })
                    .collect(),
            }
        })
        .collect::<Vec<_>>();
    RecoveryRoleCampaign {
        role,
        cycles_required,
        minimum_sqlite_bytes: minimum,
        worst_durations: worst_durations(&cycles),
        resource_envelopes: resource_envelopes(&cycles).expect("fixture envelopes"),
        cycles,
    }
}

fn role_campaign(role: RecoveryRole, minimum: u64) -> RecoveryRoleCampaign {
    role_campaign_with_cycles(role, minimum, TRANSPORT_RECOVERY_CYCLES_PER_ROLE)
}

/// Rewrite one node's post-quiescence series across a campaign and
/// re-derive the floors from it, the way the harness would have recorded
/// them. `series(cycle)` is the count for `cycle` in `1..=20`.
fn with_node_series(
    campaign: &mut RecoveryRoleCampaign,
    node_index: usize,
    series: impl Fn(u32) -> ProcessResourceCount,
) {
    for cycle in &mut campaign.cycles {
        cycle.node_resources[node_index].post_quiescence = series(cycle.cycle);
    }
    campaign.resource_envelopes =
        resource_envelopes(&campaign.cycles).expect("rewritten envelopes");
}

fn counts(threads: u64, sockets: u64, owned_async_tasks: u64) -> ProcessResourceCount {
    ProcessResourceCount {
        threads,
        sockets,
        owned_async_tasks,
    }
}

fn artifact() -> ClusterTransportRecoveryArtifact {
    ClusterTransportRecoveryArtifact {
        schema_version: TRANSPORT_RECOVERY_ARTIFACT_SCHEMA_VERSION,
        evidence_scope: EVIDENCE_SCOPE.to_owned(),
        build_sha: "d".repeat(40),
        platform: "linux".to_owned(),
        started_at_unix_ms: 1_000,
        finished_at_unix_ms: 10_000,
        snapshot_policy: RecoverySnapshotPolicy {
            validation_only: true,
            logs_since_last: TRANSPORT_RECOVERY_SNAPSHOT_LOGS_SINCE_LAST,
            production_default_unchanged: true,
        },
        transport: RecoveryTransportContract {
            raft_transport: "hiqlite_production_tls_chunked_snapshot".to_owned(),
            api_writer_transport: "hiqlite_production_tls_websocket".to_owned(),
            tls_enabled: true,
            tls_certificate_verification_disabled_for_self_signed_harness: true,
            separate_node_processes: true,
            separate_writer_process: true,
        },
        resource_thread_envelope_allowance: THREAD_ENVELOPE_ALLOWANCE,
        resource_socket_envelope_allowance: SOCKET_ENVELOPE_ALLOWANCE,
        resource_owned_async_task_envelope_allowance: OWNED_ASYNC_TASK_ENVELOPE_ALLOWANCE,
        resource_cleanup_horizon_millis: duration_millis(RESOURCE_CLEANUP_HORIZON),
        resource_stable_samples: RESOURCE_STABLE_SAMPLES,
        learner: role_campaign(RecoveryRole::Learner, TRANSPORT_RECOVERY_SMALL_IMAGE_BYTES),
        voter: role_campaign(RecoveryRole::Voter, TRANSPORT_RECOVERY_LARGE_IMAGE_BYTES),
    }
}

fn role_report(role: RecoveryRole, cycles_required: u32) -> TransportRecoveryRoleReport {
    let campaign = role_campaign_with_cycles(role, role.minimum_sqlite_bytes(), cycles_required);
    TransportRecoveryRoleReport {
        schema_version: TRANSPORT_RECOVERY_ROLE_REPORT_SCHEMA_VERSION,
        kind: ROLE_REPORT_KIND.to_owned(),
        execution_id: "test-execution-1".to_owned(),
        build_sha: "d".repeat(40),
        platform: "linux".to_owned(),
        build_profile: build_profile_descriptor(),
        started_at_unix_ms: 1_000,
        finished_at_unix_ms: 10_000,
        scope: role_report_scope(cycles_required),
        resource_envelope_asserted: envelope_is_asserted(&campaign.resource_envelopes),
        snapshot_policy: fixed_snapshot_policy(),
        transport: fixed_transport_contract(),
        resource_thread_envelope_allowance: THREAD_ENVELOPE_ALLOWANCE,
        resource_socket_envelope_allowance: SOCKET_ENVELOPE_ALLOWANCE,
        resource_owned_async_task_envelope_allowance: OWNED_ASYNC_TASK_ENVELOPE_ALLOWANCE,
        resource_cleanup_horizon_millis: duration_millis(RESOURCE_CLEANUP_HORIZON),
        resource_stable_samples: RESOURCE_STABLE_SAMPLES,
        campaign,
    }
}

#[test]
fn build_profile_descriptor_names_the_compiled_profile_and_assertions() {
    let descriptor = build_profile_descriptor();
    assert!(descriptor.starts_with("profile="), "{descriptor}");
    assert!(descriptor.contains(";opt-level="), "{descriptor}");
    assert!(descriptor.contains(";debug="), "{descriptor}");
    let assertion = if cfg!(debug_assertions) { "on" } else { "off" };
    assert!(
        descriptor.ends_with(&format!(";debug-assertions={assertion}")),
        "{descriptor}"
    );
}

fn role_plan(role: RecoveryRole, root: &Path) -> RecoveryRolePlan {
    RecoveryRolePlan {
        role,
        cycles_required: 3,
        execution_id: "test-execution-1".to_owned(),
        output: root.join(format!("{}-role.json", role.label())),
        diagnostics_dir: root.join(format!("{}-diagnostics", role.label())),
        max_runtime_seconds: None,
    }
}

fn empty_cleanup_summary() -> CleanupSummary {
    CleanupSummary {
        attempted: false,
        budget_millis: duration_millis(CLEANUP_BUDGET),
        registered_processes: 0,
        reaped_processes: 0,
        surviving_processes: Vec::new(),
    }
}

fn create_test_diagnostics(plan: &RecoveryRolePlan) -> RecoveryDiagnostics {
    std::fs::create_dir(&plan.diagnostics_dir).expect("create diagnostics directory");
    RecoveryDiagnostics::create(
        plan,
        &"d".repeat(40),
        &build_profile_descriptor(),
        diagnostic_contracts(plan.role),
        1,
    )
    .expect("create diagnostics")
}

#[test]
fn complete_twenty_plus_twenty_artifact_is_accepted() {
    validate_transport_recovery_artifact(&artifact()).expect("valid recovery artifact");
}

#[test]
fn role_cli_requires_every_explicit_input_and_rejects_ambiguous_paths() {
    let valid = vec![
        "--role".to_owned(),
        "learner".to_owned(),
        "--cycles".to_owned(),
        "3".to_owned(),
        "--execution-id".to_owned(),
        "run_1.ok".to_owned(),
        "--output".to_owned(),
        "target/role.json".to_owned(),
        "--diagnostics-dir".to_owned(),
        "target/diagnostics".to_owned(),
        "--max-runtime-seconds".to_owned(),
        "30".to_owned(),
    ];
    let plan = parse_recovery_role_plan(&valid).expect("valid role plan");
    assert_eq!(plan.role, RecoveryRole::Learner);
    assert_eq!(plan.cycles_required, 3);
    assert_eq!(plan.max_runtime_seconds, Some(30));

    for invalid in [
        vec![],
        vec!["--unknown".to_owned(), "x".to_owned()],
        vec!["--role".to_owned()],
        [valid.clone(), vec!["--role".to_owned(), "voter".to_owned()]].concat(),
    ] {
        assert!(parse_recovery_role_plan(&invalid).is_err(), "{invalid:?}");
    }
    for (position, replacement) in [(1, "unsupported"), (3, "0"), (5, "bad/path"), (11, "0")] {
        let mut invalid = valid.clone();
        invalid[position] = replacement.to_owned();
        assert!(parse_recovery_role_plan(&invalid).is_err(), "{invalid:?}");
    }
    let mut colliding = valid;
    colliding[9] = "target/role.json/diagnostics".to_owned();
    assert!(parse_recovery_role_plan(&colliding).is_err());
}

#[test]
fn assembly_cli_is_strict_and_refuses_path_collisions() {
    let valid = vec![
        "--execution-id".to_owned(),
        "paired-1".to_owned(),
        "--voter".to_owned(),
        "voter.json".to_owned(),
        "--learner".to_owned(),
        "learner.json".to_owned(),
        "--output".to_owned(),
        "combined.json".to_owned(),
    ];
    assert!(parse_recovery_assemble_plan(&valid).is_ok());
    for invalid in [
        vec![],
        vec!["--unexpected".to_owned(), "x".to_owned()],
        [
            valid.clone(),
            vec!["--voter".to_owned(), "another.json".to_owned()],
        ]
        .concat(),
    ] {
        assert!(parse_recovery_assemble_plan(&invalid).is_err());
    }
    let mut colliding = valid;
    colliding[7] = "voter.json".to_owned();
    assert!(parse_recovery_assemble_plan(&colliding).is_err());
}

#[test]
fn bounded_role_reports_derive_scope_and_resource_assertion() {
    for (cycles, scope, asserted) in [
        (1, RecoveryRoleReportScope::Smoke, false),
        (18, RecoveryRoleReportScope::Smoke, false),
        (19, RecoveryRoleReportScope::Smoke, true),
        (20, RecoveryRoleReportScope::QualificationRole, true),
    ] {
        let report = role_report(RecoveryRole::Voter, cycles);
        assert_eq!(report.scope, scope);
        assert_eq!(report.resource_envelope_asserted, asserted);
        validate_transport_recovery_role_report(&report)
            .unwrap_or_else(|error| panic!("{cycles}-cycle report failed: {error:#}"));
    }
}

fn pending_snapshot_status(snapshot_index: Option<u64>) -> RecoveryRuntimeStatus {
    RecoveryRuntimeStatus {
        last_log_index: Some(1000),
        snapshot_index,
        purged_index: None,
        applied_index: Some(1000),
        install_ok_count: 0,
        last_install_observed_at_unix_ms: None,
        last_install_elapsed_nanos: None,
        transport: hiqlite::SnapshotTransportStatus {
            schema_version: 1,
            observing_node_id: 1,
            observed_at_unix_ms: 0,
            owned_async_tasks: 0,
            observations: Vec::new(),
            local_request_started_at: None,
            local_receipt_at: None,
        },
    }
}

#[tokio::test]
async fn delayed_snapshot_publication_finishes_without_additional_writes() {
    let mut writes = Vec::new();
    let mut observations = [None, Some(41), Some(40), Some(42)].into_iter();
    let mut publication_polls = 0;
    let status = await_triggered_snapshot(
        Some(41),
        Instant::now() + Duration::from_secs(5),
        async |ordinal| {
            if let Some(ordinal) = ordinal {
                assert_eq!(publication_polls, 0, "writes restarted during publication");
                writes.push(ordinal);
                Ok(pending_snapshot_status(Some(41)))
            } else {
                publication_polls += 1;
                Ok(pending_snapshot_status(
                    observations.next().expect("bounded observations"),
                ))
            }
        },
    )
    .await
    .expect("asynchronous publication can finish after the bounded write burst");
    assert_eq!(status.snapshot_index, Some(42));
    assert_eq!(publication_polls, 4);
    assert_eq!(writes, (0..768).collect::<Vec<_>>());
}

#[tokio::test]
async fn unchanged_snapshot_expires_under_the_original_deadline() {
    let deadline = Instant::now() + Duration::from_millis(750);
    let mut writes = Vec::new();
    let mut publication_polls = 0;
    let error = await_triggered_snapshot(Some(41), deadline, async |ordinal| {
        if let Some(ordinal) = ordinal {
            assert_eq!(publication_polls, 0, "expired work must not restart writes");
            writes.push(ordinal);
        } else {
            publication_polls += 1;
        }
        Ok(pending_snapshot_status(Some(41)))
    })
    .await
    .expect_err("an unchanged completed snapshot cannot renew the deadline");
    assert!(error.to_string().contains("snapshot publication"));
    assert!(Instant::now() >= deadline);
    assert!(publication_polls > 0);
    assert_eq!(writes, (0..768).collect::<Vec<_>>());
}

#[test]
fn retained_identity_requires_a_snapshot_beyond_its_shutdown_watermark() {
    let evidence = snapshot_purge_evidence(1_279, 1_279).expect("observed purge");
    prove_retained_identity_requires_snapshot(&evidence, 1_278)
        .expect("retained logs cannot cover the purged prefix");
}

#[test]
fn retained_identity_rejects_equal_or_newer_retained_logs() {
    let evidence = snapshot_purge_evidence(1_279, 1_279).expect("observed purge");
    assert!(prove_retained_identity_requires_snapshot(&evidence, 1_279).is_err());
    assert!(prove_retained_identity_requires_snapshot(&evidence, 1_280).is_err());
}

#[test]
fn retained_identity_rejects_a_purge_that_has_not_crossed_its_watermark() {
    let evidence = SnapshotPurgeEvidence {
        snapshot_index: 1_280,
        purged_index: 1_279,
    };
    assert!(prove_retained_identity_requires_snapshot(&evidence, 1_279).is_err());
}

#[test]
fn purge_evidence_pins_the_boundary_that_was_awaited() {
    let evidence = snapshot_purge_evidence(1_278, 1_278).expect("awaited purge boundary");
    assert_eq!(evidence.snapshot_index, 1_278);
    assert_eq!(evidence.purged_index, 1_278);
    assert!(snapshot_purge_evidence(1_279, 1_278).is_err());
}

#[test]
fn cross_process_timestamp_tolerance_is_bounded() {
    assert!(!timestamp_precedes_beyond_tolerance(998, 1_000));
    assert!(timestamp_precedes_beyond_tolerance(997, 1_000));
    assert!(!timestamp_follows_beyond_tolerance(1_002, 1_000));
    assert!(timestamp_follows_beyond_tolerance(1_003, 1_000));
}

#[test]
fn role_report_refuses_identity_contract_and_cycle_drift() {
    let valid = role_report(RecoveryRole::Voter, 3);
    for mutate in [
        |value: &mut TransportRecoveryRoleReport| value.execution_id = "bad/id".to_owned(),
        |value: &mut TransportRecoveryRoleReport| value.build_sha = "x".repeat(40),
        |value: &mut TransportRecoveryRoleReport| value.build_profile.clear(),
        |value: &mut TransportRecoveryRoleReport| {
            value.scope = RecoveryRoleReportScope::QualificationRole
        },
        |value: &mut TransportRecoveryRoleReport| value.resource_envelope_asserted = true,
        |value: &mut TransportRecoveryRoleReport| value.transport.separate_writer_process = false,
        |value: &mut TransportRecoveryRoleReport| value.campaign.cycles[0].cycle = 2,
    ] {
        let mut value = valid.clone();
        mutate(&mut value);
        assert!(validate_transport_recovery_role_report(&value).is_err());
    }
}

#[test]
fn role_report_bytes_reject_unknown_top_level_and_campaign_fields() {
    for path in ["unexpected", "campaign.unexpected"] {
        let mut value = serde_json::to_value(role_report(RecoveryRole::Learner, 3))
            .expect("serialize role report");
        if path == "unexpected" {
            value["unexpected"] = serde_json::json!(true);
        } else {
            value["campaign"]["unexpected"] = serde_json::json!(true);
        }
        let bytes = serde_json::to_vec(&value).expect("encode mutated role report");
        assert!(validate_transport_recovery_role_bytes(&bytes).is_err());
    }
}

#[test]
fn assembler_requires_two_qualified_reports_from_one_execution() {
    let root = tempfile::tempdir().expect("assembly root");
    let voter_path = root.path().join("voter.json");
    let learner_path = root.path().join("learner.json");
    let output = root.path().join("combined.json");
    let build_sha = resolve_build_sha().expect("test checkout SHA");
    let mut voter = role_report(RecoveryRole::Voter, 20);
    voter.build_sha.clone_from(&build_sha);
    let mut learner = role_report(RecoveryRole::Learner, 20);
    learner.build_sha.clone_from(&build_sha);
    std::fs::write(
        &voter_path,
        serde_json::to_vec(&voter).expect("encode voter"),
    )
    .expect("write voter");
    std::fs::write(
        &learner_path,
        serde_json::to_vec(&learner).expect("encode learner"),
    )
    .expect("write learner");
    assemble_transport_recovery_reports(RecoveryAssemblePlan {
        execution_id: "test-execution-1".to_owned(),
        voter: voter_path.clone(),
        learner: learner_path.clone(),
        output: output.clone(),
    })
    .expect("assemble qualified reports");
    validate_transport_recovery_bytes(&std::fs::read(&output).expect("read artifact"))
        .expect("validate assembled canonical artifact");

    voter = role_report(RecoveryRole::Voter, 3);
    voter.build_sha = build_sha;
    std::fs::write(
        &voter_path,
        serde_json::to_vec(&voter).expect("encode smoke voter"),
    )
    .expect("write smoke voter");
    std::fs::write(&output, b"stale success").expect("seed stale output");
    assert!(assemble_transport_recovery_reports(RecoveryAssemblePlan {
        execution_id: "test-execution-1".to_owned(),
        voter: voter_path,
        learner: learner_path,
        output: output.clone(),
    })
    .is_err());
    assert!(!output.exists(), "failed assembly retained stale success");
}

#[test]
fn assembler_refuses_valid_reports_from_another_role_run_or_source() {
    let root = tempfile::tempdir().expect("assembly mismatch root");
    let voter_path = root.path().join("voter.json");
    let learner_path = root.path().join("learner.json");
    let output = root.path().join("combined.json");
    let build_sha = resolve_build_sha().expect("test checkout SHA");
    let mut voter = role_report(RecoveryRole::Voter, 20);
    voter.build_sha.clone_from(&build_sha);
    let mut learner = role_report(RecoveryRole::Learner, 20);
    learner.build_sha.clone_from(&build_sha);

    let mut wrong_execution = voter.clone();
    wrong_execution.execution_id = "other-execution".to_owned();
    let mut wrong_sha = voter.clone();
    wrong_sha.build_sha = "e".repeat(40);
    let mut wrong_role = role_report(RecoveryRole::Learner, 20);
    wrong_role.build_sha.clone_from(&build_sha);
    for (case, candidate) in [
        ("wrong execution", wrong_execution),
        ("wrong source", wrong_sha),
        ("wrong role", wrong_role),
    ] {
        std::fs::write(
            &voter_path,
            serde_json::to_vec(&candidate).expect("encode invalid pair member"),
        )
        .expect("write invalid pair member");
        std::fs::write(
            &learner_path,
            serde_json::to_vec(&learner).expect("encode learner"),
        )
        .expect("write learner");
        std::fs::write(&output, b"stale success").expect("seed stale output");
        let error = assemble_transport_recovery_reports(RecoveryAssemblePlan {
            execution_id: "test-execution-1".to_owned(),
            voter: voter_path.clone(),
            learner: learner_path.clone(),
            output: output.clone(),
        })
        .expect_err("mismatched role reports must not assemble");
        assert!(!output.exists(), "{case} retained stale output: {error:#}");
    }
}

#[test]
fn independent_role_errors_remain_attributed() {
    let voter = role_report(RecoveryRole::Voter, 3);
    let learner = role_report(RecoveryRole::Learner, 3);
    for (results, expected) in [
        (
            (Err(anyhow::anyhow!("voter cause")), Ok(learner.clone())),
            "voter transport-recovery role failed",
        ),
        (
            (Ok(voter.clone()), Err(anyhow::anyhow!("learner cause"))),
            "learner transport-recovery role failed",
        ),
    ] {
        let error = require_independent_roles(results).expect_err("one role failed");
        assert!(format!("{error:#}").contains(expected));
    }
}

#[tokio::test]
async fn voter_failure_does_not_skip_the_independent_learner_attempt() {
    let root = tempfile::tempdir().expect("sequence root");
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = calls.clone();
    let results = run_role_sequence(
        role_plan(RecoveryRole::Voter, root.path()),
        role_plan(RecoveryRole::Learner, root.path()),
        None,
        move |plan| {
            let observed = observed.clone();
            async move {
                observed.lock().expect("record role call").push(plan.role);
                bail!("{} injected failure", plan.role.label())
            }
        },
    )
    .await;
    assert_eq!(
        *calls.lock().expect("read role calls"),
        vec![RecoveryRole::Voter, RecoveryRole::Learner]
    );
    let error = require_independent_roles(results).expect_err("both roles failed");
    let message = format!("{error:#}");
    assert!(message.contains("voter") && message.contains("learner"));
}

#[tokio::test]
async fn external_cancellation_prevents_the_learner_attempt() {
    let root = tempfile::tempdir().expect("sequence root");
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = calls.clone();
    let results = run_role_sequence(
        role_plan(RecoveryRole::Voter, root.path()),
        role_plan(RecoveryRole::Learner, root.path()),
        None,
        move |plan| {
            let observed = observed.clone();
            async move {
                observed.lock().expect("record role call").push(plan.role);
                Err(anyhow::Error::new(RecoveryCancelled {
                    signal: "injected SIGTERM".to_owned(),
                }))
            }
        },
    )
    .await;
    assert_eq!(
        *calls.lock().expect("read role calls"),
        vec![RecoveryRole::Voter]
    );
    assert!(results
        .0
        .expect_err("voter cancelled")
        .is::<RecoveryCancelled>());
    assert!(results
        .1
        .expect_err("learner suppressed")
        .is::<RecoveryCancelled>());
}

#[tokio::test]
async fn latched_cancellation_between_roles_prevents_the_learner_attempt() {
    let root = tempfile::tempdir().expect("sequence root");
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = calls.clone();
    let (cancellation, sender) = RecoveryCancellationMonitor::test();
    let results = run_role_sequence(
        role_plan(RecoveryRole::Voter, root.path()),
        role_plan(RecoveryRole::Learner, root.path()),
        Some(&cancellation),
        move |plan| {
            let observed = observed.clone();
            let sender = sender.clone();
            async move {
                observed.lock().expect("record role call").push(plan.role);
                if plan.role == RecoveryRole::Voter {
                    sender
                        .send(Some("injected SIGTERM".to_owned()))
                        .expect("latch cancellation");
                }
                Ok(role_report(plan.role, 3))
            }
        },
    )
    .await;
    assert_eq!(
        *calls.lock().expect("read role calls"),
        vec![RecoveryRole::Voter]
    );
    assert!(results.0.is_ok());
    assert!(results
        .1
        .expect_err("learner suppressed")
        .is::<RecoveryCancelled>());
}

#[test]
fn recovery_failure_after_checkpoint_retains_completed_cycle() {
    use crate::transport_recovery_diagnostics::{
        read_complete_jsonl, CleanupSummary, RecoveryCycleCheckpoint, RecoveryDiagnosticSummary,
    };

    let root = tempfile::tempdir().expect("diagnostic root");
    let plan = role_plan(RecoveryRole::Voter, root.path());
    std::fs::create_dir(&plan.diagnostics_dir).expect("create diagnostics directory");
    let diagnostics = RecoveryDiagnostics::create(
        &plan,
        &"d".repeat(40),
        &build_profile_descriptor(),
        diagnostic_contracts(RecoveryRole::Voter),
        1,
    )
    .expect("create diagnostics");
    let evidence = role_report(RecoveryRole::Voter, 3)
        .campaign
        .cycles
        .remove(0);
    diagnostics
        .checkpoint(1, &evidence)
        .expect("durable checkpoint");
    let token = diagnostics
        .phase_start("target_install_observed", 2, None)
        .expect("recovery phase start");
    diagnostics
        .phase_end(token, &Err(anyhow::anyhow!("injected recovery failure")))
        .expect("recovery phase failure");
    diagnostics
        .record_terminal_observation(
            DiagnosticOutcome::Failed,
            Some(FailureClass::Recovery),
            Some("injected recovery failure"),
        )
        .expect("terminal observation");
    diagnostics
        .write_summary(
            DiagnosticOutcome::Failed,
            false,
            Some(FailureClass::Recovery),
            Some("injected recovery failure"),
            CleanupSummary {
                attempted: false,
                budget_millis: duration_millis(CLEANUP_BUDGET),
                registered_processes: 0,
                reaped_processes: 0,
                surviving_processes: Vec::new(),
            },
        )
        .expect("failed summary");
    let checkpoints =
        read_complete_jsonl::<RecoveryCycleCheckpoint>(&plan.diagnostics_dir.join("cycles.jsonl"))
            .expect("read checkpoints");
    assert_eq!(checkpoints.records.len(), 1);
    assert_eq!(checkpoints.records[0].cycle, 1);
    let summary: RecoveryDiagnosticSummary = serde_json::from_slice(
        &std::fs::read(plan.diagnostics_dir.join("summary.json")).expect("read diagnostic summary"),
    )
    .expect("decode diagnostic summary");
    assert_eq!(summary.completed_cycles, 1);
    assert_eq!(summary.failing_cycle, Some(2));
    assert!(!plan.output.exists());
}

#[test]
fn passing_role_report_is_not_published_when_its_summary_cannot_publish() {
    let root = tempfile::tempdir().expect("evidence root");
    let plan = role_plan(RecoveryRole::Voter, root.path());
    let diagnostics = create_test_diagnostics(&plan);
    std::fs::create_dir(plan.diagnostics_dir.join("summary.json"))
        .expect("block summary publication with a directory");
    let (cancellation, _sender) = RecoveryCancellationMonitor::test();

    let error = publish_passing_role_evidence_with(
        &role_report(RecoveryRole::Voter, 3),
        &diagnostics,
        &plan.output,
        empty_cleanup_summary(),
        &cancellation,
        publish_artifact_atomically,
    )
    .expect_err("summary failure must prevent role publication");
    assert!(format!("{error:#}").contains("diagnostic summary"));
    assert!(!plan.output.exists());
}

#[test]
fn failed_role_publication_removes_success_and_replaces_passing_summary() {
    use crate::transport_recovery_diagnostics::RecoveryDiagnosticSummary;

    let root = tempfile::tempdir().expect("evidence root");
    let plan = role_plan(RecoveryRole::Learner, root.path());
    let diagnostics = create_test_diagnostics(&plan);
    let (cancellation, _sender) = RecoveryCancellationMonitor::test();
    let error = publish_passing_role_evidence_with(
        &role_report(RecoveryRole::Learner, 3),
        &diagnostics,
        &plan.output,
        empty_cleanup_summary(),
        &cancellation,
        |output, bytes| {
            publish_artifact_atomically(output, bytes)?;
            bail!("injected directory sync failure after publication")
        },
    )
    .expect_err("role publication failure must fail the evidence pair");
    assert!(format!("{error:#}").contains("injected directory sync failure"));
    assert!(!plan.output.exists());

    let summary: RecoveryDiagnosticSummary = serde_json::from_slice(
        &std::fs::read(plan.diagnostics_dir.join("summary.json"))
            .expect("read replacement summary"),
    )
    .expect("decode replacement summary");
    assert_eq!(summary.status, DiagnosticOutcome::Failed);
    assert_eq!(summary.failure_class, Some(FailureClass::EvidenceIo));
    assert!(!summary.resource_envelope_asserted);
}

#[test]
fn cancellation_during_publication_revokes_role_and_passing_summary() {
    use crate::transport_recovery_diagnostics::RecoveryDiagnosticSummary;

    let root = tempfile::tempdir().expect("evidence root");
    let plan = role_plan(RecoveryRole::Voter, root.path());
    let diagnostics = create_test_diagnostics(&plan);
    let (cancellation, sender) = RecoveryCancellationMonitor::test();
    let error = publish_passing_role_evidence_with(
        &role_report(RecoveryRole::Voter, 3),
        &diagnostics,
        &plan.output,
        empty_cleanup_summary(),
        &cancellation,
        |output, bytes| {
            publish_artifact_atomically(output, bytes)?;
            sender
                .send(Some("injected SIGTERM".to_owned()))
                .context("latch injected cancellation")
        },
    )
    .expect_err("cancellation must revoke passing evidence");
    assert!(error.is::<RecoveryCancelled>());
    assert!(!plan.output.exists());

    let summary: RecoveryDiagnosticSummary = serde_json::from_slice(
        &std::fs::read(plan.diagnostics_dir.join("summary.json"))
            .expect("read cancellation summary"),
    )
    .expect("decode cancellation summary");
    assert_eq!(summary.status, DiagnosticOutcome::Cancelled);
    assert!(!summary.resource_envelope_asserted);
}

#[test]
fn smoke_aliases_are_bounded_and_cannot_claim_full_qualification() {
    let default = smoke_cycles(&[], RecoveryRole::Voter).expect("default voter smoke");
    assert_eq!(default, TRANSPORT_RECOVERY_DEFAULT_VOTER_SMOKE_CYCLES);
    assert_ne!(
        default, TRANSPORT_RECOVERY_CYCLES_PER_ROLE,
        "smoke success must remain distinct from full qualification"
    );
    assert_eq!(
        smoke_cycles(&[], RecoveryRole::Learner).expect("default learner smoke"),
        TRANSPORT_RECOVERY_DEFAULT_LEARNER_SMOKE_CYCLES
    );

    let requested =
        smoke_cycles(&["3".to_owned()], RecoveryRole::Voter).expect("requested smoke count");
    assert_eq!(requested, 3);
    for arguments in [
        vec!["0".to_owned()],
        vec![(TRANSPORT_RECOVERY_CYCLES_PER_ROLE + 1).to_string()],
        vec!["not-a-count".to_owned()],
        vec!["3".to_owned(), "extra".to_owned()],
    ] {
        assert!(smoke_cycles(&arguments, RecoveryRole::Voter).is_err());
    }
}

#[test]
fn recovery_writer_sql_executes_against_the_production_settings_shape() {
    let connection = rusqlite::Connection::open_in_memory().expect("open settings database");
    connection
        .execute_batch(
            "CREATE TABLE settings (\
                    key TEXT PRIMARY KEY,\
                    value TEXT NOT NULL,\
                    updated_at INTEGER NOT NULL\
                ) STRICT;",
        )
        .expect("create production settings shape");
    connection
        .execute(
            RECOVERY_WRITE_SQL,
            rusqlite::params!["recovery.key", "recovery.value", 1_i64],
        )
        .expect("execute recovery writer statement");
    let stored: (String, i64) = connection
        .query_row(
            "SELECT value, updated_at FROM settings WHERE key = 'recovery.key'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read recovery writer row");
    assert_eq!(stored, ("recovery.value".to_owned(), 1));
}

#[test]
fn retained_bytes_receive_the_same_closed_schema_and_semantic_validation() {
    let bytes = serde_json::to_vec(&artifact()).expect("serialize complete evidence");
    validate_transport_recovery_bytes(&bytes).expect("validate complete retained bytes");

    let partial = serde_json::to_vec(&serde_json::json!({
        "build_sha": "d".repeat(40),
        "cycles": []
    }))
    .expect("serialize partial evidence");
    assert!(validate_transport_recovery_bytes(&partial).is_err());
}

#[test]
fn worst_transfer_duration_covers_the_complete_source_series() {
    let mut campaign = role_campaign(RecoveryRole::Learner, TRANSPORT_RECOVERY_SMALL_IMAGE_BYTES);
    campaign.cycles[0].source_outbound.attempts = 3;
    campaign.cycles[0].source_outbound.retry_count = 1;
    campaign.cycles[0]
        .source_outbound
        .attempt_started_at_unix_ms = 2_001;
    campaign.cycles[0].source_outbound.elapsed_millis = 3_999;
    campaign.worst_durations = worst_durations(&campaign.cycles);

    assert_eq!(campaign.worst_durations.transfer_millis, 3_999);
    assert!(
        campaign.worst_durations.transfer_millis
            > campaign.cycles[0].target_inbound.transfer_millis
    );
}

#[test]
fn artifact_publication_replaces_stale_bytes_without_exposing_a_partial_result() {
    let root = tempfile::tempdir().expect("artifact publication root");
    let output = root.path().join("nested/recovery.json");
    std::fs::create_dir_all(output.parent().expect("artifact parent"))
        .expect("create stale artifact parent");
    std::fs::write(&output, b"stale evidence").expect("write stale artifact");

    prepare_artifact_output(&output).expect("clear stale artifact");
    assert!(!output.exists());
    publish_artifact_atomically(&output, b"{\"complete\":true}\n")
        .expect("publish complete artifact");

    assert_eq!(
        std::fs::read(&output).expect("read published artifact"),
        b"{\"complete\":true}\n"
    );
    assert_eq!(
        std::fs::read_dir(output.parent().expect("artifact parent"))
            .expect("list artifact directory")
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn snapshot_publication_can_arrive_after_the_bounded_trigger_load() {
    let mut samples =
        std::collections::VecDeque::from([(None, "absent"), (Some(20), "old"), (Some(21), "new")]);
    let deadline = (tokio::time::Instant::now() + Duration::from_secs(1)).into_std();
    let result = await_snapshot_publication(Some(20), deadline, async || {
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(samples
            .pop_front()
            .expect("only the three planned observations"))
    })
    .await
    .expect("publication may finish after the finite trigger load");
    assert_eq!(result, "new");
    assert!(
        samples.is_empty(),
        "absence and the old index cannot settle publication"
    );
}

#[tokio::test(start_paused = true)]
async fn expired_snapshot_publication_deadline_does_not_start_an_observation() {
    let mut called = false;
    let deadline = tokio::time::Instant::now().into_std();
    let result = await_snapshot_publication(Some(20), deadline, async || {
        called = true;
        Ok((Some(21), ()))
    })
    .await;
    assert!(result
        .expect_err("an expired budget cannot accept a snapshot")
        .to_string()
        .contains("deadline"));
    assert!(!called, "the old budget must not start another request");
}

#[tokio::test(start_paused = true)]
async fn snapshot_publication_observation_is_bounded_by_the_remaining_budget() {
    let start = tokio::time::Instant::now();
    let deadline = start + Duration::from_millis(50);
    let mut completed = false;
    let result = await_snapshot_publication(Some(20), deadline.into_std(), async || {
        tokio::time::sleep(Duration::from_millis(100)).await;
        completed = true;
        Ok((Some(21), ()))
    })
    .await;
    assert!(result
        .expect_err("a slow request cannot extend the absolute budget")
        .to_string()
        .contains("deadline"));
    assert!(
        !completed,
        "the owned request future must be dropped at its deadline"
    );
    assert_eq!(tokio::time::Instant::now(), deadline);
}

#[tokio::test(start_paused = true)]
async fn snapshot_publication_completion_after_the_absolute_deadline_is_refused() {
    let start = tokio::time::Instant::now();
    let deadline = start + Duration::from_millis(50);
    let result = await_snapshot_publication(Some(20), deadline.into_std(), async || {
        tokio::time::advance(Duration::from_millis(100)).await;
        Ok((Some(21), "late snapshot"))
    })
    .await;
    assert!(result
        .expect_err("a late ready value cannot settle publication")
        .to_string()
        .contains("deadline"));
    assert!(tokio::time::Instant::now() >= deadline);
}

#[tokio::test]
async fn installed_snapshot_evidence_survives_rotation_after_threshold_writes() {
    let root = tempfile::tempdir().expect("snapshot rotation fixture root");
    let source_snapshots =
        super::super::node_hiqlite_dir(root.path(), 1).join("state_machine/snapshots");
    let target_snapshots =
        super::super::node_hiqlite_dir(root.path(), TARGET_NODE).join("state_machine/snapshots");
    std::fs::create_dir_all(&source_snapshots).expect("create source snapshot fixture");
    std::fs::create_dir_all(&target_snapshots).expect("create target snapshot fixture");
    let installed_id = "00000000-0000-7000-8000-000000000001";
    let rotated_id = "00000000-0000-7000-8000-000000000002";
    std::fs::write(
        source_snapshots.join(installed_id),
        b"installed recovery snapshot",
    )
    .expect("write installed snapshot");
    std::fs::write(source_snapshots.join("current"), installed_id)
        .expect("point at installed snapshot");

    let source_snapshot = snapshot_file(root.path(), 1).expect("source evidence");
    let installed_snapshot = wait_for_installed_snapshot_file(
        root.path(),
        TARGET_NODE,
        &source_snapshot,
        Instant::now() + Duration::from_secs(2),
    );
    let rotate_snapshot = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        std::fs::write(target_snapshots.join(installed_id), b"partial snapshot")
            .expect("write partial target snapshot");
        std::fs::write(target_snapshots.join("current"), installed_id)
            .expect("publish partial target snapshot");
        tokio::time::sleep(Duration::from_millis(30)).await;
        std::fs::write(
            target_snapshots.join(installed_id),
            b"installed recovery snapshot",
        )
        .expect("install target snapshot");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let continued_writes = root.path().join("continued-writes");
        std::fs::create_dir(&continued_writes).expect("create continued-write fixture");
        for ordinal in 1..=TRANSPORT_RECOVERY_SNAPSHOT_LOGS_SINCE_LAST + 1 {
            std::fs::write(
                continued_writes.join(format!("{ordinal:04}")),
                ordinal.to_string(),
            )
            .expect("record continued write");
        }
        std::fs::write(
            target_snapshots.join(rotated_id),
            b"later snapshot after continued writes",
        )
        .expect("write rotated snapshot");
        std::fs::write(target_snapshots.join("current"), rotated_id)
            .expect("rotate current snapshot pointer");
        std::fs::remove_file(target_snapshots.join(installed_id))
            .expect("clean up superseded installed snapshot");
    };
    let (installed_snapshot, ()) = tokio::join!(installed_snapshot, rotate_snapshot);
    let installed_snapshot =
        installed_snapshot.expect("capture installed snapshot at installation");

    let current_snapshot = snapshot_file(root.path(), TARGET_NODE).expect("current evidence");
    assert_ne!(current_snapshot, source_snapshot);
    assert_eq!(installed_snapshot, source_snapshot);
    assert!(!target_snapshots.join(installed_id).exists());
}

#[test]
fn missing_cycle_is_rejected() {
    let mut value = artifact();
    value.learner.cycles.pop();
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

#[test]
fn write_not_acknowledged_during_recovery_is_rejected() {
    let mut value = artifact();
    for write in &mut value.learner.cycles[0].acknowledged_writes {
        write.acknowledged_at_unix_ms = 1_500;
    }
    value.learner.cycles[0].acknowledged_write_digest =
        acknowledged_write_digest(&value.learner.cycles[0].acknowledged_writes);
    value.learner.cycles[0].target_write_digest =
        value.learner.cycles[0].acknowledged_write_digest.clone();
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

#[test]
fn readiness_write_without_continuing_recovery_load_is_rejected() {
    let mut value = artifact();
    value.learner.cycles[0].acknowledged_writes.truncate(1);
    value.learner.cycles[0].acknowledged_write_digest =
        acknowledged_write_digest(&value.learner.cycles[0].acknowledged_writes);
    value.learner.cycles[0].target_write_digest =
        value.learner.cycles[0].acknowledged_write_digest.clone();
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

#[test]
fn mismatched_installed_snapshot_is_rejected() {
    let mut value = artifact();
    value.voter.cycles[0].installed_snapshot.sha256 = "e".repeat(64);
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

/// The fixture's baseline is 12 threads, 8 sockets, 10 owned tasks on
/// every node; the series below are written against it.
#[test]
fn a_socket_leaked_every_cycle_is_rejected() {
    let mut value = artifact();
    with_node_series(&mut value.voter, 0, |cycle| {
        counts(12, 8 + u64::from(cycle), 10)
    });
    let error = validate_transport_recovery_artifact(&value).expect_err("a socket leak");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("node 1 sockets floor rose 9 -> 19"),
        "{rendered}"
    );
    assert!(
        rendered.contains("node 1 sockets ceiling rose 18 -> 25"),
        "{rendered}"
    );
}

#[test]
fn a_thread_leaked_every_cycle_is_rejected() {
    let mut value = artifact();
    with_node_series(&mut value.learner, 3, |cycle| {
        counts(12 + u64::from(cycle), 8, 10)
    });
    let error = validate_transport_recovery_artifact(&value).expect_err("a thread leak");
    assert!(
        format!("{error:#}").contains("node 4 threads floor rose 13 -> 23"),
        "{error:#}"
    );
}

/// A leak that starts after the opening window never lifts the closing
/// floor — the closing window's first cycles are still at the old floor
/// — so it is the ceiling that has to catch it: most of the closing
/// window sits above anything the opening window showed. The onset here
/// is cycle 13, past the edge, which puts eight samples above the
/// opening ceiling and makes the fourth-highest 13; a single leaked
/// socket at cycle 15 that is never released is the smallest possible
/// late leak and puts six.
#[test]
fn a_leak_that_begins_late_in_the_campaign_is_rejected() {
    let mut value = artifact();
    with_node_series(&mut value.voter, 1, |cycle| {
        counts(12, 8 + u64::from(cycle.saturating_sub(12)), 10)
    });
    let error = validate_transport_recovery_artifact(&value).expect_err("a late leak");
    let rendered = format!("{error:#}");
    assert!(!rendered.contains("floor rose"), "{rendered}");
    assert!(
        rendered.contains("node 2 sockets ceiling rose 8 -> 13"),
        "{rendered}"
    );

    let mut value = artifact();
    with_node_series(&mut value.voter, 1, |cycle| {
        counts(12, if cycle >= 15 { 9 } else { 8 }, 10)
    });
    assert!(validate_transport_recovery_artifact(&value).is_err());

    // On the signal the lane actually samples — a follower alternating
    // between 8 and 9 sockets one recovery apart — only the high-side
    // samples after the leak sit above the opening ceiling, so the
    // boundary is earlier: one connection leaked by cycle 12 is caught
    // and one leaked at cycle 15 is the documented miss; a per-cycle
    // leak from cycle 15 is caught and one from cycle 18 is not.
    let drain = |cycle: u32| if cycle % 2 == 1 { 9 } else { 8 };
    let mut value = artifact();
    with_node_series(&mut value.voter, 1, |cycle| {
        counts(12, drain(cycle) + u64::from(cycle >= 12), 10)
    });
    assert!(validate_transport_recovery_artifact(&value).is_err());
    let mut value = artifact();
    with_node_series(&mut value.voter, 1, |cycle| {
        counts(12, drain(cycle) + u64::from(cycle >= 15), 10)
    });
    validate_transport_recovery_artifact(&value).expect("the documented miss");
    let mut value = artifact();
    with_node_series(&mut value.voter, 1, |cycle| {
        counts(12, drain(cycle) + u64::from(cycle.saturating_sub(14)), 10)
    });
    assert!(validate_transport_recovery_artifact(&value).is_err());
    let mut value = artifact();
    with_node_series(&mut value.voter, 1, |cycle| {
        counts(12, drain(cycle) + u64::from(cycle.saturating_sub(17)), 10)
    });
    validate_transport_recovery_artifact(&value).expect("the documented miss");
}

/// Exactly the measured shape: one connection and one owned task per
/// peer, alternating one recovery apart, on a source with three peers.
/// Nothing accumulates, so nothing is a leak — and the same series with
/// the warmup on the high side, or landing high only every third cycle,
/// is the same answer.
#[test]
fn the_per_peer_transport_drain_is_not_growth() {
    let mut value = artifact();
    with_node_series(&mut value.voter, 0, |cycle| {
        if cycle % 2 == 1 {
            counts(12, 8 + 3, 10 + 3)
        } else {
            counts(12, 8, 10)
        }
    });
    validate_transport_recovery_artifact(&value).expect("a drain is not a leak");
    let node = &value.voter.resource_envelopes.nodes[0];
    assert_eq!(node.opening.floor, counts(12, 8, 10));
    assert_eq!(node.opening.ceiling, counts(12, 11, 13));
    assert_eq!(node.closing.floor, counts(12, 8, 10));
    assert_eq!(node.closing.ceiling, counts(12, 11, 13));

    let mut value = artifact();
    value.voter.cycles.iter_mut().for_each(|cycle| {
        cycle.node_resources[0].baseline = counts(12, 11, 13);
    });
    with_node_series(&mut value.voter, 0, |cycle| {
        if cycle % 3 == 0 {
            counts(12, 11, 13)
        } else {
            counts(12, 8, 10)
        }
    });
    validate_transport_recovery_artifact(&value).expect("a high warmup is not a leak");
}

/// Node 1 of the voter campaign on run 1034, sample for sample: a cold
/// baseline of fourteen threads, sixteen once recoveries start, seventeen
/// from cycle ten onward, and then eleven consecutive cycles that do not
/// move. Sockets and owned tasks drain a peer's worth every third cycle
/// or so throughout.
///
/// A pool that grows twice during warmup and then stops is not a leak: a
/// leak has no last cycle. Admitting the baseline to the opening floor
/// made this campaign fail with `threads floor rose 14 -> 17`, comparing
/// steady state against a cold start rather than the campaign against
/// itself.
#[test]
fn a_warmup_step_that_plateaus_is_not_growth() {
    const THREADS: [u64; 20] = [
        16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17,
    ];
    let high = [1_u64, 12, 13, 16, 20];
    let mut value = artifact();
    value.voter.cycles.iter_mut().for_each(|cycle| {
        cycle.node_resources[0].baseline = counts(14, 16, 31);
    });
    with_node_series(&mut value.voter, 0, |cycle| {
        let threads = THREADS[cycle as usize - 1];
        if high.contains(&u64::from(cycle)) {
            counts(threads, 19, 34)
        } else {
            counts(threads, 16, 31)
        }
    });
    validate_transport_recovery_artifact(&value)
        .expect("a warmup step that plateaus is not a leak");

    let node = &value.voter.resource_envelopes.nodes[0];
    assert_eq!(
        node.opening.floor.threads, 16,
        "the cold baseline is not the floor"
    );
    assert_eq!(node.closing.floor.threads, 17);

    // The same series with the plateau replaced by continued growth is a
    // leak, and is still caught.
    let mut value = artifact();
    value.voter.cycles.iter_mut().for_each(|cycle| {
        cycle.node_resources[0].baseline = counts(14, 16, 31);
    });
    with_node_series(&mut value.voter, 0, |cycle| {
        counts(16 + u64::from(cycle) / 3, 16, 31)
    });
    let error = validate_transport_recovery_artifact(&value)
        .expect_err("growth that never stops is a leak");
    assert!(
        format!("{error:#}").contains("node 1 threads floor rose"),
        "{error:#}"
    );
}

/// The other measured shape: thread counts wander a couple either side
/// of their floor with no recovery in flight, and the floor is not seen
/// on every cycle.
#[test]
fn thread_jitter_inside_the_allowance_is_not_growth() {
    let mut value = artifact();
    with_node_series(&mut value.learner, 3, |cycle| {
        counts(12 + [1_u64, 0, 2, 1, 1][cycle as usize % 5], 8, 10)
    });
    validate_transport_recovery_artifact(&value).expect("thread jitter is not a leak");

    // Even a closing window that never shows the floor again, and shows
    // a count the opening window never did, stays inside the allowance.
    let mut value = artifact();
    with_node_series(&mut value.learner, 3, |cycle| {
        if cycle > 10 {
            counts(12 + THREAD_ENVELOPE_ALLOWANCE, 8, 10)
        } else {
            counts(12, 8, 10)
        }
    });
    validate_transport_recovery_artifact(&value).expect("inside the thread allowance");
}

#[test]
fn a_thread_envelope_past_the_allowance_is_rejected() {
    let mut value = artifact();
    with_node_series(&mut value.learner, 3, |cycle| {
        if cycle > 10 {
            counts(12 + THREAD_ENVELOPE_ALLOWANCE + 1, 8, 10)
        } else {
            counts(12, 8, 10)
        }
    });
    assert!(validate_transport_recovery_artifact(&value).is_err());

    // One thread every third recovery lifts the closing floor by three
    // against an allowance of two; every fourth lifts it by two and sits
    // inside the allowance at both edges, and that is the fastest thread
    // leak the allowance hides.
    let mut value = artifact();
    with_node_series(&mut value.learner, 3, |cycle| {
        counts(12 + u64::from(cycle / 3), 8, 10)
    });
    let error = validate_transport_recovery_artifact(&value).expect_err("one per three");
    assert!(
        format!("{error:#}").contains("node 4 threads floor rose 12 -> 15"),
        "{error:#}"
    );
    let mut value = artifact();
    with_node_series(&mut value.learner, 3, |cycle| {
        counts(12 + u64::from(cycle / 4), 8, 10)
    });
    validate_transport_recovery_artifact(&value).expect("hidden inside the allowance");
}

/// One high sample, in either window, is recorded and is not a verdict,
/// whether or not the other window ever showed that height. This is the
/// sample the old per-cycle ceiling failed the whole lane on.
#[test]
fn a_single_high_sample_in_either_window_is_recorded_not_rejected() {
    let mut value = artifact();
    value.voter.cycles[0].node_resources[0].post_quiescence = counts(14, 11, 13);
    value.voter.cycles[15].node_resources[2].post_quiescence = counts(15, 12, 14);
    value.voter.resource_envelopes =
        resource_envelopes(&value.voter.cycles).expect("rewritten envelopes");
    validate_transport_recovery_artifact(&value).expect("a high sample is evidence");
}

/// The closing ceiling is the fourth-highest sample: three cycles that
/// reached a count the opening window never did are spikes — a
/// replacement connection, a slower drain, a blocking thread that had
/// not idled out — and four are a trend. The opening ceiling is the
/// highest of its cycles, so a spike there raises the bar for the
/// closing window rather than lowering it, and the warmup baseline,
/// which nothing recomputes, bounds the floor only.
#[test]
fn three_high_closing_samples_are_spikes_and_four_are_a_trend() {
    let mut value = artifact();
    with_node_series(&mut value.voter, 0, |cycle| {
        counts(12, if [12, 15, 19].contains(&cycle) { 9 } else { 8 }, 10)
    });
    validate_transport_recovery_artifact(&value).expect("three spikes");
    assert_eq!(
        value.voter.resource_envelopes.nodes[0]
            .closing
            .ceiling
            .sockets,
        8
    );

    let mut value = artifact();
    with_node_series(&mut value.voter, 0, |cycle| {
        counts(
            12,
            if [12, 15, 17, 19].contains(&cycle) {
                9
            } else {
                8
            },
            10,
        )
    });
    let error = validate_transport_recovery_artifact(&value).expect_err("four samples");
    assert!(
        format!("{error:#}").contains("node 1 sockets ceiling rose 8 -> 9"),
        "{error:#}"
    );

    let mut value = artifact();
    value.voter.cycles.iter_mut().for_each(|cycle| {
        cycle.node_resources[0].baseline = counts(12, 99, 10);
    });
    with_node_series(&mut value.voter, 0, |cycle| {
        counts(12, if cycle >= 13 { 8 + u64::from(cycle) } else { 8 }, 10)
    });
    let envelopes = &value.voter.resource_envelopes;
    assert_eq!(envelopes.nodes[0].opening.floor.sockets, 8);
    assert_eq!(envelopes.nodes[0].opening.ceiling.sockets, 8);
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

#[test]
fn recorded_resource_envelopes_must_match_the_cycles() {
    let mut value = artifact();
    value.learner.resource_envelopes.nodes[0]
        .closing
        .floor
        .sockets -= 1;
    assert!(validate_transport_recovery_artifact(&value).is_err());

    let mut value = artifact();
    value.learner.resource_envelopes.nodes[0]
        .opening
        .ceiling
        .sockets += 1;
    assert!(validate_transport_recovery_artifact(&value).is_err());

    let mut value = artifact();
    value.learner.resource_envelopes.opening_window_last_cycle = 15;
    assert!(validate_transport_recovery_artifact(&value).is_err());

    let mut value = artifact();
    value.learner.resource_envelopes.closing_window_samples = 11;
    assert!(validate_transport_recovery_artifact(&value).is_err());

    let mut value = artifact();
    value
        .learner
        .resource_envelopes
        .closing_ceiling_sustained_samples = 1;
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

/// The envelope is computed over every cycle or not at all: a campaign
/// whose records are reordered, mislabelled or renumbered is refused by
/// the envelope itself, before any per-cycle check looks at it.
#[test]
fn an_envelope_over_incomplete_or_reordered_records_is_refused() {
    let complete = role_campaign(RecoveryRole::Voter, TRANSPORT_RECOVERY_LARGE_IMAGE_BYTES);
    assert!(resource_envelopes(&[]).is_err());

    let mut reordered = complete.clone();
    reordered.cycles[5].node_resources.swap(0, 1);
    assert!(resource_envelopes(&reordered.cycles).is_err());

    let mut short = complete.clone();
    short.cycles[5].node_resources.pop();
    assert!(resource_envelopes(&short.cycles).is_err());

    let mut renumbered = complete.clone();
    renumbered.cycles[19].cycle = 1;
    assert!(resource_envelopes(&renumbered.cycles).is_err());

    let mut mislabelled = complete;
    mislabelled.cycles[5].node_resources[3].kind = RecoveryResourceNodeKind::PersistentSourceVoter;
    assert!(resource_envelopes(&mislabelled.cycles).is_err());
}

/// The opening window is the baseline plus the first half of the cycles,
/// rounded up, for any campaign length the smoke can ask for — and the
/// envelope is a verdict only once the closing window holds the ten
/// samples the twenty-cycle campaign gives it.
#[test]
fn the_opening_window_is_the_first_half_of_the_series_rounded_up() {
    assert_eq!(opening_window_last_cycle(21), 10);
    assert_eq!(opening_window_last_cycle(4), 1);
    assert_eq!(opening_window_last_cycle(2), 0);
    assert_eq!(opening_window_last_cycle(1), 0);

    let mut campaign = role_campaign(RecoveryRole::Voter, TRANSPORT_RECOVERY_LARGE_IMAGE_BYTES);
    assert_eq!(campaign.resource_envelopes.opening_window_last_cycle, 10);
    assert_eq!(campaign.resource_envelopes.closing_window_samples, 10);
    assert!(envelope_is_asserted(&campaign.resource_envelopes));

    campaign.cycles.truncate(3);
    with_node_series(&mut campaign, 0, |cycle| {
        counts(12, 8 + u64::from(cycle), 10)
    });
    let envelopes = &campaign.resource_envelopes;
    assert_eq!(envelopes.opening_window_last_cycle, 1);
    assert_eq!(envelopes.closing_window_samples, 2);
    assert!(!envelope_is_asserted(envelopes));
    // Cycle 1 is the whole opening window, and the baseline of eight
    // sockets is not a member of it: the band is that cycle alone.
    assert_eq!(envelopes.nodes[0].opening.floor, counts(12, 9, 10));
    assert_eq!(envelopes.nodes[0].opening.ceiling, counts(12, 9, 10));
    assert_eq!(envelopes.nodes[0].closing.floor, counts(12, 10, 10));
    // Two closing samples: the sustained ceiling clamps to the lower.
    assert_eq!(envelopes.nodes[0].closing.ceiling, counts(12, 10, 10));

    for cycles in [1_usize, 2, 18] {
        let mut campaign = role_campaign(RecoveryRole::Voter, TRANSPORT_RECOVERY_LARGE_IMAGE_BYTES);
        campaign.cycles.truncate(cycles);
        let envelopes = resource_envelopes(&campaign.cycles).expect("short campaign");
        assert!(!envelope_is_asserted(&envelopes), "{cycles} cycles");
    }
    let mut campaign = role_campaign(RecoveryRole::Voter, TRANSPORT_RECOVERY_LARGE_IMAGE_BYTES);
    campaign.cycles.truncate(19);
    let envelopes = resource_envelopes(&campaign.cycles).expect("19-cycle campaign");
    assert!(envelope_is_asserted(&envelopes));
}

#[test]
fn every_persistent_source_voter_requires_resource_evidence() {
    let mut value = artifact();
    value.learner.cycles[0].node_resources.remove(1);
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

#[test]
fn persistent_source_resource_baselines_cannot_be_rebased_between_cycles() {
    let mut value = artifact();
    value.learner.cycles[1].node_resources[0].baseline.threads += 1;
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

#[test]
fn outbound_attempt_metrics_must_come_from_a_timed_source_observation() {
    let mut value = artifact();
    value.voter.cycles[0]
        .source_outbound
        .attempt_started_at_unix_ms = 6_000;
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

#[test]
fn large_snapshot_receive_and_install_timings_cannot_be_zero() {
    let mut value = artifact();
    value.voter.cycles[0].target_inbound.install_millis = 0;
    value.voter.cycles[0]
        .target_inbound
        .install_started_at_unix_ms = 5_500;
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

#[test]
fn recovery_acknowledgements_must_include_a_cadence_separated_pair() {
    let mut value = artifact();
    value.learner.cycles[0].acknowledged_writes[1].acknowledged_at_unix_ms = 2_500;
    value.learner.cycles[0].acknowledged_writes[2].acknowledged_at_unix_ms = 2_999;
    value.learner.cycles[0].acknowledged_write_digest =
        acknowledged_write_digest(&value.learner.cycles[0].acknowledged_writes);
    value.learner.cycles[0].target_write_digest =
        value.learner.cycles[0].acknowledged_write_digest.clone();
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

#[test]
fn two_early_acknowledgements_cannot_hide_a_long_recovery_gap() {
    let mut value = artifact();
    let cycle = &mut value.learner.cycles[0];
    cycle.recovery_finished_at_unix_ms = 102_000;
    cycle.recovery_millis = 100_000;
    cycle.acknowledged_writes[1].acknowledged_at_unix_ms = 2_500;
    cycle.acknowledged_writes[2].acknowledged_at_unix_ms = 40_000;
    cycle.acknowledged_write_digest = acknowledged_write_digest(&cycle.acknowledged_writes);
    cycle.target_write_digest = cycle.acknowledged_write_digest.clone();
    value.finished_at_unix_ms = 103_000;
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

#[test]
fn writer_must_cover_the_end_of_recovery() {
    let mut value = artifact();
    let cycle = &mut value.learner.cycles[0];
    cycle.recovery_finished_at_unix_ms = 50_000;
    cycle.recovery_millis = 48_000;
    cycle.acknowledged_write_digest = acknowledged_write_digest(&cycle.acknowledged_writes);
    cycle.target_write_digest = cycle.acknowledged_write_digest.clone();
    value.finished_at_unix_ms = 51_000;
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

#[test]
fn whole_recovery_duration_cannot_exceed_the_absolute_deadline() {
    let mut value = artifact();
    value.learner.cycles[0].recovery_millis = duration_millis(RECOVERY_DEADLINE).saturating_add(1);
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

fn resource_counts(
    node: u64,
    threads: u64,
    sockets: u64,
    tasks: u64,
) -> (u64, ProcessResourceCount) {
    (
        node,
        ProcessResourceCount {
            threads,
            sockets,
            owned_async_tasks: tasks,
        },
    )
}

/// A recovery that never finished has to be named, because a longer
/// horizon is the wrong answer to it and the right answer to the case
/// below.
#[test]
fn an_expired_resource_wait_names_every_node_still_owning_work() {
    let current = vec![
        resource_counts(1, 10, 10, 10),
        resource_counts(3, 10, 10, 10),
    ];
    let report = describe_resource_wait(&[1, 3], &current, None, 0);
    assert!(
        report.contains("node(s) 1, 3 still owning transport work"),
        "{report}"
    );
    assert!(!report.contains("every node idle"), "{report}");

    let error = annotate_resource_wait(
        remaining_before(Instant::now(), "stable idle resource sampling")
            .expect_err("an elapsed deadline"),
        Some(&report),
    );
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("node(s) 1, 3 still owning transport work"),
        "{rendered}"
    );
}

/// Whether a longer horizon would help is decided by what moved, so the
/// report has to carry the deltas and has to say when there were none.
#[test]
fn an_unstable_resource_wait_names_what_moved_between_the_two_samples() {
    let previous = vec![
        resource_counts(1, 10, 9, 10),
        resource_counts(2, 10, 10, 10),
    ];
    let current = vec![
        resource_counts(1, 10, 10, 10),
        resource_counts(2, 10, 10, 8),
    ];
    let report = describe_resource_wait(&[], &current, Some(&previous), 1);
    assert!(report.contains("node 1 sockets 9 -> 10"), "{report}");
    assert!(
        report.contains("node 2 owned async tasks 10 -> 8"),
        "{report}"
    );
    assert!(!report.contains("node 1 threads"), "{report}");

    let settled = describe_resource_wait(&[], &current, Some(&current), 1);
    assert!(
        settled.contains("the last two samples were identical"),
        "{settled}"
    );
}

/// The case a longer horizon does fix: nothing is wrong, the machine is
/// just too loaded for two samples in a row to match.
#[test]
fn an_expired_resource_wait_that_is_only_unstable_says_exactly_that() {
    let current = vec![resource_counts(1, 10, 10, 10)];
    let report = describe_resource_wait(&[], &current, None, 1);
    assert!(report.contains("every node idle"), "{report}");
    assert!(
        report.contains(&format!(
            "1 of {RESOURCE_STABLE_SAMPLES} consecutive repeated samples"
        )),
        "{report}"
    );

    let error = annotate_resource_wait(
        remaining_before(Instant::now(), "stable idle resource sampling")
            .expect_err("an elapsed deadline"),
        None,
    );
    assert!(
        format!("{error:#}").contains("no resource sample completed inside the horizon"),
        "{error:#}"
    );
}

/// The sampler is a sample, not a verdict: a count above the warmup is
/// recorded as it stands, and the campaign asks the leak question once,
/// over the floors. An owned task that is never released moves the
/// closing floor by one for every cycle it survives, and the allowance
/// for that resource is zero, so one per-peer transport that stops being
/// released is exactly the size of leak this catches.
#[test]
fn owned_async_task_growth_has_no_resource_slack() {
    assert_eq!(RESOURCE_BASELINE_WARMUP_CYCLES, 1);
    assert_eq!(OWNED_ASYNC_TASK_ENVELOPE_ALLOWANCE, 0);
    assert_eq!(SOCKET_ENVELOPE_ALLOWANCE, 0);

    let baseline = vec![resource_counts(1, 10, 10, 10)];
    let mut high = baseline.clone();
    high[0].1.owned_async_tasks += 1;
    let recorded = collect_node_resource_evidence(&baseline, &high)
        .expect("a high sample is recorded, not refused");
    assert_eq!(recorded[0].post_quiescence.owned_async_tasks, 11);
    assert_eq!(recorded[0].baseline.owned_async_tasks, 10);

    let mut value = artifact();
    with_node_series(&mut value.voter, 0, |cycle| {
        counts(12, 8, 10 + u64::from(cycle))
    });
    let error = validate_transport_recovery_artifact(&value).expect_err("an owned task leak");
    assert!(
        format!("{error:#}").contains("node 1 owned async tasks floor rose 11 -> 21"),
        "{error:#}"
    );

    // The smallest possible rise — the closing window never again shows
    // the opening floor — is still a leak at zero allowance.
    let mut value = artifact();
    with_node_series(&mut value.voter, 0, |cycle| {
        counts(12, 8, if cycle > 10 { 11 } else { 10 })
    });
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

#[tokio::test]
async fn writer_exit_before_readiness_fails_promptly() {
    let root = tempfile::tempdir().expect("writer readiness root");
    let child = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", "transport_recovery_no_such_test"])
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn short-lived writer stand-in");
    let mut writer = RecoveryWriter {
        child,
        stdout: None,
        identity: None,
    };

    let error = wait_for_writer_ready(
        &mut writer,
        &root.path().join("never-ready"),
        Duration::from_secs(2),
    )
    .await
    .expect_err("writer exit must fail readiness");
    assert!(error
        .to_string()
        .contains("exited before publishing readiness"));
}

#[tokio::test]
async fn writer_exit_after_readiness_fails_the_recovery_promptly() {
    use tokio::io::AsyncReadExt;

    let root = tempfile::tempdir().expect("writer supervisor root");
    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", "transport_recovery_no_such_test"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn short-lived writer stand-in");
    let mut stdout = child.stdout.take().expect("stand-in stdout");
    let stdout = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await?;
        Ok(bytes)
    });
    let mut writer = RecoveryWriter {
        child,
        stdout: Some(stdout),
        identity: None,
    };
    let config = RecoveryWriterConfig {
        addresses: vec!["one".into(), "two".into(), "three".into()],
        prefix: "writer-supervisor.".into(),
        ready_path: root.path().join("ready"),
        progress_path: root.path().join("progress"),
        start_path: root.path().join("start"),
        stop_path: root.path().join("stop"),
        interval_millis: WRITER_INTERVAL_MILLIS,
        max_operations: WRITER_MAX_OPERATIONS,
    };
    let error = supervise_recovery_writer(
        &mut writer,
        &config,
        1,
        Instant::now() + Duration::from_secs(2),
        std::future::pending::<Result<()>>(),
    )
    .await
    .expect_err("writer exit must fail recovery");
    assert!(error
        .to_string()
        .contains("exited before recovery completed"));
}

#[test]
fn recovery_acknowledgement_identifiers_must_be_unique() {
    let mut value = artifact();
    value.learner.cycles[0].acknowledged_writes[2].key =
        value.learner.cycles[0].acknowledged_writes[1].key.clone();
    value.learner.cycles[0].acknowledged_write_digest =
        acknowledged_write_digest(&value.learner.cycles[0].acknowledged_writes);
    value.learner.cycles[0].target_write_digest =
        value.learner.cycles[0].acknowledged_write_digest.clone();
    assert!(validate_transport_recovery_artifact(&value).is_err());
}

#[test]
fn ambiguous_write_resolution_accepts_only_the_exact_committed_value() {
    let exact = RecoveryWriteRow {
        key: "recovery.unique.000001".to_owned(),
        value: "expected".to_owned(),
    };
    assert_eq!(
        classify_recovery_write("recovery.unique.000001", "expected", &[exact])
            .expect("classify exact committed value"),
        RecoveryWriteResolution::Exact
    );
    assert_eq!(
        classify_recovery_write("recovery.unique.000001", "expected", &[])
            .expect("classify unresolved value"),
        RecoveryWriteResolution::Missing
    );
    let mismatch = RecoveryWriteRow {
        key: "recovery.unique.000001".to_owned(),
        value: "different".to_owned(),
    };
    assert!(classify_recovery_write("recovery.unique.000001", "expected", &[mismatch]).is_err());
}

#[test]
fn schema_is_closed_and_valid_draft_2020_12() {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../benchmarks/cluster-transport-recovery.schema.json"
    ))
    .expect("parse recovery schema");
    jsonschema::draft202012::meta::validate(&schema).expect("valid recovery meta-schema");
    let validator = jsonschema::draft202012::new(&schema).expect("compile recovery schema");
    let mut value = serde_json::to_value(artifact()).expect("serialize recovery fixture");
    value
        .as_object_mut()
        .expect("artifact object")
        .insert("invented".to_owned(), serde_json::json!(true));
    assert!(!validator.is_valid(&value));

    let role_schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../benchmarks/cluster-transport-recovery-role.schema.json"
    ))
    .expect("parse recovery role schema");
    jsonschema::draft202012::meta::validate(&role_schema).expect("valid recovery role meta-schema");
    let role_validator =
        jsonschema::draft202012::new(&role_schema).expect("compile recovery role schema");
    let mut role_value = serde_json::to_value(role_report(RecoveryRole::Voter, 3))
        .expect("serialize recovery role fixture");
    role_value
        .as_object_mut()
        .expect("role report object")
        .insert("invented".to_owned(), serde_json::json!(true));
    assert!(!role_validator.is_valid(&role_value));

    let valid_role =
        serde_json::to_value(role_report(RecoveryRole::Voter, 3)).expect("role fixture");
    let mut unknown_campaign = valid_role.clone();
    unknown_campaign["campaign"]["invented"] = serde_json::json!(true);
    assert!(!role_validator.is_valid(&unknown_campaign));
    let mut unknown_nested = valid_role.clone();
    unknown_nested["campaign"]["cycles"][0]["source_outbound"]["invented"] =
        serde_json::json!(true);
    assert!(!role_validator.is_valid(&unknown_nested));
    let mut missing_campaign = valid_role;
    missing_campaign["campaign"] = serde_json::json!({});
    assert!(!role_validator.is_valid(&missing_campaign));
}
