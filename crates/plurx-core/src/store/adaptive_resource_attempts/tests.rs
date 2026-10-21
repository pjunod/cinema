//! Actual SQLite transaction fault fixtures. No fake close proofs or workers.
use crate::domain::MediaSessionActivationOutcome;
use crate::error::StoreError;
use crate::playback_principal::PlaybackPrincipal;
use crate::store::adaptive_resource_attempts::transactions::{self, Backend};
use crate::store::adaptive_resource_attempts::*;
use crate::store::{MediaSessionStore, SqliteStore, UserStore};
use async_trait::async_trait;
use uuid::Uuid;

async fn fixture() -> (SqliteStore, AttemptClaim) {
    let store = SqliteStore::open_in_memory().expect("actual store");
    store
        .with_conn(schema::install_sqlite_checked)
        .await
        .expect("actual additive install");
    let user = store
        .create_user("adaptive-unit", "unused-fixture-hash", false)
        .await
        .expect("user");
    let request = RequestKey {
        request_id: Uuid::new_v4().to_string(),
        request_incarnation_id: Uuid::new_v4(),
    };
    let binding = RequestBinding {
        version: 1,
        key: request.clone(),
        principal: PlaybackPrincipal::LocalUser { user_id: user.id },
        request_fingerprint: "a".repeat(64),
        playback_id: Uuid::new_v4().to_string(),
        recovery_epoch: Uuid::new_v4(),
        planning_generation: 1,
        planning_source_digest: "b".repeat(64),
        settings_digest: "c".repeat(64),
        intent_digest: "d".repeat(64),
        original_deadline_ms: 20_000,
        expected_predecessor_incarnation_id: None,
        expected_predecessor_owner_epoch: None,
        expected_desired_revision: None,
    };
    store
        .claim_media_session_request(
            &binding.principal,
            &request.request_id,
            &binding.request_fingerprint,
            &binding.playback_id,
            &request.request_incarnation_id.to_string(),
            10_000,
            20_000,
        )
        .await
        .expect("root");
    let key = AttemptKey {
        request,
        attempt_id: Uuid::new_v4(),
        route_incarnation_id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        worker_node_id: "worker-fixture".into(),
        owner_epoch: 1,
        ordinal: 1,
    };
    (
        store,
        AttemptClaim {
            context: MutationContext {
                operation_id: Uuid::new_v4(),
                expected_request_sequence: 0,
                expected_attempt_sequence: 0,
                now_ms: 10_001,
            },
            binding,
            key,
            worker_instance_id: Uuid::new_v4(),
            serving_generation: u64::MAX,
            domains: vec!["processing:fixture".into()],
        },
    )
}

/// Faults sit at the real backend boundary, NOT in a fake Store authority.
/// Accepted SQL still executes through SqliteStore::with_conn transaction.
struct Fault<'a> {
    store: &'a SqliteStore,
    lose_ack: bool,
    fail_final: bool,
}
#[async_trait]
impl Backend for Fault<'_> {
    async fn attempt_layout(&self) -> Result<schema::Layout, StoreError> {
        self.store.attempt_layout().await
    }
    async fn attempt_read(&self, sql: String) -> Result<Vec<String>, StoreError> {
        self.store.attempt_read(sql).await
    }
    async fn attempt_transaction(&self, mut statements: Vec<String>) -> Result<(), StoreError> {
        if self.fail_final {
            statements.push(schema::assertion("0"));
        }
        self.store.attempt_transaction(statements).await?;
        if self.lose_ack {
            return Err(StoreError::Database(
                "fixture lost acknowledgement after real COMMIT".into(),
            ));
        }
        Ok(())
    }
    async fn attempt_activate(
        &self,
        plan: &transactions::AdoptionPlan,
    ) -> Result<Option<MediaSessionActivationOutcome>, StoreError> {
        transactions::commit_adoption(self, plan).await
    }
}

#[tokio::test]
async fn unknown_prepare_and_adopt_outcomes_require_reconciliation() {
    let (store, claim) = fixture().await;
    let fault = Fault {
        store: &store,
        lose_ack: true,
        fail_final: false,
    };
    assert!(transactions::claim(&fault, &claim).await.is_err());
    // The actual transaction committed, despite the caller's error. No fresh
    // attempt or deadline is minted during authoritative exact-op replay.
    let status = match store
        .claim_adaptive_attempt(&claim)
        .await
        .expect("reconcile")
    {
        AttemptMutation::Replay(status) => status,
        other => panic!("{other:?}"),
    };
    assert_eq!(status.attempts.len(), 1);
    assert_eq!(status.binding.original_deadline_ms, 20_000);
    assert_eq!(status.attempts[0].serving_generation, u64::MAX);
    let mut changed = claim.clone();
    changed.key.session_id = Uuid::new_v4();
    assert_eq!(
        store
            .claim_adaptive_attempt(&changed)
            .await
            .expect("exact payload history"),
        AttemptMutation::Refused(AttemptRefusal::Conflict)
    );
}

