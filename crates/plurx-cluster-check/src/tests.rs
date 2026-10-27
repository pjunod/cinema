#[test]
fn harness_node_identities_are_distinct_canonical_reduction_targets() {
    let ids = [1, 2, 3, 4, u64::from(u32::MAX) + 1, u64::MAX];
    let mut identities = std::collections::BTreeSet::new();
    for id in ids {
        let identity = harness_node_identity(id);
        assert!(
            identities.insert(identity.clone()),
            "fixture identities collided"
        );
        let reference = hiqlite::ReductionFenceReference {
            version: 1,
            target_node_id: identity,
            target_raft_id: id,
            attempt_id: "01000000-0000-4000-8000-000000000001".to_owned(),
            barrier_index: 1,
        };
        assert!(
            reference.has_valid_shape(),
            "fixture is refused by the real protocol"
        );
        let legacy = hiqlite::ReductionFenceReference {
            target_node_id: format!("node-{id}"),
            ..reference
        };
        assert!(
            !legacy.has_valid_shape(),
            "the original invalid control must remain refused"
        );
    }
}
use plurx_core::cluster::migration::HIQLITE_WAL_SIZE_BYTES;

use super::*;

#[test]
fn inspect_wal_arguments_are_explicit_and_order_independent() {
    let parsed = parse_inspect_wal_args(&[
        "--output".to_owned(),
        "report.json".to_owned(),
        "--hiqlite-dir".to_owned(),
        "forensic/hiqlite".to_owned(),
    ])
    .expect("valid inspect-wal arguments");
    assert_eq!(parsed.output, PathBuf::from("report.json"));
    assert_eq!(parsed.hiqlite_dir, PathBuf::from("forensic/hiqlite"));
    assert!(parse_inspect_wal_args(&["--output".to_owned()]).is_err());
    assert!(parse_inspect_wal_args(&[
        "--output".to_owned(),
        "a".to_owned(),
        "--output".to_owned(),
        "b".to_owned(),
    ])
    .is_err());
}

#[test]
fn state_machine_inspection_includes_a_committed_sqlite_wal_sidecar() {
    let root = tempfile::tempdir().expect("state-machine fixture");
    let path = root.path().join("plurx.db");
    let connection = Connection::open(&path).expect("open fixture database");
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .expect("enable WAL mode");
    connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .expect("retain committed WAL pages");
    connection
        .execute(
            "CREATE TABLE _metadata (key TEXT PRIMARY KEY, data BLOB NOT NULL)",
            [],
        )
        .expect("metadata table");
    let expected = InspectedLogId {
        term: 11,
        node_id: 4,
        index: 42,
    };
    let mut encoded = vec![1_u8];
    encoded.extend_from_slice(&expected.term.to_le_bytes());
    encoded.extend_from_slice(&expected.node_id.to_le_bytes());
    encoded.extend_from_slice(&expected.index.to_le_bytes());
    connection
        .execute(
            "INSERT INTO _metadata (key, data) VALUES ('meta', ?1)",
            rusqlite::params![encoded],
        )
        .expect("committed metadata in WAL");
    assert!(sqlite_sidecar_path(&path, "-wal").is_file());

    let (present, wal_present, applied) =
        read_local_state_machine_boundary(&path).expect("inspect source through private copy");
    assert!(present);
    assert!(wal_present);
    assert_eq!(applied, Some(expected));
}

