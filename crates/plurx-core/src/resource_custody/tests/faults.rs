//! Real-process restart and storage-fault regression leaves (authored, not run).
use super::*;

#[cfg(unix)]
#[test]
fn uncertain_commit_retry_rereads_exact_operation_without_double_charge_or_rearm() {
    let root = tempfile::tempdir().expect("fixture");
    let identity = owner();
    let mut store = storage::Storage::open(&options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("install");
    transaction::register_owner(&mut store.connection, &identity).expect("owner");
    let (new, _reservation) = new_obligation(identity.owner_id, vector(1));
    let member = new.manifest.members[0].member_id;
    let handle = ObligationHandle {
        journal_id: store.witness.journal_id,
        owner_id: identity.owner_id,
        obligation_id: Uuid::new_v4(),
        graph_id: new.manifest.graph_id,
        runtime_binding: new.runtime_binding,
    };
    let family = FamilyHandle {
        journal_id: store.witness.journal_id,
        family_id: Uuid::new_v4(),
    };
    let operation = QueuedOperation {
        id: Uuid::new_v4(),
        deadline: deadline(),
        command: Command::Prepare {
            batch: PrepareBatch {
                obligations: vec![new],
                families: vec![FamilyRequest {
                    family,
                    roles: vec![(Role::Video, RoleTarget::New(0))],
                }],
                trial: true,
            },
            handles: vec![handle.clone()],
            trial: Some(Uuid::new_v4()),
        },
    };
    store.fail_after_commit_once = true;
    assert_eq!(
        transaction::execute(&mut store, &operation, identity.owner_id)
            .expect_err("lost acknowledgement")
            .certainty,
        CommitCertainty::Unknown
    );
    let snapshot = transaction::snapshot(&store.connection, &store.witness)
        .expect("durable exact operation exists");
    assert_eq!(snapshot.total, vector(1));
    assert_eq!(snapshot.obligations.len(), 1);
    let OperationResult::Prepared(prepared) =
        transaction::execute(&mut store, &operation, identity.owner_id).expect("same-op reread")
    else {
        panic!("prepare")
    };
    let arm = QueuedOperation {
        id: Uuid::new_v4(),
        deadline: deadline(),
        command: Command::Change {
            key: TransitionKey {
                handle,
                operation_id: Uuid::nil(),
                expected_sequence: prepared.sequence,
            },
            mutation: Mutation::Arm {
                members: vec![member],
            },
        },
    };
    // Keep both stored operation-ID positions identical, exactly like change().
    let mut arm = arm;
    if let Command::Change { key, .. } = &mut arm.command {
        key.operation_id = arm.id;
    }
    store.fail_after_commit_once = true;
    assert_eq!(
        transaction::execute(&mut store, &arm, identity.owner_id)
            .expect_err("lost arm ack")
            .certainty,
        CommitCertainty::Unknown
    );
    let OperationResult::Changed(ack) =
        transaction::execute(&mut store, &arm, identity.owner_id).expect("same arm op resolves")
    else {
        panic!("arm ack")
    };
    assert_eq!(ack.phase, Phase::AcquisitionPossible);
    let snapshot =
        transaction::snapshot(&store.connection, &store.witness).expect("full accounting");
    assert_eq!(snapshot.total, vector(1));
    assert_eq!(snapshot.obligations[0].sequence, ack.sequence);
    assert_eq!(snapshot.trial_groups.len(), 1);
    // Changing payload under same UUID must not manufacture another commit.
    if let Command::Change { mutation, .. } = &mut arm.command {
        *mutation = Mutation::Seal;
    }
    assert_eq!(
        transaction::execute(&mut store, &arm, identity.owner_id)
            .expect_err("mismatched retry")
            .kind,
        ErrorKind::Conflict
    );
}

#[cfg(unix)]
#[test]
fn interrupted_installing_empty_database_finishes_but_partial_ddl_is_preserved() {
    let root = tempfile::tempdir().expect("fixture");
    let identity = owner();
    let store = storage::Storage::open(&options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("install");
    drop(store);
    let witness = root.path().join("resource-custody/installation.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&witness).expect("witness")).expect("json");
    value["state"] = "installing".into();
    std::fs::write(&witness, serde_json::to_vec(&value).expect("encode"))
        .expect("installing fixture");
    let store = storage::Storage::open(&options(
        root.path(),
        identity.clone(),
        InstallationContext::Existing,
    ))
    .expect("finish matching empty installation");
    assert_eq!(store.witness.state, "ready");
    drop(store);
    std::fs::write(&witness, serde_json::to_vec(&value).expect("encode"))
        .expect("reset fixture installing");
    let db = root.path().join("resource-custody/obligations.sqlite3");
    let connection = rusqlite::Connection::open(&db).expect("open fixture");
    connection
        .execute_batch("CREATE TABLE ambiguous_partial_install(value BLOB);")
        .expect("partial ddl");
    drop(connection);
    let before = std::fs::read(&db).expect("before bytes");
    assert!(storage::Storage::open(&options(
        root.path(),
        identity,
        InstallationContext::Existing
    ))
    .is_err());
    assert_eq!(std::fs::read(&db).expect("preserved bytes"), before);
}

#[cfg(unix)]
#[test]
fn missing_witness_and_oversized_or_unknown_witness_never_rebuild() {
    let root = tempfile::tempdir().expect("fixture");
    let identity = owner();
    let store = storage::Storage::open(&options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("install");
    drop(store);
    let path = root.path().join("resource-custody");
    let db = path.join("obligations.sqlite3");
    let before = std::fs::read(&db).expect("database");
    let witness = std::fs::read(path.join("installation.json")).expect("witness");
    std::fs::remove_file(path.join("installation.json")).expect("remove fixture witness");
    assert!(storage::Storage::open(&options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation
    ))
    .is_err());
    assert_eq!(std::fs::read(&db).expect("preserved db"), before);
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path.join("installation.json"))
        .expect("restore file");
    use std::io::Write;
    file.write_all(&witness).expect("restore witness");
    drop(file);
    let mut value: serde_json::Value = serde_json::from_slice(&witness).expect("json");
    value["version"] = 2.into();
    std::fs::write(
        path.join("installation.json"),
        serde_json::to_vec(&value).expect("encode"),
    )
    .expect("unknown version");
    assert!(storage::Storage::open(&options(
        root.path(),
        identity.clone(),
        InstallationContext::Existing
    ))
    .is_err());
    std::fs::write(
        path.join("installation.json"),
        vec![b'x'; codec::JSON_LIMIT + 1],
    )
    .expect("oversized witness");
    assert!(storage::Storage::open(&options(
        root.path(),
        identity,
        InstallationContext::Existing
    ))
    .is_err());
    assert_eq!(std::fs::read(&db).expect("still preserved db"), before);
}

#[cfg(unix)]
#[test]
fn subprocess_crash_after_prepare_or_arm_reconstructs_full_cleanup_only_cost() {
    for phase in ["prepare", "arm", "seal"] {
        let root = tempfile::tempdir().expect("durable fixture");
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "resource_custody::tests::faults::crash_process_entry",
                "--nocapture",
            ])
            .env("PLURX_CUSTODY_CRASH_FIXTURE", root.path())
            .env("PLURX_CUSTODY_CRASH_PHASE", phase)
            .status()
            .expect("actual separate process");
        assert_eq!(
            status.code(),
            Some(73),
            "child must reach exact committed crash point"
        );
        let (journal, snapshot) =
            CustodyJournal::open(options(root.path(), owner(), InstallationContext::Existing))
                .expect("reconstruct after process exit");
        assert_eq!(snapshot.total, vector(1));
        assert_eq!(snapshot.obligations.len(), 1);
        assert!(snapshot.obligations[0].cleanup_only);
        assert_eq!(snapshot.trial_groups.len(), 1);
        let expected = match phase {
            "prepare" => Phase::Prepared,
            "arm" => Phase::AcquisitionPossible,
            _ => Phase::Sealed,
        };
        assert_eq!(snapshot.obligations[0].phase, expected);
        // Boot identity is deliberately Unknown; neither process exit nor a new
        // daemon UUID refunds even a PREPARED candidate without positive fencing.
        assert_eq!(snapshot.owners.len(), 1);
        journal.drain().expect("drain");
    }
}

