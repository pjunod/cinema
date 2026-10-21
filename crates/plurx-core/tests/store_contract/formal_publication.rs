//! Formal-verification pilot — M2 backend conformance bridge for the chosen
//! bound library-scan insertion (`insert_item_fenced`).
//!
//! Every test here is an **executable example** against the real backends the
//! Store contract harness provides: in-memory SQLite, file-backed SQLite and,
//! with `hiqlite-contract-tests`, a real three-voter Hiqlite cluster labelled
//! `hiqlite-3-voter`. The examples observe return values *and* retained
//! database state. None of them is a refinement proof; they pin the backend
//! behaviour that the durable-jobs model
//! (`verification/durable-jobs/`) abstracts.
//!
//! Retained prior evidence (names kept, not duplicated here):
//! - `background_jobs_replayed_claim_never_advances_fence_or_changes_boot`
//!   and `background_jobs_cancellation_wins_renewal_and_rejects_new_interest`
//!   in `crates/plurx-core/src/store/background_jobs_tests.rs` (SQLite unit
//!   tests; `cargo test -p plurx-core background_jobs_`).
//! - `background_jobs_global_history_pressure_preserves_resolution_and_restores_progress`
//!   in the same module covers attempt retention and pruning bounds.
//! - `background_library_publication_requires_both_live_owners` in
//!   `tests/store_contract/background_jobs.rs` exercises `put_setting_fenced`,
//!   not the chosen insertion; it remains the dual-owner reference.
//!
//! Executable command for this module (same feature as the rest of the
//! contract suite; the replicated cases require the three-voter harness):
//! `cargo test --locked -p plurx-core --features hiqlite-contract-tests
//!  --test store_contract formal_publication -- --nocapture`.
//!
//! Mapping classification used in the comments below:
//! - *example*: observed here against both backends;
//! - *replicated example*: observed only on `hiqlite-3-voter` through the
//!   feature-gated insertion seam (`plurx_core::store::publication_conformance`);
//! - *reviewed mapping*: a source fact stated for the protocol author's
//!   transition map, not re-observed here;
//! - *assumption*: a compatibility premise this module pins where it can.

use super::{acquired, for_each_backend, publication_successor};
use plurx_core::domain::{ItemKind, ItemSort, LibraryKind, NewItem, NewLibrary};
use plurx_core::error::StoreError;
use plurx_core::store::background_jobs::*;
use plurx_core::store::background_jobs_library::{
    LibraryTrigger, LibraryWorkInput, LibraryWorkQuery, LibraryWorkResult, NewLibraryWork,
};
use plurx_core::store::background_jobs_resources::StorageDomainMapping;
use plurx_core::store::Store;
use std::path::PathBuf;
use std::sync::Arc;

/// Queue lease length from `background_jobs.rs` (`JOB_LEASE_MS`), pinned so
/// an expiry example cannot silently drift with the constant.
const JOB_LEASE_MS: i64 = 30_000;
/// Claim resolution window from `background_jobs.rs`
/// (`CLAIM_RESOLUTION_WINDOW_MS`).
const CLAIM_RESOLUTION_WINDOW_MS: i64 = 120_000;
/// Domain lease TTL the daemon's job runner uses; the publication successor
/// also extends by this much (`coordination.rs`, `PUBLICATION_TTL_MS`).
const DOMAIN_TTL_MS: i64 = 90_000;

fn unix_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
}

fn movie(library_id: i64, title: &str) -> NewItem {
    NewItem {
        library_id,
        kind: ItemKind::Movie,
        parent_id: None,
        title: title.to_owned(),
        year: Some(2024),
        season_number: None,
        episode_number: None,
    }
}

/// Retained catalogue state for one library, in title order.
async fn titles(store: &dyn Store, library_id: i64) -> Vec<String> {
    store
        .list_top_items(library_id, ItemSort::Title, 0, 100)
        .await
        .expect("list library items")
        .items
        .into_iter()
        .map(|item| item.title)
        .collect()
}

async fn request_record(
    store: &dyn Store,
    request_id: &str,
) -> plurx_core::store::background_jobs_library::LibraryWorkRecord {
    store
        .library_work_requests(LibraryWorkQuery {
            request_id: Some(request_id.to_owned()),
            limit: 1,
            ..Default::default()
        })
        .await
        .expect("read library work request")
        .into_iter()
        .next()
        .expect("library work request exists")
}

fn is_fence_rejected<T: std::fmt::Debug>(result: &Result<T, StoreError>) -> bool {
    matches!(result, Err(StoreError::FenceRejected { .. }))
}

/// One claimed library-scan attempt and the domain lease it may bind to.
struct Scan {
    job_id: String,
    token: JobToken,
    lease: plurx_core::cluster::coordination::Lease,
    /// Supplied queue time; the queue consumes caller `now_ms`, never a clock.
    claim_time: i64,
}

impl Scan {
    fn resource(library_id: i64) -> String {
        format!("scan:library:{library_id}")
    }

    fn binding(&self) -> BindLibraryJob {
        BindLibraryJob {
            token: self.token.clone(),
            lease: self.lease.clone(),
            now_ms: self.claim_time,
        }
    }

    /// Release the attempt's reservations so later scenarios can claim the
    /// two provider slots. Retirement never rewrites or removes a binding.
    async fn retire(&self, store: &dyn Store, now: i64) {
        store
            .cancel_job(CancelJob {
                job_id: self.job_id.clone(),
                now_ms: now + CLAIM_RESOLUTION_WINDOW_MS,
            })
            .await
            .expect("retire scenario owner");
        store
            .maintain_jobs(now + CLAIM_RESOLUTION_WINDOW_MS)
            .await
            .expect("upkeep");
    }
}

async fn create_library(store: &dyn Store, prefix: &str) -> i64 {
    store
        .create_library(&NewLibrary {
            name: format!("{prefix} library"),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from(format!("/{prefix}"))],
            anime: false,
        })
        .await
        .expect("library")
        .id
}

