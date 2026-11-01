//! Exact outage authority exercised through three actual selected voter owners.
use super::*;
use plurx_core::cluster::membership::{ClusterOperationCleanupOutcome, MembershipManager};

fn first_page(node: &str) -> AttemptInventoryQuery {
    AttemptInventoryQuery {
        worker_node_id: node.to_owned(),
        after_attempt_id: None,
        limit: 1,
    }
}

#[tokio::test]
async fn three_voter_outage_blocks_new_claims_but_retains_exact_replay_and_other_workers() {
    with_adaptive_cluster(|cluster| {
        Box::pin(async move {
            let store = cluster.open_store().await;
            let (node, _, _) = cluster.selected_member(0).await;
            let (other_node, _, _) = cluster.selected_member(1).await;
            let manager = cluster.membership_manager(0).await;
            manager
                .heartbeat()
                .await
                .expect("original selected heartbeat");
            let mut original = AttemptFixture::new(&store).await;
            original.claim.key.worker_node_id = node.clone();
            let before = applied(
                store
                    .claim_adaptive_attempt(&original.claim)
                    .await
                    .expect("claim before original outage"),
            );
            let lease = MembershipManager::prepare_restart_preparation_claim(
                &node,
                Duration::from_secs(60),
            )
            .expect("prepare original opaque identity");
            manager
                .commit_cluster_operation_lease(&lease)
                .await
                .expect("actual committed planned-outage row");
            let standalone = SqliteStore::open_in_memory().expect("ordinary standalone SQLite");
            let standalone_attempt = AttemptFixture::new(&standalone).await;
            let standalone_before = applied(
                standalone
                    .claim_adaptive_attempt(&standalone_attempt.claim)
                    .await
                    .expect("standalone ordinary adaptive claim"),
            );
            assert!(matches!(
                standalone
                    .unclosed_adaptive_attempts_under_outage(&lease, &first_page(&node))
                    .await,
                Err(plurx_core::error::StoreError::Migration(message))
                    if message.contains("unsupported")
            ));
            assert_eq!(
                standalone
                    .claim_adaptive_attempt(&standalone_attempt.claim)
                    .await
                    .expect("unsupported fencing does not change ordinary SQLite replay"),
                AttemptMutation::Replay(standalone_before)
            );
            assert_eq!(
                store
                    .claim_adaptive_attempt(&original.claim)
                    .await
                    .expect("existing original operation remains replayable"),
                AttemptMutation::Replay(before.clone())
            );
            let mut blocked = AttemptFixture::new(&store).await;
            blocked.claim.key.worker_node_id = node.clone();
            assert!(store.claim_adaptive_attempt(&blocked.claim).await.is_err());
            assert_eq!(
                store
                    .adaptive_request_status(
                        &original.claim.binding.key,
                        &original.claim.binding.principal
                    )
                    .await
                    .expect("outage cannot close original debt"),
                Some(before)
            );
            let mut other = AttemptFixture::new(&store).await;
            other.claim.key.worker_node_id = other_node;
            applied(
                store
                    .claim_adaptive_attempt(&other.claim)
                    .await
                    .expect("distinct actual worker remains unfenced"),
            );
            let page = store
                .unclosed_adaptive_attempts_under_outage(&lease, &first_page(&node))
                .await
                .expect("exact original fenced inventory");
            assert_eq!(page.entries.len(), 1);
            assert_eq!(page.entries[0].attempt.key, original.claim.key);
            assert!(page.next_cursor.is_none());
            assert_eq!(
                manager
                    .release_cluster_operation_lease(&lease)
                    .await
                    .expect("exact release"),
                ClusterOperationCleanupOutcome::Confirmed
            );
            applied(
                store
                    .claim_adaptive_attempt(&blocked.claim)
                    .await
                    .expect("the original blocked operation can proceed after exact release"),
            );
        })
    })
    .await;
}