fn test_media_child(admission_id: u64) -> MediaChild {
    let mut command = Command::new("sh");
    command
        .args(["-c", "cat >/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().expect("spawn test media child");
    let input = child.stdin.take().expect("retain test child stdin");
    MediaChild {
        child,
        _input: input,
        admitted_generation: 7,
        admission_id,
    }
}

#[tokio::test]
async fn delayed_media_teardown_cannot_take_or_hide_its_replacement() {
    let media = Arc::new(tokio::sync::Mutex::new(Some(test_media_child(1))));

    let delayed_old = take_media_child_if_id(&media, 1)
        .await
        .expect("old admission owns its child");
    *media.lock().await = Some(test_media_child(2));

    assert!(take_media_child_if_id(&media, 1).await.is_none());
    reap_media_child(delayed_old).await;

    let mut slot = media.lock().await;
    let replacement = slot.as_mut().expect("replacement remains registered");
    assert_eq!(replacement.admission_id, 2);
    assert!(matches!(replacement.child.try_wait(), Ok(None)));
    drop(slot);

    kill_media_child_if_id(&media, 2).await;
    assert!(media.lock().await.is_none());
}

#[test]
fn singleton_retry_requires_the_explicit_trial_marker() {
    assert!(singleton_trial_was_unstable(
        b"startup\nCLUSTER_SINGLETON_UNSTABLE stage=after-resume\n"
    ));
    assert!(!singleton_trial_was_unstable(
        b"Error: singleton proof changed leader/term during its measured interval\n"
    ));
    assert!(!singleton_trial_was_unstable(
        b"diagnostic mentioned CLUSTER_SINGLETON_UNSTABLE but was not a verdict\n"
    ));
}

#[test]
fn real_node_identities_satisfy_exact_reduction_transport() {
    let ids = [1, 2, 3, u64::MAX].map(harness_node_identity);
    assert_eq!(ids.iter().collect::<BTreeSet<_>>().len(), ids.len());
    for (raft_id, target_node_id) in [1, 2, 3, u64::MAX].into_iter().zip(ids) {
        let reference = hiqlite::ReductionFenceReference {
            version: 1,
            target_node_id,
            target_raft_id: raft_id,
            attempt_id: "00000000-0000-0000-0000-000000000001".to_owned(),
            barrier_index: 1,
        };
        assert!(reference.has_valid_shape(), "{reference:?}");
    }
}

#[test]
fn membership_harness_retries_only_typed_leader_changes() {
    assert!(matches!(
        membership_error_response(MembershipError::LeaderChanged("routing".to_owned())),
        Response::MembershipLeaderChange { message } if message == "routing"
    ));
    assert!(matches!(
        membership_error_response(MembershipError::Internal(
            "LeaderChange text from an unrelated semantic error".to_owned()
        )),
        Response::MembershipError { code, .. } if code == "membership_internal"
    ));
}

#[test]
fn read_only_repair_repeat_retries_only_stale_or_rerouted_observations() {
    assert!(
        repeatable_artwork_observation_succeeded(Response::Flag { value: true })
            .expect("a successful read-only repeat is classified")
    );
    assert!(
        !repeatable_artwork_observation_succeeded(Response::Flag { value: false })
            .expect("a stale read-only repeat is classified")
    );
    assert!(
        !repeatable_artwork_observation_succeeded(Response::MembershipLeaderChange {
            message: "election".to_owned(),
        })
        .expect("a rerouted read-only repeat is classified")
    );
    assert!(repeatable_artwork_observation_succeeded(Response::Ok).is_err());
}

#[test]
fn post_cas_repair_proof_distinguishes_quorum_age_from_topology_change() {
    let metrics = |leader, current_term, quorum_acknowledged| Response::Metrics {
        leader,
        current_term,
        voters: vec![1, 2, 3],
        members: vec![1, 2, 3],
        applied_index: Some(41),
        quorum_acknowledged,
    };

    require_artwork_repair_cas_topology(metrics(Some(2), 9, true), 2, 9)
        .expect("fresh matching topology is valid");
    require_artwork_repair_cas_topology(metrics(Some(2), 9, false), 2, 9)
        .expect("aged quorum freshness does not rewrite committed topology");
    assert!(require_artwork_repair_cas_topology(metrics(Some(3), 9, true), 2, 9).is_err());
    assert!(require_artwork_repair_cas_topology(metrics(Some(2), 10, true), 2, 9).is_err());
    assert!(require_artwork_repair_cas_topology(metrics(None, 9, false), 2, 9).is_err());
}

#[test]
fn read_only_repair_evidence_preserves_forward_to_leader_as_typed() {
    // The unit tests live in tests.rs, so lib.rs is production source only.
    let production = include_str!("lib.rs");
    let handler = production
        .rsplit_once("Request::ReadArtworkRepairFence { item_id } => {")
        .expect("repair evidence request handler")
        .1
        .split_once("Request::ClaimArtworkRepair")
        .expect("next request handler")
        .0;
    assert!(handler.contains("error.is_forward_to_leader().is_some()"));
    assert!(handler.contains("Response::MembershipLeaderChange"));
}

#[test]
fn mutating_artwork_claim_helper_has_exactly_one_dispatch() {
    let source = include_str!("lib.rs");
    let helper = source
        .split_once("async fn claim_artwork_repair_fence_once")
        .expect("single-dispatch claim helper")
        .1
        .split_once("async fn run_leader_self_leave_case")
        .expect("helper boundary")
        .0;
    assert_eq!(helper.matches(".request(").count(), 1);
    assert!(helper.contains("Response::MembershipLeaderChange"));
    assert!(helper.contains("was ambiguous and was not retried"));
}

#[test]
fn mutating_artwork_claim_retries_only_a_confirmed_noop() {
    let fence = ArtworkRepairFence {
        item_id: 17,
        owner_node_id: "node-2".to_owned(),
        leader_term: 9,
        generation: 3,
    };
    assert_eq!(
        classify_artwork_repair_claim_response(Response::ArtworkRepairFence {
            fence: Some(fence.clone()),
        })
        .expect("a committed repair claim is classified"),
        Some(fence)
    );
    assert!(
        classify_artwork_repair_claim_response(Response::ArtworkRepairFence { fence: None })
            .expect("an explicit no-op repair claim is classified")
            .is_none()
    );
    assert!(
        classify_artwork_repair_claim_response(Response::MembershipLeaderChange {
            message: "acknowledgement lost after submission".to_owned(),
        })
        .is_err()
    );
    assert!(classify_artwork_repair_claim_response(Response::Ok).is_err());
}

#[test]
fn stable_term_rerepair_reestablishes_quorum_before_the_cas() {
    // The unit tests live in tests.rs, so lib.rs is production source only.
    let production = include_str!("lib.rs");
    let proof = production
        .split_once("let stable_claim_deadline")
        .expect("stable-term repair proof")
        .1
        .split_once("let same_term_after")
        .expect("stable-term repair proof boundary")
        .0;
    assert!(proof.contains("CONVERGENCE_TIMEOUT"));
    assert!(proof.contains("quorum_acknowledged: true"));
    assert!(proof.contains("claim_artwork_repair_fence_once"));
    assert!(!proof.contains("Request::ClaimArtworkRepairFence"));
}

#[test]
fn compaction_plan_stops_writing_at_the_exact_openraft_trigger() {
    assert_eq!(
        snapshot_trigger_plan(None, 9_998).expect("plan before initial trigger"),
        1
    );
    assert_eq!(
        snapshot_trigger_plan(None, 9_999).expect("plan initial trigger"),
        0
    );
    assert_eq!(
        snapshot_trigger_plan(Some(0), 9_999).expect("plan before index-zero trigger"),
        1
    );
    assert_eq!(
        snapshot_trigger_plan(Some(0), 10_000).expect("plan index-zero trigger"),
        0
    );
    assert_eq!(
        snapshot_trigger_plan(Some(1_000), 1_512).expect("plan partial tail"),
        9_488
    );
    assert_eq!(
        snapshot_trigger_plan(Some(1_000), 11_609).expect("plan in-flight snapshot"),
        0
    );
    assert!(snapshot_trigger_plan(Some(1_000), 999).is_err());
    assert!(snapshot_trigger_plan(Some(u64::MAX), u64::MAX).is_err());
}

#[test]
fn compaction_request_outlives_snapshot_publication_and_purge_bounds() {
    let request = Request::ForceCompaction {
        phase: "response-timeout-contract".to_owned(),
        background: Vec::new(),
    };
    assert!(request.response_timeout() > Duration::from_secs(60));
    assert_eq!(Request::Metrics.response_timeout(), REQUEST_TIMEOUT);
    assert_eq!(
        Request::WriteWithoutQuorum.response_timeout(),
        WRITE_WITHOUT_QUORUM_RESPONSE_TIMEOUT
    );
}

fn test_launch(root: &Path, read_pool_size: usize) -> NodeLaunch {
    NodeLaunch {
        node_id: 1,
        root: root.to_path_buf(),
        nodes: vec![NodeSpec {
            id: 1,
            raft: "127.0.0.1:19001".to_owned(),
            api: "127.0.0.1:19002".to_owned(),
        }],
        listen_addr: default_listen_addr(),
        read_pool_size,
        snapshot_logs_since_last: None,
        instrument_store_operations: true,
        emulate_old_watermark_handler: false,
        emulate_p3a_watermark_handler: false,
        role: ClusterRole::Voter,
        emulate_pre_learner_heartbeat: false,
    }
}

#[test]
fn voter_config_uses_the_production_wal_size() {
    let root = tempfile::tempdir().expect("config test root");
    let launch = test_launch(root.path(), default_read_pool_size());

    let config = node_config(&launch).expect("build the voter config");
    assert_eq!(config.wal_size, HIQLITE_WAL_SIZE_BYTES);
    assert_eq!(config.health_check_delay_secs, 0);
}

/// A read-pool comparison is only evidence if the voter under measurement
/// actually runs the pool the arm claims. The harness built its own
/// `NodeConfig` and never set this, so 4, 8, and 16 all measured Hiqlite's
/// default pool.
#[test]
fn the_launched_voter_runs_the_read_pool_it_was_given() {
    let root = tempfile::tempdir().expect("config test root");
    for size in [4, 8, 16] {
        let config = node_config(&test_launch(root.path(), size)).expect("build the voter config");
        assert_eq!(config.read_pool_size, size);
        // The extracted defaults must arrive with it, not be traded for it.
        assert_eq!(config.wal_size, HIQLITE_WAL_SIZE_BYTES);
    }
}

/// A launch written by an older controller carries no pool size, and must
/// land on the daemon's default rather than Hiqlite's.
#[test]
fn a_launch_without_a_read_pool_size_uses_the_daemon_default() {
    let launch: NodeLaunch = serde_json::from_str(
            r#"{"node_id":1,"root":"/data","nodes":[{"id":1,"raft":"127.0.0.1:19001","api":"127.0.0.1:19002"}]}"#,
        )
        .expect("decode a legacy launch");
    assert_eq!(launch.read_pool_size, default_read_pool_size());
    assert_eq!(launch.read_pool_size, 4);
    assert!(launch.instrument_store_operations);
}

#[test]
fn a_legacy_launch_retains_the_production_snapshot_policy() {
    let root = tempfile::tempdir().expect("config test root");
    let launch: NodeLaunch = serde_json::from_value(serde_json::json!({
        "node_id": 1,
        "root": root.path(),
        "nodes": [{
            "id": 1,
            "raft": "127.0.0.1:19001",
            "api": "127.0.0.1:19002"
        }]
    }))
    .expect("decode a legacy launch");
    assert_eq!(launch.snapshot_logs_since_last, None);
    assert!(
        serde_json::to_value(&launch)
            .expect("encode the legacy launch")
            .get("snapshot_logs_since_last")
            .is_none(),
        "an absent override must preserve the legacy launch payload"
    );

    let config = node_config(&launch).expect("build the voter config");
    let production = production_hiqlite_defaults_with_read_pool(default_read_pool_size());
    assert_eq!(
        config.raft_config.snapshot_policy,
        production.raft_config.snapshot_policy
    );
    assert_eq!(
        config.raft_config.replication_lag_threshold,
        production.raft_config.replication_lag_threshold
    );
}

#[test]
fn a_snapshot_threshold_round_trips_and_reaches_the_launched_voter_config() {
    let root = tempfile::tempdir().expect("config test root");
    let launch =
        test_launch(root.path(), default_read_pool_size()).with_snapshot_logs_since_last(37);
    let encoded = serde_json::to_string(&launch).expect("encode the launch");
    let decoded: NodeLaunch = serde_json::from_str(&encoded).expect("decode the launch");
    assert_eq!(decoded.snapshot_logs_since_last, Some(37));

    let config = node_config(&decoded).expect("build the voter config");
    let expected = NodeConfig::default_raft_config(37);
    assert_eq!(config.raft_config.snapshot_policy, expected.snapshot_policy);
    assert_eq!(config.raft_config.replication_lag_threshold, 74);
    assert_eq!(config.raft_config.install_snapshot_timeout, 120_000);
    assert_eq!(config.wal_size, HIQLITE_WAL_SIZE_BYTES);
}

#[test]
fn an_invalid_snapshot_threshold_is_rejected_without_changing_production() {
    let root = tempfile::tempdir().expect("config test root");
    for threshold in [0, u64::MAX] {
        let mut launch = test_launch(root.path(), default_read_pool_size());
        launch.snapshot_logs_since_last = Some(threshold);
        let error = node_config(&launch).expect_err("reject an unsafe snapshot threshold");
        assert!(
            error
                .to_string()
                .contains("validation snapshot threshold must be in"),
            "unexpected error: {error:#}"
        );
        assert!(
            !root.path().join("node-1").exists(),
            "an invalid launch must fail before creating voter state"
        );
    }
}