#[cfg(unix)]
#[test]
fn crash_process_entry() {
    let Some(root) = std::env::var_os("PLURX_CUSTODY_CRASH_FIXTURE") else {
        return;
    };
    let root = PathBuf::from(root);
    let phase = std::env::var("PLURX_CUSTODY_CRASH_PHASE").expect("phase");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let identity = owner();
        let (journal, _) = CustodyJournal::open(options(
            &root,
            identity.clone(),
            InstallationContext::FirstInstallation,
        ))
        .expect("open");
        let (new, _reservation) = new_obligation(identity.owner_id, vector(1));
        let member = new.manifest.members[0].member_id;
        let batch = PrepareBatch {
            obligations: vec![new],
            families: vec![FamilyRequest {
                family: journal.new_family(),
                roles: vec![(Role::Video, RoleTarget::New(0))],
            }],
            trial: true,
        };
        let OperationResult::Prepared(prepared) = journal
            .prepare(Uuid::new_v4(), batch, deadline())
            .expect("queue")
            .wait()
            .await
            .expect("commit")
        else {
            panic!("prepared")
        };
        let mut sequence = prepared.sequence;
        if phase != "prepare" {
            let key = TransitionKey {
                handle: prepared.obligations[0].clone(),
                operation_id: Uuid::new_v4(),
                expected_sequence: sequence,
            };
            let OperationResult::Changed(ack) = journal
                .change(
                    key,
                    Mutation::Arm {
                        members: vec![member],
                    },
                    deadline(),
                )
                .expect("queue arm")
                .wait()
                .await
                .expect("arm committed")
            else {
                panic!("arm ack")
            };
            sequence = ack.sequence;
        }
        if phase == "seal" {
            let key = TransitionKey {
                handle: prepared.obligations[0].clone(),
                operation_id: Uuid::new_v4(),
                expected_sequence: sequence,
            };
            journal
                .change(key, Mutation::Seal, deadline())
                .expect("queue seal")
                .wait()
                .await
                .expect("seal committed");
        }
        // Deliberately do not run Drop/drain destructors: this is a real process
        // loss after a durable boundary, not a synthetic Rust handle drop.
        std::process::exit(73);
    });
}

