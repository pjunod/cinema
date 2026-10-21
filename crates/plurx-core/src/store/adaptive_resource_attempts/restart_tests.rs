//! Persisted restart, complete pagination and special-ingress parity fixtures.
use super::*;

async fn assert_reopened(store: &dyn Store, claim: &AttemptClaim, before: RequestStatus) {
    assert_eq!(
        store
            .adaptive_request_status(&claim.binding.key, &claim.binding.principal)
            .await
            .expect("new authority reader"),
        Some(before)
    );
    assert!(matches!(
        store
            .claim_adaptive_attempt(claim)
            .await
            .expect("same op after reconnect"),
        AttemptMutation::Replay(_)
    ));
    assert!(
        matches!(store.claim_media_session_request(&claim.binding.principal,&claim.binding.key.request_id,&claim.binding.request_fingerprint,&claim.binding.playback_id,&Uuid::new_v4().to_string(),30_000,40_000).await.expect("expired duplicate remains in flight"),MediaSessionRequestClaim::InFlight{incarnation_id,..} if incarnation_id==claim.binding.key.request_incarnation_id.to_string())
    );
    let mut changed = claim.clone();
    changed.context.operation_id = Uuid::new_v4();
    changed.binding.original_deadline_ms = 40_000;
    assert_eq!(
        store
            .claim_adaptive_attempt(&changed)
            .await
            .expect("cannot restart deadline"),
        AttemptMutation::Refused(AttemptRefusal::Conflict)
    );
}

#[tokio::test]
async fn sqlite_store_reopen_retains_attempt_worker_and_original_deadline() {
    let directory = tempfile::tempdir().expect("task-owned TMPDIR fixture");
    let path = directory.path().join("adaptive-restart.db");
    let store = SqliteStore::open(&path).expect("file backend");
    let fixture = AttemptFixture::new(&store).await;
    let before = applied(
        store
            .claim_adaptive_attempt(&fixture.claim)
            .await
            .expect("real commit"),
    );
    drop(store);
    let reopened = SqliteStore::open(&path).expect("reopen persisted source");
    assert_reopened(&reopened, &fixture.claim, before).await;
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test]
async fn replicated_store_reconnect_retains_attempt_worker_and_original_deadline() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .validation_reset_contract_state()
        .await
        .expect("exclusive reset");
    let fixture = AttemptFixture::new(&store).await;
    let before = applied(
        store
            .claim_adaptive_attempt(&fixture.claim)
            .await
            .expect("real replicated commit"),
    );
    drop(store);
    let reopened = open_contract_hiqlite_store(&cluster).await;
    assert_reopened(&reopened, &fixture.claim, before).await;
}

#[tokio::test]
async fn unclosed_inventory_keyset_pages_never_hide_expired_or_losing_debt() {
    for_each_backend(|store, _| async move {
        let worker = Uuid::new_v4().to_string();
        let mut expected = Vec::new();
        for _ in 0..3 {
            let mut fixture = AttemptFixture::new(store.as_ref()).await;
            fixture.claim.key.worker_node_id = worker.clone();
            expected.push(fixture.claim.key.attempt_id);
            applied(
                store
                    .claim_adaptive_attempt(&fixture.claim)
                    .await
                    .expect("separate request"),
            );
        }
        expected.sort();
        let mut after = None;
        let mut found = Vec::new();
        loop {
            let page = store
                .unclosed_adaptive_attempts(&AttemptInventoryQuery {
                    worker_node_id: worker.clone(),
                    after_attempt_id: after,
                    limit: 1,
                })
                .await
                .expect("complete page");
            found.extend(
                page.entries
                    .iter()
                    .map(|entry| entry.attempt.key.attempt_id),
            );
            after = page.next_cursor;
            if after.is_none() {
                break;
            }
        }
        assert_eq!(found, expected);
    })
    .await;
}

#[tokio::test]
async fn channel_recipe_adoption_uses_frozen_original_root_binding() {
    for_each_backend(|store, _| async move {
        let fixture = AttemptFixture::new(store.as_ref()).await;
        let recipe = "{\"library_channel\":{\"fixture\":true}}";
        let adoption = ready_adoption_recipe(store.as_ref(), &fixture, recipe).await;
        assert!(
            store.adopt_adaptive_winner(&adoption).await.is_err(),
            "an unrecorded selected channel recipe is not authority"
        );
        assert!(store
            .record_library_channel_session_recipe(
                &fixture.claim.binding.principal,
                &fixture.claim.binding.key.request_id,
                &fixture.claim.binding.key.request_incarnation_id.to_string(),
                recipe,
                10_003
            )
            .await
            .expect("record against ORIGINAL root"));
        assert!(matches!(
            store
                .adopt_adaptive_winner(&adoption)
                .await
                .expect("frozen recipe adoption"),
            AttemptAdoption::Applied(_)
        ));
    })
    .await;
}

#[tokio::test]
async fn unsupported_sharing_principal_never_coerces_into_local_adaptive_authority() {
    for_each_backend(|store, _| async move {
        let fixture = AttemptFixture::new(store.as_ref()).await;
        let mut shared = fixture.claim.clone();
        shared.binding.principal = plurx_core::playback_principal::PlaybackPrincipal::sharing(
            Uuid::new_v4(),
            &"f".repeat(64),
        )
        .expect("typed sharing principal");
        assert_eq!(
            store
                .claim_adaptive_attempt(&shared)
                .await
                .expect("explicit refusal"),
            AttemptMutation::Refused(AttemptRefusal::UnsupportedPrincipal)
        );
        assert!(store
            .adaptive_request_status(&fixture.claim.binding.key, &fixture.claim.binding.principal)
            .await
            .expect("local root unaffected")
            .is_none());
    })
    .await;
}