/// Enqueue and claim a `library_scan` attempt for `node` at the supplied
/// queue time, then acquire the library's domain lease at wall-clock `now`.
/// Binding is left to the caller so unbound states stay representable.
async fn claim_scan(
    store: &dyn Store,
    backend: &str,
    library_id: i64,
    node: &str,
    claim_time: i64,
    now: i64,
) -> Scan {
    let job_id = uuid::Uuid::new_v4().to_string();
    let admitted = store
        .enqueue_job(EnqueueJob {
            id: job_id.clone(),
            payload: JobPayload::LibraryScan {
                library_id,
                generation: "formal-v1".into(),
            },
            dedupe_key: format!("scan:{library_id}"),
            priority: 2,
            not_before_ms: claim_time,
            now_ms: claim_time,
            request: JobRequest {
                scope: "formal-conformance".into(),
                request_id: job_id.clone(),
                request_digest: "f".repeat(64),
                consumer_kind: "library_scan".into(),
                consumer_ref: library_id.to_string(),
                target_node_id: None,
                deadline_ms: None,
                retain_identity: false,
            },
        })
        .await
        .expect("admission");
    assert!(
        matches!(admitted, EnqueueOutcome::Accepted { .. }),
        "{backend}: {admitted:?}"
    );
    let ClaimOutcome::Claimed { job } = store
        .claim_job(ClaimJob {
            job_id: job_id.clone(),
            expected_revision: 0,
            node_id: node.into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind: JobKind::LibraryScan,
            payload_version: 1,
            now_ms: claim_time,
            dispatched_at_ms: claim_time,
        })
        .await
        .expect("claim")
    else {
        panic!("{backend}: claim refused");
    };
    let token = job.token.expect("token");
    assert_eq!(
        token.lease_expires_ms,
        claim_time + JOB_LEASE_MS,
        "{backend}: queue lease length is the supplied time plus JOB_LEASE_MS"
    );
    let lease = acquired(
        store
            .acquire_lease(&Scan::resource(library_id), node, now, now + DOMAIN_TTL_MS)
            .await
            .expect("domain lease"),
        backend,
    );
    Scan {
        job_id,
        token,
        lease,
        claim_time,
    }
}

struct UnboundLibraryWork {
    library_id: i64,
    job_id: String,
    token: JobToken,
    claim_time: i64,
}