#[cfg(unix)]
#[tokio::test]
async fn late_identity_after_seal_is_recorded_but_no_new_acquisition_is_allowed() {
    let root = tempfile::tempdir().expect("fixture");
    let identity = owner();
    let (journal, _) = CustodyJournal::open(options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("open");
    let (new, _reservation) = new_obligation(identity.owner_id, vector(1));
    let plan = new.manifest.members[0].clone();
    let batch = PrepareBatch {
        obligations: vec![new],
        families: vec![FamilyRequest {
            family: journal.new_family(),
            roles: vec![(Role::Video, RoleTarget::New(0))],
        }],
        trial: false,
    };
    let OperationResult::Prepared(prepared) = journal
        .prepare(Uuid::new_v4(), batch, deadline())
        .expect("queue")
        .wait()
        .await
        .expect("prepare")
    else {
        panic!("prepared")
    };
    let handle = prepared.obligations[0].clone();
    let mut sequence = prepared.sequence;
    for mutation in [
        Mutation::Arm {
            members: vec![plan.member_id],
        },
        Mutation::Seal,
        Mutation::RecordAcquired {
            identity: ObservedIdentity {
                version: 1,
                member_id: plan.member_id,
                adapter: plan.adapter,
                adapter_version: 1,
                domain: plan.domain,
                object_identity: "real-native-object-fixture".into(),
                process: None,
            },
        },
    ] {
        let key = TransitionKey {
            handle: handle.clone(),
            operation_id: Uuid::new_v4(),
            expected_sequence: sequence,
        };
        let OperationResult::Changed(ack) = journal
            .change(key, mutation, deadline())
            .expect("queue")
            .wait()
            .await
            .expect("commit")
        else {
            panic!("ack")
        };
        sequence = ack.sequence;
    }
    let key = TransitionKey {
        handle,
        operation_id: Uuid::new_v4(),
        expected_sequence: sequence,
    };
    assert!(journal
        .change(
            key,
            Mutation::Arm {
                members: vec![plan.member_id]
            },
            deadline()
        )
        .expect("queue rejected transition")
        .wait()
        .await
        .is_err());
    journal.drain().expect("drain");
    let (restarted, snapshot) =
        CustodyJournal::open(options(root.path(), owner(), InstallationContext::Existing))
            .expect("restart");
    assert_eq!(snapshot.obligations[0].phase, Phase::Sealed);
    let ObligationInventory::LegacyV1 { members, .. } = &snapshot.obligations[0].inventory else {
        panic!("v1 fixture inventory");
    };
    assert!(members[0].observed.is_some());
    assert_eq!(snapshot.total, vector(1));
    restarted.drain().expect("drain");
}