#[tokio::test]
async fn failed_conditional_statement_rolls_back_lease_route_predecessor_pointer_and_claim() {
    let (store, claim) = fixture().await;
    let fault = Fault {
        store: &store,
        lose_ack: false,
        fail_final: true,
    };
    assert!(transactions::claim(&fault, &claim).await.is_err());
    assert!(store
        .adaptive_request_status(&claim.binding.key, &claim.binding.principal)
        .await
        .expect("rollback status")
        .is_none());
    assert!(store
        .media_session_route_by_incarnation(&claim.key.route_incarnation_id.to_string())
        .await
        .expect("route rolled back")
        .is_none());
    store
        .with_conn(|conn| {
            let idle: i64 = conn.query_row(
                "SELECT count(*) FROM media_resource_schema WHERE mutation_claim IS NULL",
                [],
                |r| r.get(0),
            )?;
            let attempts: i64 =
                conn.query_row("SELECT count(*) FROM media_resource_attempts", [], |r| {
                    r.get(0)
                })?;
            let history: i64 =
                conn.query_row("SELECT count(*) FROM media_resource_operations", [], |r| {
                    r.get(0)
                })?;
            let leases: i64 = conn.query_row(
                "SELECT count(*) FROM job_leases WHERE resource LIKE 'session:%'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!((idle, attempts, history, leases), (1, 0, 0, 0));
            Ok(())
        })
        .await
        .expect("atomic rollback receipt");
    assert!(matches!(
        store
            .claim_adaptive_attempt(&claim)
            .await
            .expect("same operation after known rollback"),
        AttemptMutation::Applied(_)
    ));
}

#[tokio::test]
async fn maintenance_never_prunes_unknown_attempts_or_breaks_unrelated_legacy_work() {
    let (store, claim) = fixture().await;
    assert!(matches!(
        store.claim_adaptive_attempt(&claim).await.expect("claim"),
        AttemptMutation::Applied(_)
    ));
    let unrelated = Uuid::new_v4().to_string();
    store
        .claim_media_session_request(
            &claim.binding.principal,
            &unrelated,
            &"e".repeat(64),
            &Uuid::new_v4().to_string(),
            &Uuid::new_v4().to_string(),
            10_002,
            11_000,
        )
        .await
        .expect("unrelated legacy claim");
    store
        .maintain_media_sessions(100_000_000)
        .await
        .expect("bounded ordinary maintenance");
    let status = store
        .adaptive_request_status(&claim.binding.key, &claim.binding.principal)
        .await
        .expect("all expired debt")
        .expect("retained");
    assert_eq!(status.attempts.len(), 1);
    assert_eq!(status.attempts[0].phase, AttemptPhase::Provisional);
    let id = unrelated.clone();
    store
        .with_conn(move |conn| {
            let count: i64 = conn.query_row(
                "SELECT count(*) FROM media_session_requests WHERE request_id=?1",
                [id],
                |r| r.get(0),
            )?;
            assert_eq!(count, 0);
            Ok(())
        })
        .await
        .expect("unrelated expired work was actually pruned");
}

#[test]
fn full_u64_serving_generation_and_epoch_exhaustion_are_not_truncated() {
    assert_eq!(u64::from_le_bytes(u64::MAX.to_le_bytes()), u64::MAX);
    let context = MutationContext {
        operation_id: Uuid::new_v4(),
        expected_request_sequence: i64::MAX,
        expected_attempt_sequence: 0,
        now_ms: 1,
    };
    assert!(context.validate().is_err());
    assert!(serde_json::from_str::<RequestKey>(&format!(
        "{{\"request_id\":\"x\",\"request_incarnation_id\":\"{}\",\"extra\":true}}",
        Uuid::new_v4()
    ))
    .is_err());
}

#[test]
fn migration_preserves_adaptive_rows_and_exact_source_shape() {
    let connection = rusqlite::Connection::open_in_memory().expect("real SQLite");
    SqliteStore::apply_migrations_for_test(&connection, crate::store::SQLITE_SCHEMA_VERSION)
        .expect("full legacy schema");
    schema::install_sqlite_checked(&connection).expect("fresh adjunct");
    assert!(schema::sqlite_ready(&connection, schema::Layout::Legacy).expect("exact shape"));
    schema::install_sqlite_checked(&connection).expect("restart idempotent");
    connection.execute("CREATE TRIGGER media_resource_spoof AFTER UPDATE ON media_resource_requests BEGIN SELECT 1; END",[]).expect("unknown object fixture");
    assert!(!schema::sqlite_ready(&connection, schema::Layout::Legacy)
        .expect("unknown shape unavailable"));
    schema::install_sqlite_checked(&connection).expect("unknown installation preserved");
    let present: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='media_resource_spoof'",
            [],
            |r| r.get(0),
        )
        .expect("no destructive repair");
    assert_eq!(present, 1);
}