async fn enqueue_and_claim_library_work(
    store: &dyn Store,
    backend: &str,
    library_id: i64,
    node: &str,
    request_id: &str,
    refresh: bool,
    now: i64,
) -> UnboundLibraryWork {
    let outcome = store
        .enqueue_library_work(NewLibraryWork {
            request_id: request_id.to_owned(),
            library_id,
            input: LibraryWorkInput::Full {
                refresh,
                trigger: LibraryTrigger::Manual,
            },
            now_ms: now,
        })
        .await
        .expect("enqueue real library request");
    assert!(
        matches!(outcome, EnqueueOutcome::Accepted { .. }),
        "{backend}: {outcome:?}"
    );
    let request = store
        .library_work_requests(LibraryWorkQuery {
            request_id: Some(request_id.to_owned()),
            limit: 1,
            ..Default::default()
        })
        .await
        .expect("read admitted request")
        .into_iter()
        .next()
        .expect("request admitted");
    let kind = if refresh {
        JobKind::MetadataRefresh
    } else {
        JobKind::LibraryScan
    };
    let outcome = store
        .claim_job(ClaimJob {
            job_id: request.job_id.clone(),
            expected_revision: 0,
            node_id: node.to_owned(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind,
            payload_version: 1,
            now_ms: now,
            dispatched_at_ms: now,
        })
        .await
        .expect("claim admitted library job");
    let job = match outcome {
        ClaimOutcome::Claimed { job } => job,
        refused => panic!("{backend}: library claim refused: {refused:?}"),
    };
    UnboundLibraryWork {
        library_id,
        job_id: request.job_id,
        token: job.token.expect("claimed job token"),
        claim_time: now,
    }
}

async fn bind_library_work(
    store: &dyn Store,
    backend: &str,
    work: UnboundLibraryWork,
    node: &str,
    now: i64,
) -> Scan {
    let lease = acquired(
        store
            .acquire_lease(
                &Scan::resource(work.library_id),
                node,
                now,
                now + DOMAIN_TTL_MS,
            )
            .await
            .expect("acquire real library domain lease"),
        backend,
    );
    let scan = Scan {
        job_id: work.job_id,
        token: work.token,
        lease,
        claim_time: work.claim_time,
    };
    assert!(
        store
            .bind_library_job(scan.binding())
            .await
            .expect("bind library job"),
        "{backend}: bind real library worker"
    );
    scan
}

/// `claim_scan` followed by a successful bind of both authorities.
async fn bound_scan(
    store: &dyn Store,
    backend: &str,
    library_id: i64,
    claim_time: i64,
    now: i64,
) -> Scan {
    let scan = claim_scan(store, backend, library_id, "node-a", claim_time, now).await;
    assert!(
        store.bind_library_job(scan.binding()).await.expect("bind"),
        "{backend}: bind both live authorities"
    );
    scan
}

/// §8.2 example 1: live bound insertion commits; cancellation or takeover
/// serialized before the publication transaction rejects it with nothing
/// retained; cancellation serialized after the commit leaves the item.
#[tokio::test]
async fn formal_publication_live_bound_insertion_commits_and_cancellation_orders_around_commit() {
    for_each_backend(|store, backend| async move {
        for scenario in ["live", "cancel_before", "takeover_before", "cancel_after"] {
            let now = unix_now_ms();
            let library_id = create_library(store.as_ref(), &format!("formal-{scenario}")).await;
            let scan = bound_scan(store.as_ref(), backend, library_id, now, now).await;
            let item = movie(library_id, &format!("{scenario} movie"));
            let successor = publication_successor(&scan.lease);
            match scenario {
                "cancel_before" => {
                    store
                        .cancel_job(CancelJob {
                            job_id: scan.job_id.clone(),
                            now_ms: now + 1,
                        })
                        .await
                        .expect("cancel");
                }
                "takeover_before" => {
                    // Takeover eligibility is "old expiry <= supplied now";
                    // the new owner's claim advances the stable job fence the
                    // binding names, so the old binding is obsolete.
                    let takeover = store
                        .claim_job(ClaimJob {
                            job_id: scan.job_id.clone(),
                            expected_revision: scan.token.revision,
                            node_id: "node-b".into(),
                            boot_id: uuid::Uuid::new_v4().to_string(),
                            claim_id: uuid::Uuid::new_v4().to_string(),
                            kind: JobKind::LibraryScan,
                            payload_version: 1,
                            now_ms: scan.token.lease_expires_ms + 1,
                            dispatched_at_ms: scan.token.lease_expires_ms + 1,
                        })
                        .await
                        .expect("takeover");
                    assert!(
                        matches!(takeover, ClaimOutcome::Claimed { .. }),
                        "{backend}: {takeover:?}"
                    );
                }
                _ => {}
            }
            let result = store
                .insert_item_fenced(&item, &scan.lease, &successor)
                .await;
            match scenario {
                "live" | "cancel_after" => {
                    let id = result.unwrap_or_else(|error| panic!("{backend}: {scenario}: {error}"));
                    assert_eq!(
                        store
                            .get_item(id)
                            .await
                            .expect("read")
                            .expect("inserted item")
                            .title,
                        item.title,
                        "{backend}: {scenario}"
                    );
                    // The backend installed the successor token in the same
                    // transaction: it is the only live predecessor now.
                    let stale = store
                        .insert_item_fenced(&item, &scan.lease, &successor)
                        .await;
                    assert!(is_fence_rejected(&stale), "{backend}: {scenario}: {stale:?}");
                    if scenario == "cancel_after" {
                        store
                            .cancel_job(CancelJob {
                                job_id: scan.job_id.clone(),
                                now_ms: now + 1,
                            })
                            .await
                            .expect("cancel after commit");
                        let later = store
                            .insert_item_fenced(
                                &movie(library_id, "after cancellation"),
                                &successor,
                                &publication_successor(&successor),
                            )
                            .await;
                        assert!(is_fence_rejected(&later), "{backend}: {later:?}");
                    } else {
                        store
                            .insert_item_fenced(
                                &movie(library_id, "second live"),
                                &successor,
                                &publication_successor(&successor),
                            )
                            .await
                            .unwrap_or_else(|error| panic!("{backend}: successor live: {error}"));
                    }
                    let expected: Vec<String> = if scenario == "live" {
                        vec![item.title.clone(), "second live".into()]
                    } else {
                        vec![item.title.clone()]
                    };
                    assert_eq!(
                        titles(store.as_ref(), library_id).await,
                        expected,
                        "{backend}: {scenario}: cancellation after commit never erases it"
                    );
                }
                _ => {
                    assert!(is_fence_rejected(&result), "{backend}: {scenario}: {result:?}");
                    assert!(
                        titles(store.as_ref(), library_id).await.is_empty(),
                        "{backend}: {scenario}: rejected transaction retains no item"
                    );
                    // No partial authority transition: the exact predecessor
                    // domain token is still the current one, so the domain
                    // alone can still renew it.
                    assert!(
                        store
                            .renew_lease(&scan.lease, now + 2, now + 2 + DOMAIN_TTL_MS)
                            .await
                            .expect("domain renewal")
                            .is_some(),
                        "{backend}: {scenario}: rejected publication did not advance the domain token"
                    );
                }
            }
            scan.retire(store.as_ref(), now).await;
        }
    })
    .await;
}

/// §8.2 example 2: bind, then renew the queue attempt and the domain lease
/// independently. The binding names stable fences and attempt identity, so
/// revision churn on either side keeps the association; the exact previous
/// domain token is nevertheless stale for publication.
#[tokio::test]
async fn formal_publication_binding_survives_independent_renewals_and_rejects_exact_stale_tokens() {
    for_each_backend(|store, backend| async move {
        let now = unix_now_ms();
        let library_id = create_library(store.as_ref(), "formal-renewal").await;
        let scan = bound_scan(store.as_ref(), backend, library_id, now, now).await;

        let renewed_queue = store
            .renew_jobs(RenewJobs {
                tokens: vec![scan.token.clone()],
                now_ms: now + 1_000,
            })
            .await
            .expect("queue renewal");
        let [RenewOutcome::Renewed { token: queue_token }] = renewed_queue.as_slice() else {
            panic!("{backend}: {renewed_queue:?}");
        };
        assert_eq!(
            queue_token.fence, scan.token.fence,
            "{backend}: stable job fence"
        );
        assert_eq!(
            queue_token.claim_id, scan.token.claim_id,
            "{backend}: same attempt"
        );
        assert!(queue_token.revision > scan.token.revision, "{backend}");
        assert_eq!(
            queue_token.lease_expires_ms,
            now + 1_000 + JOB_LEASE_MS,
            "{backend}"
        );

        let domain_token = store
            .renew_lease(&scan.lease, now + 2_000, now + 2_000 + DOMAIN_TTL_MS)
            .await
            .expect("domain renewal")
            .unwrap_or_else(|| panic!("{backend}: domain renewal refused"));
        assert_eq!(
            domain_token.fence, scan.lease.fence,
            "{backend}: stable domain fence"
        );
        assert_eq!(domain_token.revision, scan.lease.revision + 1, "{backend}");

        // Legitimate continuity: neither renewal touched the binding.
        let domain_successor = publication_successor(&domain_token);
        let published = store
            .insert_item_fenced(
                &movie(library_id, "after both renewals"),
                &domain_token,
                &domain_successor,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: renewed owners publish: {error}"));
        assert!(published > 0, "{backend}");

        // Exact stale tokens: the pre-renewal domain token and the
        // just-consumed domain token are both rejected, with nothing added.
        for (label, stale) in [("pre-renewal", &scan.lease), ("consumed", &domain_token)] {
            let result = store
                .insert_item_fenced(
                    &movie(library_id, &format!("{label} stale")),
                    stale,
                    &publication_successor(stale),
                )
                .await;
            assert!(is_fence_rejected(&result), "{backend}: {label}: {result:?}");
        }
        assert_eq!(
            titles(store.as_ref(), library_id).await,
            vec!["after both renewals".to_owned()],
            "{backend}"
        );

        // The binding still names the original attempt and domain fence:
        // replaying the bind with the current exact tokens is the identical
        // stable tuple, which the conflict clause accepts.
        assert!(
            store
                .bind_library_job(BindLibraryJob {
                    token: queue_token.clone(),
                    lease: domain_successor.clone(),
                    now_ms: now + 2_000,
                })
                .await
                .expect("bind replay"),
            "{backend}: same stable tuple after both renewals"
        );
        scan.retire(store.as_ref(), now).await;
    })
    .await;
}

/// §8.2 example 3: queue expiry with a still-live domain lease rejects at
/// the backend's sampled time. The expiry is seeded through the queue's
/// caller-supplied `now_ms`, so no real elapsed time is claimed; the backend
/// sample is the real wall clock (SQLite inside its transaction, Hiqlite
/// before submission), which is after the seeded queue expiry.
#[tokio::test]
async fn formal_publication_queue_expiry_with_live_domain_rejects_at_backend_sample() {
    for_each_backend(|store, backend| async move {
        let now = unix_now_ms();
        let library_id = create_library(store.as_ref(), "formal-expiry").await;
        // Claimed JOB_LEASE_MS + 1 s "ago": the queue lease expired one
        // second before any backend sample this test can take.
        let claim_time = now - JOB_LEASE_MS - 1_000;
        let scan = bound_scan(store.as_ref(), backend, library_id, claim_time, now).await;
        assert!(
            scan.token.lease_expires_ms < now,
            "{backend}: seeded expiry precedes now"
        );

        let result = store
            .insert_item_fenced(
                &movie(library_id, "expired queue"),
                &scan.lease,
                &publication_successor(&scan.lease),
            )
            .await;
        assert!(is_fence_rejected(&result), "{backend}: {result:?}");
        assert!(
            titles(store.as_ref(), library_id).await.is_empty(),
            "{backend}"
        );

        // Separate the claims: the domain lease itself is live and renewable;
        // only the bound queue attempt is expired at the sample.
        let renewed = store
            .renew_lease(&scan.lease, now + 1, now + 1 + DOMAIN_TTL_MS)
            .await
            .expect("domain renewal");
        assert!(renewed.is_some(), "{backend}: domain lease remained live");
        let queue = store
            .renew_jobs(RenewJobs {
                tokens: vec![scan.token.clone()],
                now_ms: now,
            })
            .await
            .expect("queue renewal verdict");
        assert!(
            matches!(queue.as_slice(), [RenewOutcome::LostOwnership { .. }]),
            "{backend}: {queue:?}"
        );
        scan.retire(store.as_ref(), now).await;
    })
    .await;
}

/// §8.2 example 5: required-resource coverage, owner removal, obsolete and
/// missing bindings follow their actual code semantics. In particular a
/// domain lease with no matching binding publishes domain-only; the bridge
/// must not assert that a binding always exists.
#[tokio::test]
async fn formal_publication_reservations_owner_removal_and_binding_lifetime_follow_code_semantics()
{
    for_each_backend(|store, backend| async move {
        // Required resource no longer covered by the held reservation. The
        // reservation row is not revoked; the job's requirement changes
        // (library roots move to an unmapped storage domain) and the held
        // set no longer covers it, which both guards treat as lost.
        {
            let now = unix_now_ms();
            let library_id = create_library(store.as_ref(), "formal-coverage").await;
            let library = store
                .get_library(library_id)
                .await
                .expect("library")
                .expect("library");
            assert!(store
                .replace_storage_domains(
                    vec![StorageDomainMapping {
                        library_id,
                        root_path: library.paths[0].to_string_lossy().into_owned(),
                        domain_id: "formal-nas".into(),
                    }],
                    now,
                )
                .await
                .expect("map storage domain"));
            let scan = bound_scan(store.as_ref(), backend, library_id, now, now).await;
            store
                .update_library(
                    library_id,
                    &NewLibrary {
                        name: library.name.clone(),
                        kind: library.kind,
                        paths: vec![PathBuf::from("/formal-unmapped-storage")],
                        anime: library.anime,
                    },
                )
                .await
                .expect("move library roots");
            let result = store
                .insert_item_fenced(
                    &movie(library_id, "uncovered resource"),
                    &scan.lease,
                    &publication_successor(&scan.lease),
                )
                .await;
            assert!(
                is_fence_rejected(&result),
                "{backend}: coverage: {result:?}"
            );
            assert!(
                titles(store.as_ref(), library_id).await.is_empty(),
                "{backend}"
            );
            scan.retire(store.as_ref(), now).await;
        }

        // Owner removal: the removed-owner marker is a settings row the
        // guards consult by key; bind and publication both refuse it.
        {
            let now = unix_now_ms();
            let library_id = create_library(store.as_ref(), "formal-removed").await;
            let scan = claim_scan(
                store.as_ref(),
                backend,
                library_id,
                "node-removed",
                now,
                now,
            )
            .await;
            assert!(
                store.bind_library_job(scan.binding()).await.expect("bind"),
                "{backend}"
            );
            store
                .put_setting("internal.cluster_job_owner_removed.node-removed", "1")
                .await
                .expect("mark owner removed");
            assert!(
                !store
                    .bind_library_job(scan.binding())
                    .await
                    .expect("bind replay"),
                "{backend}: a removed owner cannot rebind"
            );
            let result = store
                .insert_item_fenced(
                    &movie(library_id, "removed owner"),
                    &scan.lease,
                    &publication_successor(&scan.lease),
                )
                .await;
            assert!(is_fence_rejected(&result), "{backend}: removed: {result:?}");
            assert!(
                titles(store.as_ref(), library_id).await.is_empty(),
                "{backend}"
            );
            scan.retire(store.as_ref(), now).await;
        }

        // Missing binding: a domain lease that never bound publishes
        // domain-only. This is the actual guard semantics, not a defect the
        // bridge may paper over; the model must represent the unbound state.
        {
            let now = unix_now_ms();
            let library_id = create_library(store.as_ref(), "formal-unbound").await;
            let lease = acquired(
                store
                    .acquire_lease(
                        &Scan::resource(library_id),
                        "node-a",
                        now,
                        now + DOMAIN_TTL_MS,
                    )
                    .await
                    .expect("domain lease"),
                backend,
            );
            store
                .insert_item_fenced(
                    &movie(library_id, "domain only"),
                    &lease,
                    &publication_successor(&lease),
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: domain-only publication: {error}"));
            assert_eq!(
                titles(store.as_ref(), library_id).await,
                vec!["domain only".to_owned()],
                "{backend}"
            );
        }

        // Obsolete binding: release expires the domain lease and advances its
        // revision without deleting the row, so the binding survives and
        // names the old fence. A new fence has no matching binding and is
        // domain-only until the attempt binds again; the released exact
        // token stays stale.
        {
            let now = unix_now_ms();
            let library_id = create_library(store.as_ref(), "formal-obsolete").await;
            let scan = bound_scan(store.as_ref(), backend, library_id, now, now).await;
            assert!(
                store
                    .release_lease(&scan.lease, now + 10)
                    .await
                    .expect("release"),
                "{backend}"
            );
            let second = acquired(
                store
                    .acquire_lease(
                        &Scan::resource(library_id),
                        "node-a",
                        now + 10,
                        now + 10 + DOMAIN_TTL_MS,
                    )
                    .await
                    .expect("re-acquire"),
                backend,
            );
            assert!(
                second.fence > scan.lease.fence,
                "{backend}: new domain fence"
            );
            let released = store
                .insert_item_fenced(
                    &movie(library_id, "released token"),
                    &scan.lease,
                    &publication_successor(&scan.lease),
                )
                .await;
            assert!(is_fence_rejected(&released), "{backend}: {released:?}");
            let second_successor = publication_successor(&second);
            store
                .insert_item_fenced(
                    &movie(library_id, "new fence unbound"),
                    &second,
                    &second_successor,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: unbound new fence: {error}"));
            // Rebinding the same running attempt to the higher fence is the
            // conflict branch `domain_fence < excluded.domain_fence`.
            assert!(
                store
                    .bind_library_job(BindLibraryJob {
                        token: scan.token.clone(),
                        lease: second_successor.clone(),
                        now_ms: now + 10,
                    })
                    .await
                    .expect("rebind"),
                "{backend}: rebind to the new domain fence"
            );
            store
                .cancel_job(CancelJob {
                    job_id: scan.job_id.clone(),
                    now_ms: now + 11,
                })
                .await
                .expect("cancel");
            let bound_again = store
                .insert_item_fenced(
                    &movie(library_id, "bound then cancelled"),
                    &second_successor,
                    &publication_successor(&second_successor),
                )
                .await;
            assert!(
                is_fence_rejected(&bound_again),
                "{backend}: {bound_again:?}"
            );
            assert_eq!(
                titles(store.as_ref(), library_id).await,
                vec!["new fence unbound".to_owned()],
                "{backend}"
            );
            scan.retire(store.as_ref(), now).await;
        }
    })
    .await;
}

/// §8.2 example 4 on the contract harness: a lost initial claim reply is
/// resolved to the identical attempt token, which then binds and publishes;
/// the resolution window and a reboot identity do not restore stale rights.
/// The queue-only regressions named in the module docs remain the primary
/// evidence; this is the selected trace through to the insertion.
#[tokio::test]
async fn formal_publication_resolved_claim_keeps_attempt_identity_through_insertion() {
    for_each_backend(|store, backend| async move {
        let now = unix_now_ms();
        let library_id = create_library(store.as_ref(), "formal-resolve").await;
        let scan = claim_scan(store.as_ref(), backend, library_id, "node-a", now, now).await;
        // The worker lost the claim reply: it only knows the identities it
        // sent. Resolution without a fence is bounded by the dispatch window.
        let request = ResolveClaim {
            job_id: scan.job_id.clone(),
            node_id: scan.token.node_id.clone(),
            boot_id: scan.token.boot_id.clone(),
            claim_id: scan.token.claim_id.clone(),
            fence: None,
            dispatched_at_ms: now,
            now_ms: now + 1,
        };
        let ClaimResolution::Running { token } = store
            .resolve_claim(request.clone())
            .await
            .expect("resolve lost reply")
        else {
            panic!("{backend}: lost reply did not resolve to the running attempt");
        };
        assert_eq!(
            token, scan.token,
            "{backend}: resolution returns the exact attempt token"
        );

        let closed = store
            .resolve_claim(ResolveClaim {
                now_ms: now + CLAIM_RESOLUTION_WINDOW_MS,
                ..request.clone()
            })
            .await
            .expect("window verdict");
        assert!(
            matches!(closed, ClaimResolution::ExpiredOrPruned),
            "{backend}: {closed:?}"
        );
        let rebooted = store
            .claim_job(ClaimJob {
                job_id: scan.job_id.clone(),
                expected_revision: 0,
                node_id: scan.token.node_id.clone(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: scan.token.claim_id.clone(),
                kind: JobKind::LibraryScan,
                payload_version: 1,
                now_ms: now + 2,
                dispatched_at_ms: now,
            })
            .await
            .expect("reboot replay");
        assert!(
            matches!(rebooted, ClaimOutcome::ExpiredOrPruned),
            "{backend}: {rebooted:?}"
        );

        assert!(
            store
                .bind_library_job(BindLibraryJob {
                    token: token.clone(),
                    lease: scan.lease.clone(),
                    now_ms: now + 1,
                })
                .await
                .expect("bind resolved token"),
            "{backend}"
        );
        store
            .insert_item_fenced(
                &movie(library_id, "resolved attempt"),
                &scan.lease,
                &publication_successor(&scan.lease),
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: resolved attempt publishes: {error}"));
        assert_eq!(
            titles(store.as_ref(), library_id).await,
            vec!["resolved attempt".to_owned()],
            "{backend}"
        );
        scan.retire(store.as_ref(), now).await;
    })
    .await;
}

/// S1: a captured completion token remains usable while both durable owners
/// remain current.
#[tokio::test]
async fn formal_publication_library_captured_token_completes_while_durable_owners_current() {
    for_each_backend(|store, backend| async move {
        let now = unix_now_ms();
        let library_id = create_library(store.as_ref(), "formal-late-completion").await;
        let work = enqueue_and_claim_library_work(
            store.as_ref(),
            backend,
            library_id,
            "node-a",
            "request-late",
            false,
            now,
        )
        .await;
        let scan = bind_library_work(store.as_ref(), backend, work, "node-a", now).await;
        // This direct Store call isolates the positive durable-authority
        // predicate. It does not schedule local revocation or an in-flight
        // production facade write.
        assert!(
            store
                .complete_library_work(CompleteLibraryWork {
                    token: scan.token.clone(),
                    lease: scan.lease.clone(),
                    request_id: "request-late".into(),
                    result: LibraryWorkResult::Failed {
                        error: "scheduled result".into()
                    },
                    now_ms: now + 1,
                })
                .await
                .expect("detached completion"),
            "{backend}: both durable owners still apply"
        );
        let record = store
            .library_work_requests(LibraryWorkQuery {
                request_id: Some("request-late".into()),
                limit: 1,
                ..Default::default()
            })
            .await
            .expect("read result")
            .remove(0);
        assert_eq!(record.state, "failed", "{backend}");
        assert!(
            matches!(
                record.result,
                Some(LibraryWorkResult::Failed { ref error })
                    if error == "scheduled result"
            ),
            "{backend}: stored result matches the submitted failure"
        );
        scan.retire(store.as_ref(), now).await;
    })
    .await;
}

/// S2/S3: release or a new worker's higher-fence binding invalidates an old
/// captured completion token.
#[tokio::test]
async fn formal_publication_library_completion_after_release_and_rebind_is_rejected() {
    for_each_backend(|store, backend| async move {
        let now = unix_now_ms();
        let library_id = create_library(store.as_ref(), "formal-late-owner").await;
        let first_work = enqueue_and_claim_library_work(
            store.as_ref(),
            backend,
            library_id,
            "node-a",
            "request-old",
            false,
            now,
        )
        .await;
        let first = bind_library_work(store.as_ref(), backend, first_work, "node-a", now).await;

        // Releasing the exact lease closes the first detached completion.
        assert!(
            store
                .release_lease(&first.lease, now + 1)
                .await
                .expect("release first owner"),
            "{backend}"
        );
        let still_running = store
            .background_job(&first.job_id)
            .await
            .expect("read queue owner after domain release")
            .expect("old queue job retained");
        assert_eq!(still_running.state, JobState::Running, "{backend}");
        assert_eq!(
            still_running.token.as_ref(),
            Some(&first.token),
            "{backend}"
        );
        assert!(first.token.lease_expires_ms > now + 2, "{backend}");
        let after_release = store
            .complete_library_work(CompleteLibraryWork {
                token: first.token.clone(),
                lease: first.lease.clone(),
                request_id: "request-old".into(),
                result: LibraryWorkResult::Failed {
                    error: "stale after release".into(),
                },
                now_ms: now + 2,
            })
            .await
            .expect("stale completion verdict");
        assert!(
            !after_release,
            "{backend}: release linearized before the detached write"
        );
        assert_eq!(
            request_record(store.as_ref(), "request-old").await.state,
            "pending",
            "{backend}"
        );

        // S2 isolates release of the domain owner while the old queue token
        // is still valid. Follow the worker's real unavailable-domain policy:
        // yield the current attempt without charging a failure, keep its
        // request pending, and release its source_io reservation until retry.
        assert!(
            store
                .settle_job(SettleJob {
                    token: first.token.clone(),
                    settlement: JobSettlement::Yield {
                        error_code: None,
                        checkpoint: None,
                        not_before_ms: now + 5_000,
                    },
                    now_ms: now + 3,
                })
                .await
                .expect("yield current queue attempt"),
            "{backend}: real worker yield accepted"
        );
        assert_eq!(
            request_record(store.as_ref(), "request-old").await.state,
            "pending",
            "{backend}: yield preserves pending request"
        );
        let yielded = store
            .background_job(&first.job_id)
            .await
            .expect("read yielded queue owner")
            .expect("old queue job retained");
        assert_eq!(yielded.state, JobState::Queued, "{backend}");
        assert_eq!(yielded.failed_attempts, 0, "{backend}");
        assert!(yielded.token.is_none(), "{backend}");
        assert_eq!(yielded.not_before_ms, now + 5_000, "{backend}");
        // Claim the second job before the yielded attempt's retry is eligible,
        // proving the real settlement released source_io capacity.
        let replacement_time = now + 4;
        let second_work = enqueue_and_claim_library_work(
            store.as_ref(),
            backend,
            library_id,
            "node-b",
            "request-new",
            true,
            replacement_time,
        )
        .await;
        assert_ne!(
            first.job_id, second_work.job_id,
            "{backend}: separate dedupe identities"
        );
        let second = bind_library_work(
            store.as_ref(),
            backend,
            second_work,
            "node-b",
            replacement_time,
        )
        .await;
        assert!(
            second.lease.fence > first.lease.fence,
            "{backend}: higher domain fence"
        );
        let after_rebind = store
            .complete_library_work(CompleteLibraryWork {
                token: first.token.clone(),
                lease: first.lease.clone(),
                request_id: "request-old".into(),
                result: LibraryWorkResult::Failed {
                    error: "stale after replacement".into(),
                },
                now_ms: replacement_time + 1,
            })
            .await
            .expect("old owner verdict");
        assert!(
            !after_rebind,
            "{backend}: replacement owner invalidates old completion"
        );
        assert_eq!(
            request_record(store.as_ref(), "request-old").await.state,
            "pending",
            "{backend}"
        );
        assert!(
            store
                .complete_library_work(CompleteLibraryWork {
                    token: second.token.clone(),
                    lease: second.lease.clone(),
                    request_id: "request-new".into(),
                    result: LibraryWorkResult::Failed {
                        error: "new owner result".into()
                    },
                    now_ms: replacement_time + 2,
                })
                .await
                .expect("new owner completion"),
            "{backend}: replacement may complete its request"
        );
        second.retire(store.as_ref(), replacement_time).await;
    })
    .await;
}

#[cfg(feature = "hiqlite-contract-tests")]
mod replicated {
    //! Replicated-only examples: the statement-zero classification pin and
    //! the two insertion traces that need the feature-gated seam.
    use super::super::{
        open_contract_hiqlite_store, ContractCluster, CONTRACT_API_SECRET, HIQLITE_CASE,
    };
    use super::*;
    use plurx_core::store::publication_conformance::{
        install, Acknowledgement, InsertionAttempt, InsertionSeam,
    };
    use plurx_core::store::{HiqliteAuthStore, PublicationFence, PublicationStore};
    use std::sync::Mutex;

    /// Error text production reports when a replicated reply never arrives
    /// (`hiqlite.rs`, `REPLICATED_STORE_TIMEOUT`); the seam's `Lose` maps to
    /// exactly that error. Pinned here so a drift in the production text is
    /// visible to this example.
    const LOST_REPLY: &str = "replicated store operation timed out";

    async fn raw_client(cluster: &ContractCluster) -> hiqlite::Client {
        hiqlite::Client::remote(
            cluster.addresses.clone(),
            true,
            true,
            CONTRACT_API_SECRET.to_owned(),
            false,
            None,
        )
        .await
        .expect("raw replicated client")
    }

    /// The store under test, reset, as the concrete handle so a seam can be
    /// installed on it. Callers coerce to `&dyn Store` where needed.
    async fn fresh_store(cluster: &ContractCluster) -> Arc<HiqliteAuthStore> {
        let store = open_contract_hiqlite_store(cluster).await;
        store
            .validation_reset_contract_state()
            .await
            .expect("reset replicated contract state");
        Arc::new(store)
    }

    /// Another client of the same cluster without the reset, for isolation
    /// examples: it shares the data, not the handle.
    async fn other_store(cluster: &ContractCluster) -> Arc<HiqliteAuthStore> {
        Arc::new(open_contract_hiqlite_store(cluster).await)
    }

    async fn raw_titles(client: &hiqlite::Client, library_id: i64) -> Vec<String> {
        struct TitleRow {
            title: String,
        }
        impl From<&mut hiqlite::Row<'_>> for TitleRow {
            fn from(row: &mut hiqlite::Row<'_>) -> Self {
                Self {
                    title: row.get("title"),
                }
            }
        }
        client
            .query_consistent_map::<TitleRow, _>(
                "SELECT title FROM items WHERE library_id = $1 ORDER BY title",
                hiqlite::params!(library_id),
            )
            .await
            .expect("raw titles")
            .into_iter()
            .map(|row| row.title)
            .collect()
    }

    /// Records every attempt and discards the acknowledgement of the first
    /// successful submission exactly once.
    struct LoseFirstReply {
        observed: Mutex<Vec<InsertionAttempt>>,
        lost: Mutex<bool>,
    }

    #[async_trait::async_trait]
    impl InsertionSeam for LoseFirstReply {
        async fn before_submission(&self, attempt: &InsertionAttempt) {
            self.observed
                .lock()
                .expect("observed")
                .push(attempt.clone());
        }

        fn acknowledgement(&self, _attempt: &InsertionAttempt) -> Acknowledgement {
            let mut lost = self.lost.lock().expect("lost");
            if *lost {
                return Acknowledgement::Deliver;
            }
            *lost = true;
            Acknowledgement::Lose
        }
    }

    /// Revokes the facade's local fence after the backend committed and
    /// keeps the acknowledgement, which is the `fenced_call` post-check path.
    struct RevokeAfterCommit {
        fence: PublicationFence,
    }

    #[async_trait::async_trait]
    impl InsertionSeam for RevokeAfterCommit {
        async fn before_submission(&self, _attempt: &InsertionAttempt) {}

        fn acknowledgement(&self, _attempt: &InsertionAttempt) -> Acknowledgement {
            self.fence.revoke();
            Acknowledgement::Deliver
        }
    }

    /// Spends the selected candidate identity through the real allocator
    /// trigger before the first `conflicts` submissions are sent.
    struct CompeteForCandidate {
        client: hiqlite::Client,
        library_id: i64,
        conflicts: usize,
        observed: Mutex<Vec<InsertionAttempt>>,
    }

    #[async_trait::async_trait]
    impl InsertionSeam for CompeteForCandidate {
        async fn before_submission(&self, attempt: &InsertionAttempt) {
            self.observed
                .lock()
                .expect("observed")
                .push(attempt.clone());
            if attempt.attempt < self.conflicts {
                let title = format!("competitor {}", attempt.attempt);
                self.client
                    .execute(
                        "INSERT INTO items(id,library_id,kind,title,sort_title,added_at,updated_at) \
                         VALUES($1,$2,'movie',$3,$4,0,0)",
                        hiqlite::params!(attempt.candidate_id, self.library_id, title.as_str(), title.as_str()),
                    )
                    .await
                    .expect("competing allocation through the real trigger");
            }
        }

        fn acknowledgement(&self, _attempt: &InsertionAttempt) -> Acknowledgement {
            Acknowledgement::Deliver
        }
    }

    /// *Assumption pin*: `atomic_publication` classifies the vendor error
    /// text `StmtIndex(0) does not have observable row output` as
    /// `FenceRejected`. This exercises the real zero-row statement-zero case
    /// through the raw client on the pinned Hiqlite, shows the transaction
    /// rolled back an earlier executed statement, shows the positive case
    /// binds, and then shows the store path reports `FenceRejected`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn formal_publication_hiqlite_statement_zero_missing_output_is_fence_rejected_and_rolled_back(
    ) {
        let _case = HIQLITE_CASE.lock().await;
        let cluster = ContractCluster::start().await;
        let handle = fresh_store(&cluster).await;
        let store: Arc<dyn Store> = handle.clone();
        let client = raw_client(&cluster).await;
        let backend = "hiqlite-3-voter";
        let now = unix_now_ms();
        let library_id = create_library(store.as_ref(), "formal-stmt0").await;
        let lease = acquired(
            store
                .acquire_lease(
                    &Scan::resource(library_id),
                    "node-a",
                    now,
                    now + DOMAIN_TTL_MS,
                )
                .await
                .expect("domain lease"),
            backend,
        );

        let statements = |resource: &str, marker: &str, bound: &str| {
            vec![
                (
                    "UPDATE job_leases SET revision = revision WHERE resource = $1 RETURNING resource"
                        .to_owned(),
                    hiqlite::params!(resource.to_owned()),
                ),
                (
                    "INSERT INTO items(library_id,kind,title,sort_title,added_at,updated_at) \
                     VALUES($1,'movie',$2,$2,0,0)"
                        .to_owned(),
                    hiqlite::params!(library_id, marker.to_owned()),
                ),
                (
                    "INSERT INTO items(library_id,kind,title,sort_title,added_at,updated_at) \
                     SELECT $1,'movie',$2,$2,0,0 WHERE $3 IS NOT NULL"
                        .to_owned(),
                    hiqlite::params!(
                        library_id,
                        bound.to_owned(),
                        hiqlite::Param::StmtOutputNamed(0, "resource".into())
                    ),
                ),
            ]
        };

        let missing = client
            .txn(statements(
                "scan:library:absent",
                "marker rolled back",
                "never bound",
            ))
            .await
            .expect_err("statement zero without a row cannot bind its output");
        assert!(
            missing
                .to_string()
                .contains("StmtIndex(0) does not have observable row output"),
            "{backend}: pinned vendor text changed: {missing}"
        );
        assert!(
            raw_titles(&client, library_id).await.is_empty(),
            "{backend}: the executed marker statement was rolled back"
        );

        let bound = client
            .txn(statements(&lease.resource, "marker committed", "bound"))
            .await
            .expect("statement zero with a row binds");
        for result in bound {
            result.expect("bound statement");
        }
        assert_eq!(
            raw_titles(&client, library_id).await,
            vec!["bound".to_owned(), "marker committed".to_owned()],
            "{backend}"
        );

        // Store path: a stale exact predecessor is exactly a zero-row
        // statement zero, reported as FenceRejected with nothing retained.
        let successor = publication_successor(&lease);
        store
            .insert_item_fenced(&movie(library_id, "live"), &lease, &successor)
            .await
            .expect("live domain publication");
        let stale = store
            .insert_item_fenced(&movie(library_id, "stale"), &lease, &successor)
            .await;
        assert!(is_fence_rejected(&stale), "{backend}: {stale:?}");
        assert_eq!(
            raw_titles(&client, library_id).await,
            vec![
                "bound".to_owned(),
                "live".to_owned(),
                "marker committed".to_owned()
            ],
            "{backend}"
        );
    }

    /// §8.2 lost-reply example (review R1): the chosen insertion commits, its
    /// acknowledgement is lost at the real submission boundary, and the
    /// facade reports an error while clearing its local authority. The item
    /// is retained; a retry with the exact predecessor is stale; the
    /// committed successor is the live token. A lost reply is not an
    /// identity-conflict error, so no automatic resubmission happens.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn formal_publication_hiqlite_lost_acknowledgement_retains_commit_and_clears_facade() {
        let _case = HIQLITE_CASE.lock().await;
        let cluster = ContractCluster::start().await;
        let handle = fresh_store(&cluster).await;
        let store: Arc<dyn Store> = handle.clone();
        let client = raw_client(&cluster).await;
        let backend = "hiqlite-3-voter";
        let now = unix_now_ms();
        let library_id = create_library(store.as_ref(), "formal-lost-reply").await;
        let scan = bound_scan(store.as_ref(), backend, library_id, now, now).await;
        let fence = PublicationFence::new(scan.lease.clone());
        let publisher = PublicationStore::fenced(store.as_ref(), fence.clone());

        let seam = Arc::new(LoseFirstReply {
            observed: Mutex::new(Vec::new()),
            lost: Mutex::new(false),
        });
        let lost = {
            let _installed = install(&handle, seam.clone());
            // Isolation: the seam belongs to `handle`. A separately opened
            // client of the same cluster submits the same kind of insertion
            // through its own handle, and neither boundary is observed.
            let other: Arc<dyn Store> = other_store(&cluster).await;
            let other_library = create_library(other.as_ref(), "formal-other-client").await;
            let other_lease = acquired(
                other
                    .acquire_lease(
                        &Scan::resource(other_library),
                        "node-b",
                        now,
                        now + DOMAIN_TTL_MS,
                    )
                    .await
                    .expect("other client's domain lease"),
                backend,
            );
            other
                .insert_item_fenced(
                    &movie(other_library, "other client"),
                    &other_lease,
                    &publication_successor(&other_lease),
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: other client acknowledged: {error}"));
            assert!(
                seam.observed.lock().expect("observed").is_empty(),
                "{backend}: a seam never observes another handle's submissions"
            );
            assert_eq!(
                raw_titles(&client, other_library).await,
                vec!["other client".to_owned()],
                "{backend}"
            );
            publisher
                .insert_item(&movie(library_id, "committed unacknowledged"))
                .await
        };
        assert!(
            matches!(&lost, Err(StoreError::Database(message)) if message == LOST_REPLY),
            "{backend}: {lost:?}"
        );
        let observed = seam.observed.lock().expect("observed").clone();
        assert_eq!(
            observed.len(),
            1,
            "{backend}: a lost reply is not resubmitted"
        );
        assert_eq!(observed[0].predecessor, scan.lease, "{backend}");
        let successor = observed[0].successor.clone();
        assert_eq!(successor.revision, scan.lease.revision + 1, "{backend}");

        // Facade cleared its local authority on the error.
        assert!(
            fence.snapshot().await.is_none(),
            "{backend}: facade state cleared"
        );
        let after = publisher
            .insert_item(&movie(library_id, "after clearing"))
            .await;
        assert!(is_fence_rejected(&after), "{backend}: {after:?}");

        // The commit is real: the item is visible to a raw consistent read
        // and to the Store, and the predecessor is stale.
        assert_eq!(
            raw_titles(&client, library_id).await,
            vec!["committed unacknowledged".to_owned()],
            "{backend}"
        );
        assert_eq!(
            titles(store.as_ref(), library_id).await,
            vec!["committed unacknowledged".to_owned()],
            "{backend}"
        );
        let stale_retry = store
            .insert_item_fenced(&movie(library_id, "stale retry"), &scan.lease, &successor)
            .await;
        assert!(
            is_fence_rejected(&stale_retry),
            "{backend}: {stale_retry:?}"
        );
        // Evidence only: the test knows the successor the facade lost.
        let live = publication_successor(&successor);
        store
            .insert_item_fenced(&movie(library_id, "successor live"), &successor, &live)
            .await
            .unwrap_or_else(|error| panic!("{backend}: committed successor is live: {error}"));
        assert_eq!(
            raw_titles(&client, library_id).await,
            vec![
                "committed unacknowledged".to_owned(),
                "successor live".to_owned()
            ],
            "{backend}"
        );

        // Facade local revocation after a real commit returns FenceRejected
        // while the item stays visible: an error return does not imply that
        // no write happened.
        let revoked_fence = PublicationFence::new(live.clone());
        let revoked_publisher = PublicationStore::fenced(store.as_ref(), revoked_fence.clone());
        let revoked = {
            let _installed = install(
                &handle,
                Arc::new(RevokeAfterCommit {
                    fence: revoked_fence.clone(),
                }),
            );
            revoked_publisher
                .insert_item(&movie(library_id, "revoked after commit"))
                .await
        };
        assert!(is_fence_rejected(&revoked), "{backend}: {revoked:?}");
        assert_eq!(
            raw_titles(&client, library_id).await,
            vec![
                "committed unacknowledged".to_owned(),
                "revoked after commit".to_owned(),
                "successor live".to_owned(),
            ],
            "{backend}: locally revoked publication was committed"
        );
        scan.retire(store.as_ref(), now).await;
    }

    /// §8.2 allocator-conflict example (review R1): with the candidate item
    /// identity allocator installed, a submission whose candidate was spent
    /// by another writer fails on the real `item_identity_reuse` trigger and
    /// is resubmitted with the same predecessor/successor and a fresh
    /// candidate; four conflicts exhaust the loop with nothing committed and
    /// no authority transition.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn formal_publication_hiqlite_allocator_conflict_retries_same_authority_fresh_candidate()
    {
        let _case = HIQLITE_CASE.lock().await;
        let cluster = ContractCluster::start().await;
        let handle = fresh_store(&cluster).await;
        let store: Arc<dyn Store> = handle.clone();
        let client = raw_client(&cluster).await;
        let backend = "hiqlite-3-voter";
        for result in client
            .txn(
                plurx_core::store::sharing_catalogue_source::candidate_item_identity_statements()
                    .into_iter()
                    .map(|sql| (sql, hiqlite::params!()))
                    .collect::<Vec<_>>(),
            )
            .await
            .expect("install candidate allocator")
        {
            result.expect("allocator statement");
        }
        let now = unix_now_ms();
        let library_id = create_library(store.as_ref(), "formal-allocator").await;
        let scan = bound_scan(store.as_ref(), backend, library_id, now, now).await;

        let one = Arc::new(CompeteForCandidate {
            client: client.clone(),
            library_id,
            conflicts: 1,
            observed: Mutex::new(Vec::new()),
        });
        let successor = publication_successor(&scan.lease);
        let id = {
            let _installed = install(&handle, one.clone());
            store
                .insert_item_fenced(&movie(library_id, "retried"), &scan.lease, &successor)
                .await
                .unwrap_or_else(|error| panic!("{backend}: retry after conflict: {error}"))
        };
        let observed = one.observed.lock().expect("observed").clone();
        assert_eq!(
            observed.len(),
            2,
            "{backend}: one conflict, one resubmission"
        );
        assert_eq!(
            observed[0].predecessor, observed[1].predecessor,
            "{backend}"
        );
        assert_eq!(observed[1].predecessor, scan.lease, "{backend}");
        assert_eq!(observed[0].successor, observed[1].successor, "{backend}");
        assert_eq!(observed[1].successor, successor, "{backend}");
        assert_eq!(
            observed[1].candidate_id,
            observed[0].candidate_id + 1,
            "{backend}"
        );
        assert_eq!(
            id, observed[1].candidate_id,
            "{backend}: fresh candidate committed"
        );
        assert_eq!(
            raw_titles(&client, library_id).await,
            vec!["competitor 0".to_owned(), "retried".to_owned()],
            "{backend}"
        );

        // Exhaustion: four conflicts, no commit, predecessor still live.
        let every = Arc::new(CompeteForCandidate {
            client: client.clone(),
            library_id,
            conflicts: usize::MAX,
            observed: Mutex::new(Vec::new()),
        });
        let next = publication_successor(&successor);
        let exhausted = {
            let _installed = install(&handle, every.clone());
            store
                .insert_item_fenced(&movie(library_id, "never committed"), &successor, &next)
                .await
        };
        assert!(
            matches!(&exhausted, Err(StoreError::Database(message))
                if message == "item identity allocation contention"),
            "{backend}: {exhausted:?}"
        );
        let observed = every.observed.lock().expect("observed").clone();
        assert_eq!(observed.len(), 4, "{backend}: bounded retry loop");
        assert!(
            observed
                .iter()
                .all(|attempt| attempt.predecessor == successor && attempt.successor == next),
            "{backend}: same authority on every resubmission"
        );
        assert!(
            observed
                .windows(2)
                .all(|pair| pair[1].candidate_id == pair[0].candidate_id + 1),
            "{backend}: fresh candidate on every resubmission"
        );
        assert_eq!(
            raw_titles(&client, library_id).await,
            vec![
                "competitor 0".to_owned(),
                "competitor 0".to_owned(),
                "competitor 1".to_owned(),
                "competitor 2".to_owned(),
                "competitor 3".to_owned(),
                "retried".to_owned(),
            ],
            "{backend}: only the competitors' rows exist"
        );
        // Every rejected submission rolled back the renewal too: the exact
        // predecessor still publishes.
        store
            .insert_item_fenced(&movie(library_id, "after exhaustion"), &successor, &next)
            .await
            .unwrap_or_else(|error| panic!("{backend}: predecessor still live: {error}"));
        scan.retire(store.as_ref(), now).await;
    }
}
