//! Owner-book and retry fault regression bodies, compiled without execution.
use super::super::tests::{deadline, new_obligation, options, owner, vector};
use super::*;

#[cfg(unix)]
#[test]
fn expired_retry_cannot_downgrade_prior_unknown_commit_or_forget_its_command() {
    let root = tempfile::tempdir().expect("fixture");
    let identity = owner();
    let mut storage = storage::Storage::open(&options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("storage");
    let mut error = CustodyError::new(ErrorKind::Storage, "fixture uncertain result");
    error.certainty = CommitCertainty::Unknown;
    let id = Uuid::new_v4();
    let owned = Arc::new(Mutex::new(OwnedOperation {
        operation: QueuedOperation {
            id,
            deadline: Instant::now(),
            command: empty_command(),
        },
        result: Some(Err(error)),
        queued: true,
    }));
    let book = Arc::new(Mutex::new(Book {
        operations: BTreeMap::from([(id, owned.clone())]),
    }));
    let error = run(&mut storage, identity.owner_id, &book, &owned)
        .expect_err("deadline cannot resolve old commit");
    assert_eq!(error.kind, ErrorKind::Deadline);
    assert_eq!(error.certainty, CommitCertainty::Unknown);
    assert!(
        matches!(&owned.lock().expect("owned").result, Some(Err(e)) if e.certainty == CommitCertainty::Unknown)
    );
    assert!(!owned.lock().expect("owned").queued);
}

#[cfg(unix)]
#[tokio::test]
async fn definitive_failed_operations_are_reclaimable_without_leaking_owner_book_slots() {
    let root = tempfile::tempdir().expect("fixture");
    let identity = owner();
    let (journal, _) = CustodyJournal::open(options(
        root.path(),
        identity,
        InstallationContext::FirstInstallation,
    ))
    .expect("open");
    for _ in 0..8 {
        let id = Uuid::new_v4();
        let invalid = PrepareBatch {
            obligations: Vec::new(),
            families: Vec::new(),
            trial: false,
        };
        assert!(journal
            .prepare(id, invalid, deadline())
            .expect("queue")
            .wait()
            .await
            .is_err());
        journal
            .acknowledge(id)
            .expect("forget definitively failed prepare");
        assert!(journal.book.lock().expect("book").operations.is_empty());
    }
    journal.drain().expect("drain");
}

#[cfg(unix)]
#[tokio::test]
async fn failed_transition_returns_original_key_and_mutation_only_after_definitive_rollback() {
    let root = tempfile::tempdir().expect("fixture");
    let identity = owner();
    let (journal, _) = CustodyJournal::open(options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("open");
    let (new, _reservation) = new_obligation(identity.owner_id, vector(1));
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
    let id = Uuid::new_v4();
    let key = TransitionKey {
        handle: prepared.obligations[0].clone(),
        operation_id: id,
        expected_sequence: prepared.sequence,
    };
    assert!(journal
        .change(
            key.clone(),
            Mutation::Arm {
                members: Vec::new()
            },
            deadline()
        )
        .expect("queue")
        .wait()
        .await
        .is_err());
    let rejected = journal
        .take_failed_transition(id)
        .expect("return actual command");
    assert_eq!(rejected.key.operation_id, id);
    assert_eq!(rejected.key.handle.obligation_id, key.handle.obligation_id);
    assert!(matches!(rejected.mutation, Mutation::Arm { members } if members.is_empty()));
    assert_ne!(rejected.error.certainty, CommitCertainty::Unknown);
    assert!(
        journal.retry(id, deadline()).is_err(),
        "no detached retry after return"
    );
    journal.drain().expect("drain");
}

#[cfg(unix)]
#[test]
fn uncertain_other_obligation_prevents_new_arm_but_preserves_cleanup_custody() {
    let root = tempfile::tempdir().expect("fixture");
    let identity = owner();
    let mut storage = storage::Storage::open(&options(
        root.path(),
        identity.clone(),
        InstallationContext::FirstInstallation,
    ))
    .expect("storage");
    let (new, _reservation) = new_obligation(identity.owner_id, vector(1));
    let handle = ObligationHandle {
        journal_id: storage.witness.journal_id,
        owner_id: identity.owner_id,
        obligation_id: Uuid::new_v4(),
        graph_id: new.manifest.graph_id,
        runtime_binding: new.runtime_binding,
    };
    let mut unknown = CustodyError::new(ErrorKind::Storage, "fixture uncertain result");
    unknown.certainty = CommitCertainty::Unknown;
    let old_id = Uuid::new_v4();
    let old = Arc::new(Mutex::new(OwnedOperation {
        operation: QueuedOperation {
            id: old_id,
            deadline: deadline(),
            command: empty_command(),
        },
        result: Some(Err(unknown)),
        queued: false,
    }));
    let id = Uuid::new_v4();
    let armed = Arc::new(Mutex::new(OwnedOperation {
        operation: QueuedOperation {
            id,
            deadline: deadline(),
            command: Command::Change {
                key: TransitionKey {
                    handle,
                    operation_id: id,
                    expected_sequence: 1,
                },
                mutation: Mutation::Arm {
                    members: vec![new.manifest.members[0].member_id],
                },
            },
        },
        result: None,
        queued: true,
    }));
    let book = Arc::new(Mutex::new(Book {
        operations: BTreeMap::from([(old_id, old.clone()), (id, armed.clone())]),
    }));
    let error = run(&mut storage, identity.owner_id, &book, &armed)
        .expect_err("no new acquisition under uncertain storage");
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(
        matches!(&old.lock().expect("old custody").result, Some(Err(e)) if e.certainty == CommitCertainty::Unknown)
    );
    assert_eq!(
        storage
            .connection
            .query_row("SELECT count(*) FROM operations", [], |r| r
                .get::<_, i64>(0))
            .expect("no write"),
        0
    );
}
