//! Cross-row corruption and complete-family regression leaves; not executed here.
use super::*;

fn prepared_store(
    root: &std::path::Path,
) -> (
    storage::Storage,
    OwnerIdentity,
    QueuedOperation,
    ObligationHandle,
    i64,
) {
    let identity = owner();
    let mut store = storage::Storage::open(&options(
        root,
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("install");
    transaction::register_owner(&mut store.connection, &identity).expect("owner");
    let (new, _reservation) = new_obligation(identity.owner_id, vector(1));
    let handle = ObligationHandle {
        journal_id: store.witness.journal_id,
        owner_id: identity.owner_id,
        obligation_id: Uuid::new_v4(),
        graph_id: new.manifest.graph_id,
        runtime_binding: new.runtime_binding,
    };
    let operation = QueuedOperation {
        id: Uuid::new_v4(),
        deadline: deadline(),
        command: Command::Prepare {
            batch: PrepareBatch {
                obligations: vec![new],
                families: vec![FamilyRequest {
                    family: FamilyHandle {
                        journal_id: store.witness.journal_id,
                        family_id: Uuid::new_v4(),
                    },
                    roles: vec![(Role::Video, RoleTarget::New(0))],
                }],
                trial: false,
            },
            handles: vec![handle.clone()],
            trial: None,
        },
    };
    let OperationResult::Prepared(prepared) =
        transaction::execute(&mut store, &operation, identity.owner_id).expect("prepare")
    else {
        panic!("prepared")
    };
    (store, identity, operation, handle, prepared.sequence)
}

#[cfg(unix)]
#[test]
fn acquisition_possible_without_any_armed_member_is_unreadable_not_free() {
    let root = tempfile::tempdir().expect("fixture");
    let (store, _, _, _, _) = prepared_store(root.path());
    store
        .connection
        .execute("UPDATE obligations SET phase='acquisition_possible'", [])
        .expect("corrupt phase");
    assert!(transaction::snapshot(&store.connection, &store.witness).is_err());
    assert_eq!(
        store
            .connection
            .query_row("SELECT count(*) FROM obligations", [], |r| r
                .get::<_, i64>(0))
            .expect("preserved"),
        1
    );
}

#[cfg(unix)]
#[test]
fn changed_operation_wrong_uuid_graph_sequence_or_phase_refuses_reconstruction() {
    for field in ["operation_id", "graph_id", "sequence", "phase"] {
        let root = tempfile::tempdir().expect("fixture");
        let (mut store, identity, _, handle, sequence) = prepared_store(root.path());
        let id = Uuid::new_v4();
        let operation = QueuedOperation {
            id,
            deadline: deadline(),
            command: Command::Change {
                key: TransitionKey {
                    handle,
                    operation_id: id,
                    expected_sequence: sequence,
                },
                mutation: Mutation::Seal,
            },
        };
        transaction::execute(&mut store, &operation, identity.owner_id).expect("seal");
        let json: String = store
            .connection
            .query_row(
                "SELECT result_json FROM operations WHERE operation_id=?1",
                [id.as_bytes()],
                |r| r.get(0),
            )
            .expect("result");
        let mut value: serde_json::Value = serde_json::from_str(&json).expect("json");
        value[field] = match field {
            "sequence" => sequence.into(),
            "phase" => "prepared".into(),
            _ => Uuid::new_v4().to_string().into(),
        };
        store
            .connection
            .execute(
                "UPDATE operations SET result_json=?2 WHERE operation_id=?1",
                rusqlite::params![
                    id.as_bytes(),
                    serde_json::to_string(&value).expect("encode")
                ],
            )
            .expect("corrupt binding");
        assert!(
            transaction::snapshot(&store.connection, &store.witness).is_err(),
            "{field}"
        );
    }
}

#[cfg(unix)]
#[test]
fn missing_historical_operation_refuses_even_when_latest_operation_exists() {
    let root = tempfile::tempdir().expect("fixture");
    let (mut store, identity, prepare, handle, sequence) = prepared_store(root.path());
    let id = Uuid::new_v4();
    let operation = QueuedOperation {
        id,
        deadline: deadline(),
        command: Command::Change {
            key: TransitionKey {
                handle,
                operation_id: id,
                expected_sequence: sequence,
            },
            mutation: Mutation::Seal,
        },
    };
    transaction::execute(&mut store, &operation, identity.owner_id).expect("seal");
    store
        .connection
        .execute(
            "DELETE FROM operations WHERE operation_id=?1",
            [prepare.id.as_bytes()],
        )
        .expect("truncate history");
    assert!(transaction::snapshot(&store.connection, &store.witness).is_err());
}

#[cfg(unix)]
#[test]
fn divergent_member_settlement_record_cannot_clear_recovered_charge() {
    let root = tempfile::tempdir().expect("fixture");
    let (mut store, identity, _, handle, sequence) = prepared_store(root.path());
    let id = Uuid::new_v4();
    let operation = QueuedOperation {
        id,
        deadline: deadline(),
        command: Command::Change {
            key: TransitionKey {
                handle,
                operation_id: id,
                expected_sequence: sequence,
            },
            mutation: Mutation::CancelPrepared,
        },
    };
    transaction::execute(&mut store, &operation, identity.owner_id).expect("cancel never armed");
    transaction::snapshot(&store.connection, &store.witness).expect("valid settlement audit");
    let wrong = codec::SettlementRecord::PreparedNeverArmed {
        version: 1,
        owner_id: Uuid::new_v4(),
    };
    store
        .connection
        .execute(
            "UPDATE members SET proof_json=?1",
            [codec::encode(&wrong).expect("encode")],
        )
        .expect("diverge member proof");
    assert!(transaction::snapshot(&store.connection, &store.witness).is_err());
}

#[cfg(unix)]
#[test]
fn existing_family_request_cannot_omit_preexisting_role_edges() {
    let root = tempfile::tempdir().expect("fixture");
    let (mut store, identity, prepare, handle, _) = prepared_store(root.path());
    let Command::Prepare { batch, .. } = &prepare.command else {
        panic!("prepare")
    };
    let (new, _reservation) = new_obligation(identity.owner_id, vector(1));
    let new_handle = ObligationHandle {
        obligation_id: Uuid::new_v4(),
        graph_id: new.manifest.graph_id,
        runtime_binding: new.runtime_binding,
        ..handle
    };
    let operation = QueuedOperation {
        id: Uuid::new_v4(),
        deadline: deadline(),
        command: Command::Prepare {
            batch: PrepareBatch {
                obligations: vec![new],
                families: vec![FamilyRequest {
                    family: batch.families[0].family.clone(),
                    roles: vec![(Role::Audio, RoleTarget::New(0))],
                }],
                trial: false,
            },
            handles: vec![new_handle],
            trial: None,
        },
    };
    assert!(transaction::execute(&mut store, &operation, identity.owner_id).is_err());
    let snapshot =
        transaction::snapshot(&store.connection, &store.witness).expect("prior authority intact");
    assert_eq!(snapshot.total, vector(1));
    assert_eq!(snapshot.families.len(), 1);
}

#[cfg(unix)]
#[test]
fn aggregate_overflow_and_exhausted_sequence_refuse_without_partial_rows() {
    let root = tempfile::tempdir().expect("fixture");
    let (mut store, identity, _, handle, _) = prepared_store(root.path());
    store
        .connection
        .execute(
            "UPDATE obligations SET resources=?1",
            [codec::vector_bytes(vector(u64::MAX))],
        )
        .expect("full u64 charge");
    let (new, _reservation) = new_obligation(identity.owner_id, vector(1));
    let new_handle = ObligationHandle {
        obligation_id: Uuid::new_v4(),
        graph_id: new.manifest.graph_id,
        runtime_binding: new.runtime_binding,
        ..handle
    };
    let operation = QueuedOperation {
        id: Uuid::new_v4(),
        deadline: deadline(),
        command: Command::Prepare {
            batch: PrepareBatch {
                obligations: vec![new],
                families: vec![FamilyRequest {
                    family: FamilyHandle {
                        journal_id: store.witness.journal_id,
                        family_id: Uuid::new_v4(),
                    },
                    roles: vec![(Role::Video, RoleTarget::New(0))],
                }],
                trial: false,
            },
            handles: vec![new_handle],
            trial: None,
        },
    };
    assert!(transaction::execute(&mut store, &operation, identity.owner_id).is_err());
    assert_eq!(
        transaction::snapshot(&store.connection, &store.witness)
            .expect("full historical charge")
            .total,
        vector(u64::MAX)
    );
    store
        .connection
        .execute(
            "UPDATE obligations SET resources=?1",
            [codec::vector_bytes(vector(1))],
        )
        .expect("restore fixture cost");
    store
        .connection
        .execute("UPDATE journal_meta SET next_sequence=?1", [i64::MAX])
        .expect("exhaust sequence fixture");
    assert_eq!(
        transaction::execute(&mut store, &operation, identity.owner_id)
            .expect_err("no wrap")
            .kind,
        ErrorKind::Exhausted
    );
    assert_eq!(
        store
            .connection
            .query_row("SELECT count(*) FROM obligations", [], |r| r
                .get::<_, i64>(0))
            .expect("rows"),
        1
    );
}