#[tokio::test]
async fn three_voter_outage_inventory_rejects_foreign_released_and_superseded_owners() {
    with_adaptive_cluster(|cluster| {
        Box::pin(async move {
            let store = cluster.open_store().await;
            let (node, _, _) = cluster.selected_member(0).await;
            let (foreign_node, _, _) = cluster.selected_member(1).await;
            let manager = cluster.membership_manager(0).await;
            manager
                .heartbeat()
                .await
                .expect("original selected heartbeat");
            let mut expected = Vec::new();
            for _ in 0..3 {
                let mut attempt = AttemptFixture::new(&store).await;
                attempt.claim.key.worker_node_id = node.clone();
                expected.push(attempt.claim.key.attempt_id);
                applied(
                    store
                        .claim_adaptive_attempt(&attempt.claim)
                        .await
                        .expect("original debt"),
                );
            }
            expected.sort();
            let lease = MembershipManager::prepare_restart_preparation_claim(
                &node,
                Duration::from_secs(60),
            )
            .expect("original opaque identity");
            assert!(
                store
                    .unclosed_adaptive_attempts_under_outage(&lease, &first_page(&node))
                    .await
                    .is_err(),
                "a merely prepared identity cannot authorize inventory"
            );
            manager
                .commit_cluster_operation_lease(&lease)
                .await
                .expect("original owner");
            assert!(
                store
                    .unclosed_adaptive_attempts_under_outage(&lease, &first_page(&foreign_node))
                    .await
                    .is_err(),
                "an actual different node cannot borrow this exact lease"
            );
            let mut query = first_page(&node);
            let mut found = Vec::new();
            loop {
                let page = store
                    .unclosed_adaptive_attempts_under_outage(&lease, &query)
                    .await
                    .expect("consistent page under original owner");
                found.extend(
                    page.entries
                        .iter()
                        .map(|entry| entry.attempt.key.attempt_id),
                );
                if let Some(after) = page.next_cursor {
                    query.after_attempt_id = Some(after);
                } else {
                    query.after_attempt_id = page
                        .entries
                        .last()
                        .map(|entry| entry.attempt.key.attempt_id);
                    break;
                }
            }
            assert_eq!(found, expected);
            assert!(store
                .unclosed_adaptive_attempts_under_outage(&lease, &query)
                .await
                .expect("tail after actual last entry")
                .entries
                .is_empty());
            assert!(
                !store
                    .unclosed_adaptive_attempts_under_outage(&lease, &first_page(&node))
                    .await
                    .expect("fresh first page still sees original debt")
                    .entries
                    .is_empty(),
                "an empty pagination tail never proves total zero"
            );
            assert_eq!(
                manager
                    .release_cluster_operation_lease(&lease)
                    .await
                    .expect("exact release"),
                ClusterOperationCleanupOutcome::Confirmed
            );
            assert!(store
                .unclosed_adaptive_attempts_under_outage(&lease, &query)
                .await
                .is_err());
            assert!(manager
                .commit_cluster_operation_lease(&lease)
                .await
                .is_err());
            let successor = MembershipManager::prepare_restart_preparation_claim(
                &node,
                Duration::from_secs(60),
            )
            .expect("new original identity for the same actual node");
            manager
                .commit_cluster_operation_lease(&successor)
                .await
                .expect("successor owner");
            assert!(store
                .unclosed_adaptive_attempts_under_outage(&lease, &query)
                .await
                .is_err());
            assert_eq!(
                store
                    .unclosed_adaptive_attempts_under_outage(&successor, &first_page(&node))
                    .await
                    .expect("fresh successor page")
                    .entries[0]
                    .attempt
                    .key
                    .attempt_id,
                expected[0]
            );
            assert_eq!(
                manager
                    .release_cluster_operation_lease(&successor)
                    .await
                    .expect("successor release"),
                ClusterOperationCleanupOutcome::Confirmed
            );
        })
    })
    .await;
}

#[tokio::test]
async fn three_voter_outage_expiry_never_turns_missing_authority_into_zero() {
    with_adaptive_cluster(|cluster| {
        Box::pin(async move {
            let store = cluster.open_store().await;
            let (node, _, _) = cluster.selected_member(0).await;
            let manager = cluster.membership_manager(0).await;
            manager
                .heartbeat()
                .await
                .expect("original selected heartbeat");
            let lease =
                MembershipManager::prepare_restart_preparation_claim(&node, Duration::from_secs(1))
                    .expect("short real lease with the owning grace period");
            manager
                .commit_cluster_operation_lease(&lease)
                .await
                .expect("actual owner");
            let page = store
                .unclosed_adaptive_attempts_under_outage(&lease, &first_page(&node))
                .await
                .expect("fresh owner with no admitted adaptive work");
            assert!(page.entries.is_empty());
            assert!(page.next_cursor.is_none());
            // Observe the opaque lease's actual deadline; never fabricate its
            // private expiry or a caller-supplied authority timestamp.
            tokio::time::timeout(Duration::from_secs(10), async {
                while lease
                    .preparation_expiry_unix_ms(Duration::from_secs(1))
                    .is_some()
                {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("actual lease expires within its original bounded window");
            assert!(
                store
                    .unclosed_adaptive_attempts_under_outage(&lease, &first_page(&node))
                    .await
                    .is_err(),
                "elapsed authority cannot return an apparently valid zero"
            );
            manager
                .heartbeat()
                .await
                .expect("owning durable expiry transition");
            assert!(
                store
                    .unclosed_adaptive_attempts_under_outage(&lease, &first_page(&node))
                    .await
                    .is_err(),
                "missing durable row remains unavailable after owning expiry"
            );
        })
    })
    .await;
}
