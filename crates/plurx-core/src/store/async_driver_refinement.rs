//! Actual common-SQL conformance for the async library-worker boundary.
//!
//! The graph covers only explicitly bounded selected-protocol inputs when
//! its BFS closes; predicate-partition cases are separate conformance.
//! Neither result proves arbitrary production histories or SQLite/Raft internals.
//! QueueSql below delegates to the real backend; it never copies production
//! transition SQL, validation, schema, or trigger definitions.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::{json, Value};

use super::background_jobs::{
    BackgroundJobStore, ClaimJob, ClaimOutcome, ClaimResolution, JobKind, JobSettlement, JobState,
    JobToken, QueueSql, RenewJobs, RenewOutcome, ResolveClaim, SettleJob,
};
use super::background_jobs_domain::BindLibraryJob;
use super::background_jobs_library::{
    CompleteLibraryWork, LibraryTrigger, LibraryWorkInput, LibraryWorkQuery, LibraryWorkResult,
    NewLibraryWork,
};
use super::background_jobs_resources::StorageDomainMapping;
use super::{CoordinationStore, LibraryStore, SqliteStore};
use crate::cluster::coordination::{Lease, LeaseClaim};
use crate::domain::{LibraryKind, NewLibrary};
use crate::error::StoreError;

const NOW: i64 = 1_000;
const NODES: [&str; 2] = ["node-a", "node-b"];
const REQUESTS: [&str; 2] = ["selected-scan", "selected-refresh"];
const BOOTS: [&str; 2] = [
    "00000000-0000-4000-8000-000000000001",
    "00000000-0000-4000-8000-000000000002",
];

struct Fixture {
    store: SqliteStore,
    library: i64,
    jobs: [String; 2],
    omitted_dv_sequence: Value,
}

impl Fixture {
    async fn new() -> Self {
        Self::seed(SqliteStore::open_in_memory().expect("production SQLite schema")).await
    }

    async fn seed(store: SqliteStore) -> Self {
        // Preserve the actual migrated AUTOINCREMENT state, including absence
        // versus an initialized zero; no selected operation may change it.
        let omitted_dv_sequence = omitted_dv_state(&store)
            .await
            .expect("initialized omitted DV state")["sequence"]
            .clone();
        // This installs all production migrations in their real order,
        // including later replacements of the required-resource view.
        let library = store
            .create_library(&NewLibrary {
                name: "async-store-domain".into(),
                kind: LibraryKind::Movies,
                paths: vec!["/async-store-root".into()],
                anime: false,
            })
            .await
            .expect("library")
            .id;
        assert!(store
            .replace_storage_domains(
                vec![StorageDomainMapping {
                    library_id: library,
                    root_path: "/async-store-root".into(),
                    domain_id: "shared-root".into(),
                }],
                NOW
            )
            .await
            .expect("storage mapping"));
        let mut jobs = Vec::new();
        for (index, request_id) in REQUESTS.into_iter().enumerate() {
            let outcome = store
                .enqueue_library_work(NewLibraryWork {
                    request_id: request_id.into(),
                    library_id: library,
                    now_ms: NOW,
                    input: LibraryWorkInput::Full {
                        refresh: index == 1,
                        trigger: LibraryTrigger::Manual,
                    },
                })
                .await
                .expect("paired durable intent");
            let super::background_jobs::EnqueueOutcome::Accepted { job_id, .. } = outcome else {
                panic!("fresh selected request was not accepted: {outcome:?}");
            };
            jobs.push(job_id);
        }
        Self {
            store,
            library,
            jobs: jobs.try_into().expect("two selected jobs"),
            omitted_dv_sequence,
        }
    }

    fn claim(&self, index: usize, revision: i64, now_ms: i64, attempt: u8) -> ClaimJob {
        ClaimJob {
            job_id: self.jobs[index].clone(),
            expected_revision: revision,
            node_id: NODES[index].into(),
            boot_id: BOOTS[index].into(),
            claim_id: format!(
                "00000000-0000-4000-8000-{:012}",
                10 + index * 4 + usize::from(attempt)
            ),
            kind: if index == 0 {
                JobKind::LibraryScan
            } else {
                JobKind::MetadataRefresh
            },
            payload_version: 1,
            now_ms,
            dispatched_at_ms: now_ms,
        }
    }

    async fn claimed(&self, index: usize) -> JobToken {
        let ClaimOutcome::Claimed { job } = self
            .store
            .claim_job(self.claim(index, 0, NOW, 0))
            .await
            .expect("claim")
        else {
            panic!("selected job claim refused");
        };
        job.token.expect("claimed queue token")
    }

    async fn lease(&self, index: usize) -> Lease {
        let LeaseClaim::Acquired(lease) = self
            .store
            .acquire_lease(
                &format!("scan:library:{}", self.library),
                NODES[index],
                NOW,
                NOW + 90_000,
            )
            .await
            .expect("domain acquire")
        else {
            panic!("domain refused");
        };
        lease
    }

    async fn bound(&self) -> (JobToken, Lease) {
        let token = self.claimed(0).await;
        let lease = self.lease(0).await;
        assert!(self
            .store
            .bind_library_job(BindLibraryJob {
                token: token.clone(),
                lease: lease.clone(),
                now_ms: NOW,
            })
            .await
            .expect("bind"));
        (token, lease)
    }

    fn completion(&self, token: JobToken, lease: Lease, now_ms: i64) -> CompleteLibraryWork {
        CompleteLibraryWork {
            token,
            lease,
            request_id: REQUESTS[0].into(),
            now_ms,
            result: LibraryWorkResult::Completed {
                scan: crate::scan::TargetedScan {
                    report: crate::scan::ScanReport::default(),
                    items: vec![],
                },
            },
        }
    }
}

async fn sql<T: QueueSql>(store: &T, statement: &str, parameters: Value) -> Vec<String> {
    store
        .queue_sql(statement.into(), parameters.to_string(), true, true)
        .await
        .expect("fixture SQL")
}

// Compare every column of every durable table in this slice after a failed
// CAS. Column discovery comes from the installed production schema, so a new
// column cannot silently evade the no-mutation observation.
const SNAPSHOT_TABLES: [&str; 9] = [
    "background_jobs",
    "background_job_waiters",
    "background_job_attempts",
    "background_job_reservations",
    "background_job_domain_leases",
    "background_library_requests",
    "background_job_commands",
    "job_leases",
    "settings",
];

async fn snapshot<T: QueueSql>(store: &T) -> Value {
    static SNAPSHOT_SQL: OnceLock<String> = OnceLock::new();
    if SNAPSHOT_SQL.get().is_none() {
        let mut pairs = Vec::new();
        for table in SNAPSHOT_TABLES {
            let rows = store.queue_sql(format!("SELECT json_group_array(name) FROM pragma_table_info('{table}') WHERE json_valid($1)"),
                "null".into(), false, true).await.expect("schema columns");
            let columns: Vec<String> = serde_json::from_str(&rows[0]).expect("column names");
            assert!(!columns.is_empty(), "missing production table {table}");
            let columns = columns
                .into_iter()
                .map(|name| format!("\"{}\"", name.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(",");
            pairs.push(format!("'{table}',json((SELECT json_group_array(json(row)) FROM (SELECT json_array({columns}) AS row FROM {table} ORDER BY {columns})))"));
        }
        let _ = SNAPSHOT_SQL.set(format!(
            "SELECT json_object({}) WHERE json_valid($1)",
            pairs.join(",")
        ));
    }
    let rows = store
        .queue_sql(
            SNAPSHOT_SQL
                .get()
                .expect("initialised snapshot SQL")
                .clone(),
            "null".into(),
            false,
            true,
        )
        .await
        .expect("one authoritative full-column durable snapshot");
    serde_json::from_str(&rows[0]).expect("snapshot JSON")
}

fn resolution(request: &ClaimJob, fence: Option<i64>, now_ms: i64) -> ResolveClaim {
    ResolveClaim {
        job_id: request.job_id.clone(),
        node_id: request.node_id.clone(),
        boot_id: request.boot_id.clone(),
        claim_id: request.claim_id.clone(),
        fence,
        dispatched_at_ms: request.dispatched_at_ms,
        now_ms,
    }
}

fn yielding(token: JobToken, now_ms: i64) -> SettleJob {
    SettleJob {
        token,
        now_ms,
        settlement: JobSettlement::Yield {
            error_code: None,
            checkpoint: None,
            not_before_ms: now_ms,
        },
    }
}

// Models only the observable SQL bridge reply boundary. Mode 1 refuses
// before application; mode 2 applies through SQLite and loses the reply.
// Neither mode simulates a running blocking transaction's cancellation,
// TimedClient's timer, nor Raft; backend execution stays an explicit boundary.
struct ReplyBoundary<'a> {
    store: &'a SqliteStore,
    mode: AtomicU8,
    submissions: AtomicUsize,
}

#[async_trait]
impl QueueSql for ReplyBoundary<'_> {
    async fn queue_transaction(&self, statements: Vec<(String, String)>) -> Result<(), StoreError> {
        self.store.queue_transaction(statements).await
    }

    async fn queue_sql(
        &self,
        statement: String,
        request: String,
        mutation: bool,
        authoritative: bool,
    ) -> Result<Vec<String>, StoreError> {
        if !mutation {
            return self
                .store
                .queue_sql(statement, request, mutation, authoritative)
                .await;
        }
        self.submissions.fetch_add(1, Ordering::SeqCst);
        let mode = self.mode.swap(0, Ordering::SeqCst);
        if mode == 1 {
            return Err(StoreError::Database(
                "injected unknown application before SQL".into(),
            ));
        }
        let result = self
            .store
            .queue_sql(statement, request, mutation, authoritative)
            .await?;
        if mode == 2 {
            return Err(StoreError::Database(
                "injected application before lost reply".into(),
            ));
        }
        Ok(result)
    }
}

async fn reservations<T: QueueSql>(store: &T, job: &str) -> Value {
    let rows = store.queue_sql("SELECT json_group_array(json_object('resource',resource_key,'fence',fence,'expiry',expires_at_ms)) FROM (SELECT * FROM background_job_reservations WHERE job_id=json_extract($1,'$.job') ORDER BY resource_key)".into(),
        json!({"job": job}).to_string(), false, true).await.expect("reservations");
    serde_json::from_str(&rows[0]).expect("reservation JSON")
}

async fn completion_contract(fixture: &Fixture) -> bool {
    let job = fixture
        .store
        .background_job(&fixture.jobs[0])
        .await
        .expect("job")
        .expect("job");
    let requests = fixture
        .store
        .library_work_requests(LibraryWorkQuery {
            job_id: Some(fixture.jobs[0].clone()),
            limit: 2,
            ..Default::default()
        })
        .await
        .expect("request reconciliation");
    job.state == JobState::Succeeded
        && job.token.is_none()
        && reservations(&fixture.store, &fixture.jobs[0]).await == json!([])
        && requests.len() == 1
        && requests[0].state == "succeeded"
        && requests[0].result.is_some()
}

#[tokio::test]
async fn two_node_same_root_claim_refusal_and_atomic_last_request_retirement() {
    let fixture = Fixture::new().await;
    let (token, lease) = fixture.bound().await;
    let held = reservations(&fixture.store, &fixture.jobs[0]).await;
    assert_eq!(
        held.as_array()
            .expect("resources")
            .iter()
            .map(|row| row["resource"]
                .as_str()
                .expect("string in typed JSON projection"))
            .collect::<Vec<_>>(),
        vec!["provider:anilist", "provider:tmdb", "source_io:shared-root"]
    );
    let before = snapshot(&fixture.store).await;
    assert!(matches!(
        fixture
            .store
            .claim_job(fixture.claim(1, 0, NOW, 0))
            .await
            .expect("second node claim"),
        ClaimOutcome::Contended
    ));
    assert_eq!(
        snapshot(&fixture.store).await,
        before,
        "refusal cannot create an attempt or reservation"
    );
    assert!(fixture
        .store
        .complete_library_work(fixture.completion(token.clone(), lease, NOW + 1))
        .await
        .expect("last request"));
    assert!(
        completion_contract(&fixture).await,
        "last request must retire job and resources atomically"
    );
    let after = snapshot(&fixture.store).await;
    assert!(!fixture
        .store
        .settle_job(yielding(token, NOW + 2))
        .await
        .expect("already retired yield"));
    assert_eq!(
        snapshot(&fixture.store).await,
        after,
        "later stale retirement cannot mutate the result"
    );
    assert!(matches!(
        fixture
            .store
            .claim_job(fixture.claim(1, 0, NOW + 2, 0))
            .await
            .expect("successor node"),
        ClaimOutcome::Claimed { .. }
    ));
}

#[tokio::test]
async fn reply_loss_keeps_application_and_exact_claim_reconciliation_distinct() {
    for mode in [1, 2] {
        let fixture = Fixture::new().await;
        let boundary = ReplyBoundary {
            store: &fixture.store,
            mode: AtomicU8::new(mode),
            submissions: AtomicUsize::new(0),
        };
        let request = fixture.claim(0, 0, NOW, 0);
        let before = snapshot(&fixture.store).await;
        assert!(boundary.claim_job(request.clone()).await.is_err());
        assert_eq!(
            boundary.submissions.load(Ordering::SeqCst),
            1,
            "unknown reply is not an internal retry"
        );
        let outcome = fixture
            .store
            .resolve_claim(resolution(&request, None, NOW + 1))
            .await
            .expect("resolve");
        if mode == 1 {
            assert!(matches!(outcome, ClaimResolution::ExpiredOrPruned));
            assert_eq!(snapshot(&fixture.store).await, before);
        } else {
            let ClaimResolution::Running { token } = outcome else {
                panic!("applied claim lost its identity");
            };
            assert_eq!(token.claim_id, request.claim_id);
            assert_eq!(
                fixture
                    .store
                    .job_attempts(&request.job_id)
                    .await
                    .expect("attempts")
                    .len(),
                1
            );
            for identity in ["boot", "claim", "node", "fence"] {
                let mut wrong = resolution(&request, Some(token.fence), NOW + 1);
                match identity {
                    "boot" => wrong.boot_id = BOOTS[1].into(),
                    "claim" => wrong.claim_id = fixture.claim(0, 0, NOW, 1).claim_id,
                    "node" => wrong.node_id = NODES[1].into(),
                    _ => wrong.fence = Some(token.fence + 1),
                }
                assert!(
                    !matches!(
                        fixture
                            .store
                            .resolve_claim(wrong)
                            .await
                            .expect("wrong identity"),
                        ClaimResolution::Running { .. }
                    ),
                    "cannot adopt wrong {identity}"
                );
            }
            // Repeating the exact applied attempt returns the same fence,
            // rather than turning an uncertain write into a second claim.
            let ClaimOutcome::Claimed { job } =
                fixture.store.claim_job(request).await.expect("replay")
            else {
                panic!("exact claim replay refused");
            };
            assert_eq!(job.token.expect("replayed token"), token);
        }
    }
}

#[tokio::test]
async fn renewal_and_completion_application_before_reply_reconcile_authoritatively() {
    let fixture = Fixture::new().await;
    let (old, lease) = fixture.bound().await;
    let boundary = ReplyBoundary {
        store: &fixture.store,
        mode: AtomicU8::new(2),
        submissions: AtomicUsize::new(0),
    };
    assert!(boundary
        .renew_jobs(RenewJobs {
            tokens: vec![old.clone()],
            now_ms: NOW + 1
        })
        .await
        .is_err());
    assert_eq!(boundary.submissions.load(Ordering::SeqCst), 1);
    let request = fixture.claim(0, 0, NOW, 0);
    let ClaimResolution::Running { token } = fixture
        .store
        .resolve_claim(resolution(&request, Some(old.fence), NOW + 2))
        .await
        .expect("renewal resolve")
    else {
        panic!("renewal application not resolved");
    };
    assert_eq!(token.revision, old.revision + 1);
    assert_eq!(token.fence, old.fence);
    let held = reservations(&fixture.store, &fixture.jobs[0]).await;
    assert!(held
        .as_array()
        .expect("typed reservation array")
        .iter()
        .all(|row| row["expiry"] == token.lease_expires_ms));
    let before = snapshot(&fixture.store).await;
    assert!(!fixture
        .store
        .complete_library_work(fixture.completion(old, lease.clone(), NOW + 2))
        .await
        .expect("old revision"));
    assert_eq!(snapshot(&fixture.store).await, before);
    boundary.mode.store(2, Ordering::SeqCst);
    assert!(boundary
        .complete_library_work(fixture.completion(token, lease, NOW + 2))
        .await
        .is_err());
    assert_eq!(boundary.submissions.load(Ordering::SeqCst), 2);
    assert!(
        completion_contract(&fixture).await,
        "lost completion reply is not rollback"
    );
}

#[tokio::test]
async fn completion_trigger_mutation_is_detected_by_last_request_atomic_retirement() {
    let directory = tempfile::tempdir().expect("mutation directory");
    let path = directory.path().join("mutation.sqlite");
    let fixture = Fixture::seed(SqliteStore::open(&path).expect("production file schema")).await;
    let (token, lease) = fixture.bound().await;
    let schema = super::background_jobs_library::SCHEMA;
    let trigger = schema
        .split("-- next statement")
        .find(|statement| {
            statement.contains("CREATE TRIGGER IF NOT EXISTS background_library_request_completed")
        })
        .expect("production completion trigger");
    let start = trigger
        .find("    UPDATE background_jobs SET state = 'succeeded'")
        .expect("mutation's production job-succeed clause");
    let mutated = format!("{}\nEND;", &trigger[..start]);
    let connection = rusqlite::Connection::open(&path).expect("mutation connection");
    connection
        .execute_batch("DROP TRIGGER background_library_request_completed;")
        .expect("remove original");
    connection
        .execute_batch(&mutated)
        .expect("install source-derived mutated trigger");
    assert!(fixture
        .store
        .complete_library_work(fixture.completion(token, lease, NOW + 1))
        .await
        .expect("mutated completion applies"));
    assert!(
        !completion_contract(&fixture).await,
        "C4 must violate named LastRequestAtomicRetirement, not merely change the source hash"
    );
}

#[tokio::test]
async fn reclaim_equality_and_removed_owner_preserve_failed_cas_atomicity() {
    let fixture = Fixture::new().await;
    let old = fixture.claimed(0).await;
    let before = snapshot(&fixture.store).await;
    assert!(matches!(
        fixture
            .store
            .claim_job(fixture.claim(0, old.revision, old.lease_expires_ms - 1, 1))
            .await
            .expect("before expiry"),
        ClaimOutcome::Contended
    ));
    assert_eq!(snapshot(&fixture.store).await, before);
    let request = fixture.claim(0, old.revision, old.lease_expires_ms, 1);
    let ClaimOutcome::Claimed { job } = fixture
        .store
        .claim_job(request)
        .await
        .expect("equal expiry reclaim")
    else {
        panic!("expiry equality must be reclaimable");
    };
    assert_eq!(job.failed_attempts, 1);
    assert_eq!(job.fence, old.fence + 1);
    let current = job.token.expect("successor");
    sql(&fixture.store, "INSERT INTO settings(key,value,updated_at) VALUES('internal.cluster_job_owner_removed.node-a','true',json_extract($1,'$.now'))",
        json!({"now": NOW})).await;
    let before = snapshot(&fixture.store).await;
    assert!(!fixture
        .store
        .settle_job(yielding(current, old.lease_expires_ms + 1))
        .await
        .expect("removed owner"));
    assert_eq!(snapshot(&fixture.store).await, before);
}

// D is a bounded set of protocol inputs, not a quotient of all production
// histories. All numeric fields and captured token values remain exact.
const STATE_CAP: usize = 20_000;
const ATTEMPTS_PER_JOB: u8 = 2;
const DOMAIN_OWNERS: [&str; 3] = ["node-a", "node-b", "foreign-holder"];

#[derive(Clone, Debug, Serialize)]
pub(super) struct Episode {
    pub(super) now: i64,
    pub(super) claims: [u8; 2],
    pub(super) claim_snapshots: Vec<ClaimSnapshot>,
    pub(super) tokens: [Vec<JobToken>; 2],
    pub(super) renewed_claims: Vec<String>,
    pub(super) leases: Vec<Lease>,
    pub(super) domain_acquired: [bool; 3],
    pub(super) removed: [bool; 2],
}

impl Episode {
    fn initial() -> Self {
        Self {
            now: NOW,
            claims: [0; 2],
            claim_snapshots: vec![],
            tokens: [vec![], vec![]],
            renewed_claims: vec![],
            leases: vec![],
            domain_acquired: [false; 3],
            removed: [false; 2],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClaimSnapshot {
    pub(super) job: usize,
    pub(super) raw_job_id: String,
    pub(super) failed_attempts: i64,
    pub(super) claim_id: String,
    pub(super) token_capture: JobToken,
}

impl std::hash::Hash for ClaimSnapshot {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.job.hash(state);
        self.raw_job_id.hash(state);
        self.failed_attempts.hash(state);
        self.claim_id.hash(state);
        let JobToken {
            job_id,
            node_id,
            boot_id,
            claim_id,
            fence,
            revision,
            lease_expires_ms,
        } = &self.token_capture;
        (
            job_id,
            node_id,
            boot_id,
            claim_id,
            fence,
            revision,
            lease_expires_ms,
        )
            .hash(state);
    }
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

// Exact logical initial-claim query. Keep its original dispatch identity even
// when the selected wall sample advances; this is not renewal resolution.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InitialClaimResolution {
    pub(super) job_id: String,
    pub(super) node_id: String,
    pub(super) boot_id: String,
    pub(super) claim_id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(super) fence: Option<i64>,
    pub(super) dispatched_at_ms: i64,
    pub(super) now_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Action {
    Claim {
        job: usize,
        node: usize,
    },
    ResolveInitialClaim {
        job: usize,
        request: InitialClaimResolution,
    },
    ReadClaimJob {
        job: usize,
    },
    Renew {
        job: usize,
        token: usize,
    },
    Settle {
        job: usize,
        token: usize,
        disposition: usize,
    },
    OrdinarySettle {
        job: usize,
        token: usize,
        kind: String,
        not_before_ms: i64,
        request_now_ms: i64,
        #[serde(deserialize_with = "required_nullable")]
        error_code: Option<String>,
        #[serde(deserialize_with = "required_nullable")]
        checkpoint: Option<Value>,
    },
    Resolve {
        job: usize,
        token: usize,
    },
    Bind {
        job: usize,
        token: usize,
        lease: usize,
    },
    Complete {
        job: usize,
        token: usize,
        lease: usize,
    },
    AcquireDomain {
        node: usize,
    },
    ReleaseDomain {
        lease: usize,
    },
    RemoveOwner {
        node: usize,
    },
    ClockAdvance {
        now: i64,
    },
}

async fn domain_row(fixture: &Fixture) -> Option<Value> {
    let rows = fixture.store.queue_sql("SELECT json_object('resource',resource,'owner',owner_node_id,'fence',fence,'revision',revision,'expiry',expires_at_ms) FROM job_leases WHERE resource=json_extract($1,'$.resource')".into(),
        json!({"resource": format!("scan:library:{}", fixture.library)}).to_string(), false, true)
        .await.expect("authoritative domain row");
    rows.first()
        .map(|row| serde_json::from_str(row).expect("domain row JSON"))
}

async fn next_boundary(fixture: &Fixture, episode: &Episode) -> Option<i64> {
    let mut boundaries = Vec::new();
    for tokens in &episode.tokens {
        for token in tokens {
            boundaries.push(token.lease_expires_ms);
        }
    }
    for lease in &episode.leases {
        boundaries.push(lease.expires_at_unix_ms);
    }
    if let Some(row) = domain_row(fixture).await {
        boundaries.push(row["expiry"].as_i64().expect("domain expiry"));
    }
    // Exact neighbours preserve equality and strictness; there is no rank
    // normalisation, truncation, or collapsed difference between timestamps.
    boundaries
        .into_iter()
        .flat_map(|time| [time.checked_sub(1), Some(time), time.checked_add(1)])
        .flatten()
        .filter(|time| *time > episode.now)
        .min()
}

async fn actions(fixture: &Fixture, episode: &Episode) -> Vec<Action> {
    let mut result = Vec::new();
    let mut owned_nodes = [false; 2];
    for job in 0..2 {
        let durable = fixture
            .store
            .background_job(&fixture.jobs[job])
            .await
            .expect("protocol input ownership")
            .expect("selected job");
        if durable.state == JobState::Running {
            if let Some(token) = &durable.token {
                if let Some(node) = NODES.iter().position(|node| *node == token.node_id) {
                    owned_nodes[node] = true;
                }
            }
        }
        if episode.claims[job] < ATTEMPTS_PER_JOB {
            for node in 0..2 {
                result.push(Action::Claim { job, node });
            }
        }
        // The ordinary adapter holds the queue-token Mutex across renewal
        // and dispatch. Its next operation therefore captures the latest
        // revision of each claim. Historical tokens stay in the state;
        // stale-revision API inputs are tested separately as conformance.
        for (token, captured) in episode.tokens[job].iter().enumerate() {
            // Retirement carries its original token even when a renewal's
            // application/observation changed which revision is current.
            result.push(Action::Settle {
                job,
                token,
                disposition: 3,
            });
            result.push(Action::Resolve { job, token });
            result.push(Action::Settle {
                job,
                token,
                disposition: 2,
            });
            if episode.tokens[job][token + 1..]
                .iter()
                .any(|later| later.claim_id == captured.claim_id)
            {
                continue;
            }
            if !episode.renewed_claims.contains(&captured.claim_id) {
                result.push(Action::Renew { job, token });
            }
            for (lease, domain) in episode.leases.iter().enumerate() {
                // The driver dispatches its acquired node-local lease;
                // wrong-owner API inputs remain separate conformance cases.
                if domain.owner_node_id == captured.node_id {
                    result.push(Action::Bind { job, token, lease });
                    result.push(Action::Complete { job, token, lease });
                }
            }
        }
    }
    for (node, owns_queue) in owned_nodes.into_iter().chain([true]).enumerate() {
        // Expired-but-retained queue ownership is deliberately included:
        // dispatch may precede expiry and backend application may follow it.
        if !episode.domain_acquired[node] && owns_queue {
            result.push(Action::AcquireDomain { node });
        }
    }
    for lease in 0..episode.leases.len() {
        result.push(Action::ReleaseDomain { lease });
    }
    for node in 0..2 {
        if !episode.removed[node] {
            result.push(Action::RemoveOwner { node });
        }
    }
    if let Some(now) = next_boundary(fixture, episode).await {
        result.push(Action::ClockAdvance { now });
    }
    result
}

fn remember(tokens: &mut Vec<JobToken>, token: JobToken) -> usize {
    if let Some(index) = tokens.iter().position(|captured| *captured == token) {
        return index;
    }
    let index = tokens.len();
    tokens.push(token);
    index
}

fn domain_matches(row: Option<&Value>, lease: &Lease, now: i64) -> bool {
    row.is_some_and(|row| {
        row["resource"] == lease.resource
            && row["owner"] == lease.owner_node_id
            && row["fence"].as_u64() == Some(lease.fence)
            && row["revision"].as_u64() == Some(lease.revision)
            && row["expiry"] == lease.expires_at_unix_ms
            && lease.expires_at_unix_ms > now
    })
}

async fn apply_graph(
    fixture: &Fixture,
    episode: &mut Episode,
    action: &Action,
) -> Result<(String, Option<usize>), String> {
    let applied = apply(fixture, episode, action, false).await?;
    Ok((applied.result, applied.resolved_token))
}

#[derive(Clone, Debug, PartialEq)]
struct Applied {
    result: String,
    resolved_token: Option<usize>,
    claim_request: Option<Value>,
    job_read: Option<Value>,
}

async fn apply(
    fixture: &Fixture,
    episode: &mut Episode,
    action: &Action,
    observe_refusals: bool,
) -> Result<Applied, String> {
    let before_rows = if observes_rows(action, observe_refusals) {
        Some(snapshot(&fixture.store).await)
    } else {
        None
    };
    let applied = apply_primitive(fixture, episode, action).await?;
    if read_only(action) || (observe_refusals && refused(&applied.result)) {
        compare_rows(
            action,
            before_rows.as_ref().expect("guarded primitive preimage"),
            &snapshot(&fixture.store).await,
        )?;
    }
    Ok(applied)
}

fn read_only(action: &Action) -> bool {
    matches!(
        action,
        Action::Resolve { .. } | Action::ResolveInitialClaim { .. } | Action::ReadClaimJob { .. }
    )
}

fn refused(result: &str) -> bool {
    matches!(
        result,
        "refused"
            | "cancelled"
            | "expired_or_pruned"
            | "held"
            | "validation_error_yield_time"
            | "validation_error_retry_time"
    )
}

fn observes_rows(action: &Action, observe_refusals: bool) -> bool {
    read_only(action)
        || (observe_refusals
            && !matches!(
                action,
                Action::RemoveOwner { .. } | Action::ClockAdvance { .. }
            ))
}

fn compare_rows(action: &Action, before: &Value, after: &Value) -> Result<(), String> {
    if before != after {
        return Err(if read_only(action) {
            "ResolutionReadOnly: logical resolution changed durable rows".into()
        } else {
            "FailedCasAtomicity: refused primitive changed durable rows".into()
        });
    }
    Ok(())
}

// Only observation ownership changes: every branch below still invokes the
// original production Store API with the original exact request and Episode.
async fn apply_primitive(
    fixture: &Fixture,
    episode: &mut Episode,
    action: &Action,
) -> Result<Applied, String> {
    let now = episode.now;
    let mut resolved_token = None;
    let mut claim_request = None;
    let mut job_read = None;
    let result = match action.clone() {
        Action::Claim { job, node } => {
            let before = fixture
                .store
                .background_job(&fixture.jobs[job])
                .await
                .expect("claim preimage")
                .expect("job");
            let mut request = fixture.claim(job, before.revision, now, episode.claims[job]);
            request.node_id = NODES[node].into();
            request.boot_id = BOOTS[node].into();
            claim_request = Some(serde_json::to_value(&request).expect("exact actual ClaimJob"));
            match fixture
                .store
                .claim_job(request)
                .await
                .expect("actual CLAIM_SQL")
            {
                ClaimOutcome::Claimed { job: claimed } => {
                    if claimed.fence <= before.fence {
                        return Err("MonotoneFence: fresh claim did not advance fence".into());
                    }
                    let returned = claimed.token.clone().expect("claimed token");
                    episode.claim_snapshots.push(ClaimSnapshot {
                        job,
                        raw_job_id: claimed.id.clone(),
                        failed_attempts: claimed.failed_attempts,
                        claim_id: returned.claim_id.clone(),
                        token_capture: returned.clone(),
                    });
                    remember(&mut episode.tokens[job], returned);
                    episode.claims[job] += 1;
                    "claimed"
                }
                ClaimOutcome::Contended => "refused",
                ClaimOutcome::Cancelled => "cancelled",
                ClaimOutcome::ExpiredOrPruned => "expired_or_pruned",
                other => return Err(format!("UnexpectedClaimOutcome: {other:?}")),
            }
        }
        Action::ResolveInitialClaim { job, request } => {
            if request.job_id != fixture.jobs[job]
                || request.fence.is_some()
                || request.now_ms != now
            {
                return Err(
                    "InitialClaimResolutionInput: exact original identity/current wall required"
                        .into(),
                );
            }
            let outcome = fixture
                .store
                .resolve_claim(ResolveClaim {
                    job_id: request.job_id.clone(),
                    node_id: request.node_id.clone(),
                    boot_id: request.boot_id.clone(),
                    claim_id: request.claim_id.clone(),
                    fence: None,
                    dispatched_at_ms: request.dispatched_at_ms,
                    now_ms: request.now_ms,
                })
                .await
                .map_err(|error| format!("InitialClaimResolution: {error}"))?;
            let running = matches!(&outcome, ClaimResolution::Running { .. });
            match outcome {
                ClaimResolution::Running { token }
                | ClaimResolution::CancelRequested {
                    cleanup_token: token,
                } => {
                    if token.job_id != request.job_id
                        || token.node_id != request.node_id
                        || token.boot_id != request.boot_id
                        || token.claim_id != request.claim_id
                    {
                        return Err(
                            "ExactInitialIdentityResolution: resolver adopted another attempt"
                                .into(),
                        );
                    }
                    resolved_token = Some(remember(&mut episode.tokens[job], token));
                    if running {
                        "running"
                    } else {
                        "cancel_requested"
                    }
                }
                ClaimResolution::Settled { .. } => "settled",
                ClaimResolution::LostOwnership => "lost_ownership",
                ClaimResolution::ExpiredOrPruned => "expired_or_pruned",
            }
        }
        Action::ReadClaimJob { job } => {
            let row = fixture
                .store
                .background_job(&fixture.jobs[job])
                .await
                .map_err(|error| format!("InitialClaimRead: {error}"))?;
            job_read = row.map(|row| serde_json::to_value(row).expect("actual full BackgroundJob"));
            if job_read.is_some() {
                "present"
            } else {
                "missing"
            }
        }
        Action::Renew { job, token } => {
            let captured = episode.tokens[job][token].clone();
            let before = fixture
                .store
                .background_job(&fixture.jobs[job])
                .await
                .expect("renew preimage")
                .expect("job");
            let outcomes = fixture
                .store
                .renew_jobs(RenewJobs {
                    tokens: vec![captured.clone()],
                    now_ms: now,
                })
                .await
                .expect("actual RENEW_SQL");
            match outcomes.as_slice() {
                [RenewOutcome::Renewed { token }] => {
                    if before.token.as_ref() != Some(&captured)
                        || captured.lease_expires_ms <= now
                        || NODES
                            .iter()
                            .position(|node| *node == captured.node_id)
                            .is_some_and(|node| episode.removed[node])
                    {
                        return Err("NoStaleCommit: stale queue renewal accepted".into());
                    }
                    episode.renewed_claims.push(captured.claim_id);
                    episode.renewed_claims.sort();
                    remember(&mut episode.tokens[job], token.clone());
                    "renewed"
                }
                [RenewOutcome::LostOwnership { .. }] => "refused",
                _ => return Err("RenewalCardinality: one input must have one verdict".into()),
            }
        }
        Action::OrdinarySettle {
            job,
            token,
            kind,
            not_before_ms,
            request_now_ms,
            error_code,
            checkpoint,
        } => {
            let captured = episode.tokens[job][token].clone();
            let preimage = fixture
                .store
                .background_job(&fixture.jobs[job])
                .await
                .map_err(|error| format!("OrdinaryPreimage: {error}"))?
                .ok_or("OrdinaryPreimage: missing selected job")?;
            let settlement = match kind.as_str() {
                "yield" if error_code.is_none() && checkpoint.is_none() => JobSettlement::Yield {
                    error_code,
                    checkpoint,
                    not_before_ms,
                },
                "retry"
                    if error_code.as_deref() == Some("library_execution_failed")
                        && checkpoint.is_none() =>
                {
                    JobSettlement::Retry {
                        error_code: error_code.expect("checked error"),
                        not_before_ms,
                    }
                }
                _ => return Err("OrdinaryInput: unsupported adapter payload".into()),
            };
            match fixture
                .store
                .settle_job(SettleJob {
                    token: captured.clone(),
                    settlement,
                    now_ms: request_now_ms,
                })
                .await
            {
                Ok(true) => {
                    if preimage.token.as_ref() != Some(&captured)
                        || captured.lease_expires_ms <= request_now_ms
                        || NODES
                            .iter()
                            .position(|node| *node == captured.node_id)
                            .is_some_and(|node| episode.removed[node])
                    {
                        return Err("NoStaleCommit: ordinary settlement accepted an invalid captured request-time fence".into());
                    }
                    "accepted"
                }
                Ok(false) => "refused",
                Err(StoreError::Task(message))
                    if kind == "yield"
                        && message == "invalid background checkpoint or yield time" =>
                {
                    "validation_error_yield_time"
                }
                Err(StoreError::Task(message))
                    if kind == "retry" && message == "invalid background failure retry" =>
                {
                    "validation_error_retry_time"
                }
                Err(error) => return Err(format!("OrdinaryUnexpectedStoreError: {error}")),
            }
        }
        Action::Settle {
            job,
            token,
            disposition,
        } => {
            let captured = episode.tokens[job][token].clone();
            let before = fixture
                .store
                .background_job(&fixture.jobs[job])
                .await
                .expect("settle preimage")
                .expect("job");
            // Dispositions 0/1 remain only for legacy primitive conformance.
            // The selected product has no route to them: ordinary service uses
            // the full captured OrdinarySettle request above.
            let settlement = match disposition {
                0 => JobSettlement::Yield {
                    error_code: None,
                    checkpoint: None,
                    not_before_ms: now,
                },
                1 => JobSettlement::Retry {
                    error_code: "transient".into(),
                    not_before_ms: now,
                },
                2 => JobSettlement::Cancel,
                3 => JobSettlement::Yield {
                    error_code: Some("worker_retired".into()),
                    checkpoint: None,
                    not_before_ms: now,
                },
                _ => return Err("ProtocolInputDomain: unknown library settlement tag".into()),
            };
            if fixture
                .store
                .settle_job(SettleJob {
                    token: captured.clone(),
                    settlement,
                    now_ms: now,
                })
                .await
                .expect("actual SETTLE_SQL")
            {
                if before.token.as_ref() != Some(&captured)
                    || captured.lease_expires_ms <= now
                    || NODES
                        .iter()
                        .position(|node| *node == captured.node_id)
                        .is_some_and(|node| episode.removed[node])
                {
                    return Err("NoStaleCommit: stale queue settlement accepted".into());
                }
                "accepted"
            } else {
                "refused"
            }
        }
        Action::Resolve { job, token } => {
            let original = episode.tokens[job][token].clone();
            // This is the real logical API. Its two internal authoritative
            // awaits are an explicit read-conformance boundary, not atomic
            // SQL or a claim to closed internal read-interleaving coverage.
            let outcome = fixture
                .store
                .resolve_claim(ResolveClaim {
                    job_id: original.job_id.clone(),
                    node_id: original.node_id.clone(),
                    boot_id: original.boot_id.clone(),
                    claim_id: original.claim_id.clone(),
                    fence: Some(original.fence),
                    dispatched_at_ms: now,
                    now_ms: now,
                })
                .await
                .expect("actual logical resolve_claim API");
            match outcome {
                ClaimResolution::Running { token } => {
                    if token.job_id != original.job_id
                        || token.node_id != original.node_id
                        || token.boot_id != original.boot_id
                        || token.claim_id != original.claim_id
                        || token.fence != original.fence
                    {
                        return Err(
                            "ExactIdentityResolution: resolver adopted another attempt".into()
                        );
                    }
                    resolved_token = Some(remember(&mut episode.tokens[job], token));
                    "running"
                }
                ClaimResolution::CancelRequested {
                    cleanup_token: token,
                } => {
                    if token.job_id != original.job_id
                        || token.node_id != original.node_id
                        || token.boot_id != original.boot_id
                        || token.claim_id != original.claim_id
                        || token.fence != original.fence
                    {
                        return Err("ExactIdentityResolution: resolver adopted another cancellation attempt".into());
                    }
                    resolved_token = Some(remember(&mut episode.tokens[job], token));
                    "cancel_requested"
                }
                ClaimResolution::Settled { .. } => "settled",
                ClaimResolution::LostOwnership => "lost_ownership",
                ClaimResolution::ExpiredOrPruned => "expired_or_pruned",
            }
        }
        Action::Bind { job, token, lease } => {
            let captured = episode.tokens[job][token].clone();
            let lease = episode.leases[lease].clone();
            let before = fixture
                .store
                .background_job(&fixture.jobs[job])
                .await
                .expect("bind preimage")
                .expect("job");
            let domain = domain_row(fixture).await;
            if fixture
                .store
                .bind_library_job(BindLibraryJob {
                    token: captured.clone(),
                    lease: lease.clone(),
                    now_ms: now,
                })
                .await
                .expect("actual BIND_SQL")
            {
                if before.token.as_ref() != Some(&captured)
                    || captured.lease_expires_ms <= now
                    || NODES
                        .iter()
                        .position(|node| *node == captured.node_id)
                        .is_some_and(|node| episode.removed[node])
                    || !domain_matches(domain.as_ref(), &lease, now)
                    || lease.owner_node_id != captured.node_id
                {
                    return Err("NoStaleCommit: stale binding accepted".into());
                }
                "accepted"
            } else {
                "refused"
            }
        }
        Action::Complete { job, token, lease } => {
            let captured = episode.tokens[job][token].clone();
            let lease = episode.leases[lease].clone();
            let before = fixture
                .store
                .background_job(&fixture.jobs[job])
                .await
                .expect("complete preimage")
                .expect("job");
            let domain = domain_row(fixture).await;
            let mut request = fixture.completion(captured.clone(), lease.clone(), now);
            request.request_id = REQUESTS[job].into();
            if fixture
                .store
                .complete_library_work(request)
                .await
                .expect("actual COMPLETE_SQL")
            {
                if before.token.as_ref() != Some(&captured)
                    || captured.lease_expires_ms <= now
                    || NODES
                        .iter()
                        .position(|node| *node == captured.node_id)
                        .is_some_and(|node| episode.removed[node])
                    || !domain_matches(domain.as_ref(), &lease, now)
                    || lease.owner_node_id != captured.node_id
                {
                    return Err("NoStaleCommit: stale completion accepted".into());
                }
                "accepted"
            } else {
                "refused"
            }
        }
        Action::AcquireDomain { node } => {
            match fixture
                .store
                .acquire_lease(
                    &format!("scan:library:{}", fixture.library),
                    DOMAIN_OWNERS[node],
                    now,
                    now.checked_add(90_000)
                        .expect("concrete episode clock cannot overflow"),
                )
                .await
                .expect("actual backend domain acquire")
            {
                LeaseClaim::Acquired(lease) => {
                    episode.domain_acquired[node] = true;
                    episode.leases.push(lease);
                    "acquired"
                }
                LeaseClaim::Held { .. } => "held",
            }
        }
        Action::ReleaseDomain { lease } => {
            if fixture
                .store
                .release_lease(&episode.leases[lease], now)
                .await
                .expect("actual backend domain release")
            {
                "accepted"
            } else {
                "refused"
            }
        }
        Action::RemoveOwner { node } => {
            sql(&fixture.store, "INSERT INTO settings(key,value,updated_at) VALUES('internal.cluster_job_owner_removed.' || json_extract($1,'$.node'),'true',json_extract($1,'$.now'))",
                json!({"node": NODES[node], "now": now})).await;
            episode.removed[node] = true;
            "removed"
        }
        Action::ClockAdvance { now } => {
            episode.now = now;
            "advanced"
        }
    };
    Ok(Applied {
        result: result.into(),
        resolved_token,
        claim_request,
        job_read,
    })
}

// These omitted rows are constant for the selected library-only protocol.
// Observe them separately so the original nine-table canonical bytes stay exact.
async fn omitted_dv_state(store: &SqliteStore) -> Result<Value, String> {
    let rows = store.queue_sql(
        r#"SELECT json_object(
          'files',(SELECT count(*) FROM files),
          'demand',(SELECT count(*) FROM dv_title_jobs_demand),
          'marker',json((SELECT json_group_array(json_array(singleton,version))
            FROM (SELECT singleton,version FROM dv_title_jobs_schema ORDER BY singleton,version))),
          'sequence',json((SELECT json_group_array(json_array(name,seq))
            FROM (SELECT name,seq FROM sqlite_sequence WHERE name='dv_title_jobs_demand' ORDER BY name,seq)))
        ) WHERE json_valid($1)"#.into(),
        "null".into(), false, true,
    ).await.map_err(|error| format!("OmittedDvState: observation failed: {error}"))?;
    let [row] = rows.as_slice() else {
        return Err("OmittedDvState: expected one observation".into());
    };
    serde_json::from_str(row)
        .map_err(|error| format!("OmittedDvState: invalid observation: {error}"))
}

async fn omitted_dv_invariants(fixture: &Fixture) -> Result<(), String> {
    check_omitted_dv(fixture, &omitted_dv_state(&fixture.store).await?)
}

fn check_omitted_dv(fixture: &Fixture, state: &Value) -> Result<(), String> {
    if state["files"] != json!(0)
        || state["demand"] != json!(0)
        || state["marker"] != json!([[1, 1]])
        || !state["sequence"].is_array()
        || state["sequence"] != fixture.omitted_dv_sequence
    {
        return Err("OmittedDvState: selected protocol changed fixed DV rows or sequence".into());
    }
    Ok(())
}

async fn invariants(fixture: &Fixture, episode: &Episode) -> Result<(), String> {
    omitted_dv_invariants(fixture).await?;
    let mut active = 0;
    for job in 0..2 {
        let row = fixture
            .store
            .background_job(&fixture.jobs[job])
            .await
            .expect("invariant job")
            .expect("job");
        let held = reservations(&fixture.store, &fixture.jobs[job]).await;
        let held = held.as_array().expect("reservation array");
        if let Some(token) = &row.token {
            if row.state == JobState::Running && token.lease_expires_ms > episode.now {
                active += 1;
            }
            let resources = held
                .iter()
                .map(|row| row["resource"].as_str().expect("resource"))
                .collect::<Vec<_>>();
            if token.lease_expires_ms > episode.now
                && (resources != ["provider:anilist", "provider:tmdb", "source_io:shared-root"]
                    || held.iter().any(|row| {
                        row["fence"] != token.fence
                            || row["expiry"]
                                .as_i64()
                                .expect("integer in typed JSON projection")
                                < token.lease_expires_ms
                    }))
            {
                return Err(format!("ReservationConservation: running job {job} lacks its complete current resources"));
            }
        } else if !held.is_empty() {
            return Err(format!(
                "ReservationConservation: retired job {job} retained reservations"
            ));
        }
        let requests = fixture
            .store
            .library_work_requests(LibraryWorkQuery {
                job_id: Some(fixture.jobs[job].clone()),
                limit: 2,
                ..Default::default()
            })
            .await
            .expect("invariant paired requests");
        if requests.len() != 1 {
            return Err("RequestPairing: episode requires one request per selected job".into());
        }
        if requests[0].result.is_some()
            && (row.state != JobState::Succeeded || row.token.is_some() || !held.is_empty())
        {
            return Err(format!("LastRequestAtomicRetirement: completed request {job} did not retire queue/resources"));
        }
        let attempts = fixture
            .store
            .job_attempts(&fixture.jobs[job])
            .await
            .expect("invariant attempts");
        let mut by_fence = std::collections::BTreeSet::new();
        for attempt in attempts {
            if !by_fence.insert(attempt.fence) {
                return Err("SingleOwnerPerFence: duplicate attempt fence".into());
            }
        }
    }
    if active > 1 {
        return Err("NoTwoSourceReaders: same-root non-live claims overlap".into());
    }
    Ok(())
}

/// One actual sequential writer boundary, never a database restoration image.
/// The complete nine-table rows and exact Episode determine canonical identity;
/// omitted DV rows/sequence are independently observed and checked each time.
/// No SQL action or mutable Episode change may occur while carrying this value.
struct BoundaryObservation {
    rows: Value,
    episode: Episode,
    omitted_dv: Value,
    canonical: String,
}

impl BoundaryObservation {
    async fn observe(fixture: &Fixture, episode: &Episode) -> Result<Self, String> {
        let rows = snapshot(&fixture.store).await;
        let omitted_dv = omitted_dv_state(&fixture.store).await?;
        check_omitted_dv(fixture, &omitted_dv)?;
        let canonical = serde_json::to_string(&json!({"rows": rows, "episode": episode}))
            .expect("canonical JSON");
        Ok(Self {
            rows,
            episode: episode.clone(),
            omitted_dv,
            canonical,
        })
    }

    fn compare_action(
        &self,
        action: &Action,
        applied: &Applied,
        after: &Self,
    ) -> Result<(), String> {
        if read_only(action) || refused(&applied.result) {
            compare_rows(action, &self.rows, &after.rows)?;
        }
        if refused(&applied.result) && self.canonical != after.canonical {
            return Err(
                "FailedCasAtomicity: refused operation changed durable rows or captured state"
                    .into(),
            );
        }
        if self.omitted_dv != after.omitted_dv {
            return Err("OmittedDvState: operation changed separately observed DV state".into());
        }
        Ok(())
    }
}

async fn canonical(fixture: &Fixture, episode: &Episode) -> String {
    // Raw production UUID bytes remain part of identity: retry jitter reads them.
    serde_json::to_string(&json!({"rows": snapshot(&fixture.store).await, "episode": episode}))
        .expect("canonical JSON")
}

async fn projection(fixture: &Fixture, episode: &Episode, id: usize, key: &str) -> Value {
    let mut jobs = Vec::new();
    let mut cutoffs = Vec::new();
    for job in 0..2 {
        let row = fixture
            .store
            .background_job(&fixture.jobs[job])
            .await
            .expect("projection")
            .expect("job");
        let requests = fixture
            .store
            .library_work_requests(LibraryWorkQuery {
                job_id: Some(fixture.jobs[job].clone()),
                limit: 1,
                ..Default::default()
            })
            .await
            .expect("projection requests");
        jobs.push(json!({"state": row.state, "owner": row.token.as_ref().map(|token| &token.node_id),
            "fence": row.fence, "revision": row.revision, "expiry": row.token.as_ref().map(|token| token.lease_expires_ms),
            "failed_attempts": row.failed_attempts, "not_before_ms": row.not_before_ms, "active": row.token.as_ref().is_some_and(|token| row.state == JobState::Running && token.lease_expires_ms > episode.now),
            "request_done": requests[0].result.is_some(), "request_state": requests[0].state,
            "reservation_count": reservations(&fixture.store, &fixture.jobs[job]).await.as_array().expect("array in typed JSON projection").len()}));
        if episode.claims[job] == ATTEMPTS_PER_JOB {
            cutoffs.push(format!("attempt_budget_j{job}"));
        }
    }
    for (owner, acquired) in DOMAIN_OWNERS.iter().zip(episode.domain_acquired) {
        if acquired {
            cutoffs.push(format!("domain_acquire_budget_{owner}"));
        }
    }
    if episode.domain_acquired.iter().all(|used| *used) {
        cutoffs.push("domain_acquire_budget".into());
    }
    for claim in &episode.renewed_claims {
        cutoffs.push(format!("renewal_budget_{claim}"));
    }
    if next_boundary(fixture, episode).await.is_none() {
        cutoffs.push("no_intrinsic_expiry_boundary".into());
    }
    json!({"id": id, "now": episode.now, "jobs": jobs, "domain": domain_row(fixture).await,
        "captured_tokens": episode.tokens, "captured_leases": episode.leases,
        "budgets": {"claims": episode.claims, "renewed_claims": episode.renewed_claims,
            "domain_acquired": episode.domain_acquired, "removed": episode.removed},
        "cutoffs": cutoffs, "canonical_sha256": format!("{:x}", Sha256::digest(key.as_bytes()))})
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(super) enum TriggerMutation {
    CompletionRetirement,
    ReservationRelease,
}

async fn install_mutation(fixture: &Fixture, mutation: TriggerMutation) -> Value {
    let (schema, name) = match mutation {
        TriggerMutation::CompletionRetirement => (
            super::background_jobs_library::SCHEMA,
            "background_library_request_completed",
        ),
        TriggerMutation::ReservationRelease => (
            super::background_jobs::SCHEMA,
            "background_job_settlement_effects",
        ),
    };
    let trigger = schema
        .split("-- next statement")
        .find(|statement| statement.contains(&format!("CREATE TRIGGER IF NOT EXISTS {name}")))
        .expect("production mutation source trigger");
    let mutated = match mutation {
        TriggerMutation::CompletionRetirement => {
            let start = trigger.find("    UPDATE background_jobs SET state = 'succeeded'").expect("source retirement clause");
            format!("{}\nEND;", &trigger[..start])
        }
        TriggerMutation::ReservationRelease => trigger.replace(
            "    DELETE FROM background_job_reservations WHERE job_id = NEW.id AND fence = OLD.fence;", ""),
    };
    assert_ne!(
        mutated, trigger,
        "mutation must change the named source clause"
    );
    fixture
        .store
        .async_refinement_control(format!("DROP TRIGGER {name};\n{mutated}"))
        .await
        .expect("source-derived trigger mutation");
    json!({"kind": match mutation { TriggerMutation::CompletionRetirement => "completion_retirement", TriggerMutation::ReservationRelease => "reservation_release" },
        "trigger": name, "source_sha256": format!("{:x}", Sha256::digest(schema.as_bytes())),
        "mutated_trigger_sha256": format!("{:x}", Sha256::digest(mutated.as_bytes()))})
}

fn source_bindings() -> BTreeMap<&'static str, String> {
    [
        (
            "store/background_jobs.rs",
            include_str!("background_jobs.rs"),
        ),
        (
            "store/background_jobs_domain.rs",
            include_str!("background_jobs_domain.rs"),
        ),
        (
            "store/background_jobs_library.rs",
            include_str!("background_jobs_library.rs"),
        ),
        (
            "store/background_jobs_schema.sql",
            include_str!("background_jobs_schema.sql"),
        ),
        (
            "store/background_jobs_resources.sql",
            include_str!("background_jobs_resources.sql"),
        ),
        (
            "store/background_jobs_library.sql",
            include_str!("background_jobs_library.sql"),
        ),
        (
            "store/background_jobs_domain.sql",
            include_str!("background_jobs_domain.sql"),
        ),
        (
            "store/subtitle_managed_schema.sql",
            include_str!("subtitle_managed_schema.sql"),
        ),
        ("store/dv_title_jobs.rs", include_str!("dv_title_jobs.rs")),
        ("store/dv_title_jobs.sql", include_str!("dv_title_jobs.sql")),
        (
            "store/dv_title_jobs_schema.rs",
            include_str!("dv_title_jobs_schema.rs"),
        ),
        ("store/sqlite/mod.rs", include_str!("sqlite/mod.rs")),
        (
            "store/sqlite/background_jobs.rs",
            include_str!("sqlite/background_jobs.rs"),
        ),
        (
            "store/sqlite/coordination.rs",
            include_str!("sqlite/coordination.rs"),
        ),
        (
            "store/async_driver_refinement.rs",
            include_str!("async_driver_refinement.rs"),
        ),
        (
            "cluster/coordination.rs",
            include_str!("../cluster/coordination.rs"),
        ),
    ]
    .into_iter()
    .map(|(path, source)| (path, format!("{:x}", Sha256::digest(source.as_bytes()))))
    .collect()
}

async fn schema_bindings(fixture: &Fixture) -> Value {
    // Hash actual compiled migration strings, including every transitive SQL
    // include and Rust schema constant, rather than only their include sites.
    let migrations = super::sqlite::MIGRATIONS.iter().enumerate().map(|(index, sql)|
        json!({"version": index + 1, "sha256": format!("{:x}", Sha256::digest(sql.as_bytes()))}))
        .collect::<Vec<_>>();
    let rows = fixture.store.queue_sql("SELECT json_group_array(json_array(type,name,tbl_name,sql)) FROM (SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name,tbl_name,sql) WHERE json_valid($1)".into(),
        "null".into(), false, true).await.expect("installed production schema inventory");
    json!({"compiled_sqlite_migrations": migrations,
        "installed_sqlite_schema_sha256": format!("{:x}", Sha256::digest(rows[0].as_bytes())),
        "schema_inventory": serde_json::from_str::<Value>(&rows[0]).expect("installed schema JSON")})
}

// Failure-only telemetry: no raw rows, opaque strings, or unbounded trace.
const CAP_DIAGNOSTIC_TRACE_ACTIONS: usize = 8;
const CAP_DIAGNOSTIC_ACTION_BYTES: usize = 256;
const CAP_DIAGNOSTIC_MAX_BYTES: usize = 8_192;

struct CapDiagnostic<'a> {
    counts: [usize; 4], // retained states, recorded edges, pending frontier, expanded states
    from: usize,
    trace: &'a [Action],
    predecessor: (&'a Episode, &'a str),
    rejected: (&'a Episode, &'a str, &'a Action),
}

pub(super) fn diagnostic_action(encoded: String) -> Value {
    let bytes = encoded.len();
    json!({
        "sha256": format!("{:x}", Sha256::digest(encoded.as_bytes())),
        "encoded_bytes": bytes,
        "action": if bytes <= CAP_DIAGNOSTIC_ACTION_BYTES {
            Some(serde_json::from_str::<Value>(&encoded).expect("serialized action JSON"))
        } else { None },
    })
}

fn state_cap_diagnostic(input: &CapDiagnostic<'_>) -> String {
    fn dimensions(episode: &Episode) -> Value {
        json!({
            "now": episode.now, "claims": episode.claims,
            "captured_tokens": episode.tokens.each_ref().map(Vec::len),
            "renewed_claims": episode.renewed_claims.len(),
            "captured_leases": episode.leases.len(),
            "domain_acquired": episode.domain_acquired, "removed": episode.removed,
        })
    }
    let start = input
        .trace
        .len()
        .saturating_sub(CAP_DIAGNOSTIC_TRACE_ACTIONS);
    let tail: Vec<Value> = input.trace[start..]
        .iter()
        .map(|action| diagnostic_action(serde_json::to_string(action).expect("action JSON")))
        .collect();
    let record = json!({
        "result_class": "domain_state_cap_diagnostic", "complete": false,
        "state_cap": STATE_CAP,
        "retained_state_count": input.counts[0], "recorded_edge_count": input.counts[1],
        "pending_frontier_count": input.counts[2], "fully_expanded_state_count": input.counts[3],
        "predecessor_id": input.from,
        "predecessor_sha256": format!("{:x}", Sha256::digest(input.predecessor.1.as_bytes())),
        "rejected_successor_sha256": format!("{:x}", Sha256::digest(input.rejected.1.as_bytes())),
        "rejected_action": diagnostic_action(serde_json::to_string(input.rejected.2).expect("action JSON")),
        "predecessor_depth": input.trace.len(), "rejected_depth": input.trace.len() + 1,
        "trace_tail_start": start, "trace_complete": start == 0,
        "trace_scope": "predecessor trace tail; rejected action is separate",
        "trace_tail": tail,
        "dimensions": {"predecessor": dimensions(input.predecessor.0), "rejected": dimensions(input.rejected.0)},
    });
    let encoded = serde_json::to_string(&record).expect("cap diagnostic JSON");
    // At most nine action records, each carrying <=256 action bytes plus a
    // digest/length, two fixed-shape numeric summaries, and fixed-size keys.
    // Oversized action payloads retain only their digest/length; rows never leak.
    debug_assert!(encoded.len() <= CAP_DIAGNOSTIC_MAX_BYTES);
    encoded
}

pub(super) fn domain_metadata() -> Value {
    let mut domain = json!({"jobs": 2, "nodes": NODES, "physical_capacity_per_node": 1,
            "attempts_per_job": ATTEMPTS_PER_JOB, "renewals_per_attempt": 1, "rearrival": false,
            "ordinary_token_inputs": "latest captured revision per claim for intrinsic renew/bind/complete; ordinary saved settlement captures are contextual and may be historical",
            "binding_completion_inputs": "captured domain lease owner equals captured queue-token node; wrong-owner inputs are separate conformance",
            "settlement_dispositions": ["cancel", "retirement_yield"],
            "node_domain_acquisition_inputs": "node has selected running durable queue ownership, including expired retained ownership for dispatch/application races; foreign holder is independent",
            "domain_acquisitions_per_owner": 1, "domain_owners": DOMAIN_OWNERS,
            "selected_attempt_ids": 4, "reserved_boundary_ids": 2,
            "allocator_exhaustion": "outside D: the attempt budget is exhausted before the six-ID namespace",
            "numeric_quotient": "none: every i64/u64 and raw production job ID remains exact",
            "clock": "wall and mono independently advance to the smallest strict greater nonnegative representable neighbour of retained exact authority/payload/queued-eligibility or source local-deadline anchors; no horizon clipping",
            "publication": "opaque conformance boundary; no item-insertion SQL or retry proof in this relation",
            "reply_faults": "split application/reply/reconciliation in temporal composition; separate bridge-boundary conformance histories",
            "cap": STATE_CAP,
            "premises": ["completion opportunity before attempt/domain/fault budget exhaustion", "two fixed selected requests; no infinite fresh-progress conclusion"],
            "savepoints": "same production writer connection, sequential harness; transaction/commit visibility and SQLite internals remain assumptions; any SQL/ABORT/ROLLBACK error aborts closure"});
    let boundary_metadata = json!({
        "renewal_resolution_boundary": "selected zero-delay renewal plus actual logical resolve has no inner interleaving; both actual outcomes and capture references are certified; resolution errors consume the remaining per-attempt fault allowance",
        "ordinary_settlement_payload_transfer": "selected actual ordinary settlement payload/capture/request/local-service correspondence; evidence pending qualification; normal wall/mono input domain only",
        "retirement_inputs": "disposition 3 uses Some(worker_retired), None checkpoint and not_before_ms=current episode.now for every captured original token; cancellation uses disposition 2; ordinary service uses contextual ordinary_settle payloads",
        "initial_claim_recovery_boundary": "lost initial claims use actual fence-None resolution with original dispatch identity, conditional actual row reread or Cancel, and no inner query interleaving; at most eight persistent job/node/claim loss keys; inconclusive outcomes service exact 250ms source wakes until the fixed 30000ms local deadline; the separate Store request window is 120000ms",
        "resolution_read_boundary": "resolve_claim is one logical actual-API pure-read action; its two authoritative QueueSql awaits and internal interleavings are a named conformance boundary, not closed read-interleaving coverage",
        "primitive_clock_boundary": "intrinsic primitives use current episode.now; ordinary_settle preserves its separately captured request_now_ms; retirement wall samples/internal dispatch races and zero-delay claim/renew-resolution envelopes remain named source/conformance boundaries",
        "reservation_conservation_boundary": "conservation is checked for unexpired queue authority only; expired Running rows and residue remain represented, with no independent complete-residue conservation guarantee until reclaim/prune",
        "domain_reacquisition_boundary": "each owner can acquire at most once; release or expiry does not reset its budget, so same-owner release-to-reacquire cycles are excluded",
        "foreign_holder_boundary": "foreign-holder is a third adversarial domain-lease owner, not either worker node, with one independent acquisition budget",
        "retirement_sequence_boundary": "the graph supplies separate actual settle and logical resolve primitives; daemon drain/local-token/first-settle/resolve/optional-second-settle sequencing is a reviewed source and daemon-conformance boundary composed by the temporal model",
        "cleanup_boundary": "local cleanup needs bounded retirement calls to return, not eventual successful acknowledgement; errors are logged and finish returns while durable authority may remain",
        "omitted_dv_state_boundary": "selected library_scan/metadata_refresh inputs preserve empty files and DV demand, marker [(1,1)] and exact initialized dv_title_jobs_demand sqlite_sequence rows; omitted-state drift aborts the whole run without changing nine-table canonical identity",
        "snapshot_tables": SNAPSHOT_TABLES,
        "dispatch_time_noninterference": "for retirement fence Some(F), resolve_claim reads dispatched_at_ms only in its skipped fence-is-none window branch; neither SQL query nor later classification reads it, so a representative dispatch sample preserves results at fixed now/read outcomes; this is a reviewed source lemma, not a machine proof",
        "queue_sql_boundary": "all generated queue primitives use QueueSql::queue_sql, not queue_transaction; ReplyBoundary passes queue_transaction through without fault injection, so multi-statement transaction faults are outside this slice",
        "schema_snapshot_boundary": "canonical durable rows cover exactly the nine snapshot_tables; all installed schema objects are hash-bound for identity, not complete row snapshots or proofs of unrelated tables",
        "savepoint_boundary": "no closure transition invokes queue_transaction or nested explicit transactions; test savepoints bracket sequential primitives on the production writer and any savepoint/SQL failure aborts closure",
        "refusal_observation": "the extracted oracle retains the original full-canonical refused-edge equality guard before memoization; logical resolve retains durable read-only observation; no pre-existing closure gap is claimed",
    });
    domain
        .as_object_mut()
        .expect("domain metadata object")
        .extend(
            boundary_metadata
                .as_object()
                .expect("boundary metadata object")
                .clone(),
        );
    domain
}

pub(super) type StoreHandle = usize;

#[derive(Clone, Debug, Serialize)]
pub(super) struct OracleEdge {
    pub(super) from: StoreHandle,
    pub(super) to: StoreHandle,
    pub(super) action: Action,
    pub(super) result: String,
    pub(super) resolved_token: Option<usize>,
    pub(super) claim_snapshot: Option<ClaimSnapshot>,
    pub(super) error: Option<String>,
    pub(super) claim_request: Option<Value>,
    pub(super) job_read: Option<Value>,
}

pub(super) struct OracleState {
    pub(super) canonical: String,
    pub(super) episode: Episode,
    pub(super) view: Value,
    parent: Option<(StoreHandle, Action)>,
    inputs: Vec<Action>,
    // Private to this Oracle's fixed fixture, installed schema and mutation.
    // Published only after independent invariants, domain and projection checks.
    certified: bool,
}

// A cache is bound to one migrated fixture/source/mutation identity for its
// entire lifetime. Keys compare full canonical bytes and exact input values.
// Host-only measurements: never exported as Store state, edge or identity.
#[derive(Default)]
struct OracleDiagnostics {
    hits: usize,
    misses: usize,
    replay_depth_total: usize,
    replay_depth_max: usize,
    replay_performed: usize,
    replay_saved: usize,
    replay: Duration,
    requested_apply: Duration,
    requested_checks: Duration,
    rollback: Duration,
    canonical: Duration,
    actions: Duration,
    invariants: Duration,
    controls: Duration,
    replayed_apply: Duration,
    observations: usize,
    certifications: usize,
    certificate_reuses: usize,
    replay_comparisons: usize,
    invalidations: usize,
}

// Fixed host-only totals. Await exactly once and preserve the original Result;
// these measurements never enter canonical state, edges, hashes or SQL inputs.
macro_rules! oracle_timed {
    ($diagnostics:expr, $field:ident, $future:expr) => {{
        let started = Instant::now();
        let value = $future.await;
        $diagnostics.$field = $diagnostics.$field.saturating_add(started.elapsed());
        value
    }};
}

pub(super) struct Oracle {
    fixture: Fixture,
    diagnostics: OracleDiagnostics,
    prefix: Vec<(Action, Episode)>,
    root_savepoint: bool,
    materialization_failed: bool,
    // Core-local reference fixture disables substitution; closure always enables it.
    reuse_certificates: bool,
    pub(super) states: Vec<OracleState>,
    pub(super) edges: Vec<OracleEdge>,
    index: HashMap<String, StoreHandle>,
    cache: HashMap<(StoreHandle, Action), usize>,
    pub(super) schema: Value,
    pub(super) failure: Option<Value>,
    pub(super) mutation: Value,
}

impl Oracle {
    pub(super) async fn new(mutation: Option<TriggerMutation>) -> Result<Self, String> {
        let fixture = Fixture::new().await;
        let mutation = if let Some(mutation) = mutation {
            install_mutation(&fixture, mutation).await
        } else {
            Value::Null
        };
        let episode = Episode::initial();
        invariants(&fixture, &episode).await?;
        let key = canonical(&fixture, &episode).await;
        let view = projection(&fixture, &episode, 0, &key).await;
        let inputs = actions(&fixture, &episode).await;
        let schema = schema_bindings(&fixture).await;
        Ok(Self {
            fixture,
            diagnostics: OracleDiagnostics::default(),
            prefix: Vec::new(),
            root_savepoint: false,
            materialization_failed: false,
            reuse_certificates: true,
            states: vec![OracleState {
                canonical: key.clone(),
                episode,
                view,
                parent: None,
                inputs,
                certified: true,
            }],
            edges: vec![],
            index: HashMap::from([(key, 0)]),
            cache: HashMap::new(),
            schema,
            failure: None,
            mutation,
        })
    }

    pub(super) async fn begin_paired_mutation(
        &mut self,
        mutation: TriggerMutation,
    ) -> Result<(), String> {
        self.restore_root().await?;
        for state in &mut self.states {
            state.certified = false;
        }
        self.diagnostics.invalidations = self.diagnostics.invalidations.saturating_add(1);
        self.mutation = install_mutation(&self.fixture, mutation).await;
        self.schema = schema_bindings(&self.fixture).await;
        let rebuilt = async {
            let observed = self.observe(&Episode::initial()).await?;
            self.check_endpoint(0, &observed).await
        }
        .await;
        if let Err(error) = rebuilt {
            self.materialization_failed = true;
            return Err(error);
        }
        self.states.truncate(1);
        self.edges.clear();
        self.cache.clear();
        self.index = HashMap::from([(self.states[0].canonical.clone(), 0)]);
        self.failure = None;
        Ok(())
    }

    pub(super) fn initial(&self) -> StoreHandle {
        0
    }

    pub(super) fn domain_inputs(&self, handle: StoreHandle) -> &[Action] {
        &self.states[handle].inputs
    }

    pub(super) fn normalize(&self, value: &Value) -> Value {
        // No recursive UUID replacement: raw identity is a semantic retry input.
        value.clone()
    }

    pub(super) fn fixture_identity(&self) -> Value {
        json!({"jobs": self.fixture.jobs.iter().enumerate().map(|(job,id)|json!({"job":job,"raw_job_id":id})).collect::<Vec<_>>()})
    }

    pub(super) fn export_state(&self, handle: StoreHandle) -> Value {
        let state = &self.states[handle];
        let mut view = state.view.clone();
        view["raw"] = serde_json::from_str(&state.canonical).expect("full canonical raw view");
        view["canonical_json"] = json!(state.canonical);
        self.normalize(&view)
    }

    pub(super) fn export_product_state(&self, handle: StoreHandle) -> Value {
        // Logical export preserves original bytes; schema5 packs exact fragments.
        // The standalone Store export keeps its existing redundant raw view.
        let state = &self.states[handle];
        let mut view = state.view.clone();
        view["canonical_json"] = json!(state.canonical);
        self.normalize(&view)
    }

    pub(super) fn export_states(&self) -> Vec<Value> {
        (0..self.states.len())
            .map(|id| self.export_state(id))
            .collect()
    }

    pub(super) fn trace(&self, mut handle: StoreHandle) -> Vec<Action> {
        let mut trace = Vec::new();
        while let Some((parent, action)) = &self.states[handle].parent {
            trace.push(action.clone());
            handle = *parent;
        }
        trace.reverse();
        trace
    }

    pub(super) fn source_bindings(&self) -> BTreeMap<&'static str, String> {
        let mut sources = source_bindings();
        sources.insert(
            "store/async_driver_product.rs",
            format!(
                "{:x}",
                Sha256::digest(include_str!("async_driver_product.rs").as_bytes())
            ),
        );
        sources.insert(
            "store/mod.rs",
            format!("{:x}", Sha256::digest(include_str!("mod.rs").as_bytes())),
        );
        for (path, bytes) in [
            ("Cargo.toml", include_str!("../../../../Cargo.toml")),
            ("Cargo.lock", include_str!("../../../../Cargo.lock")),
            (
                "rust-toolchain.toml",
                include_str!("../../../../rust-toolchain.toml"),
            ),
            (
                "crates/plurx-core/Cargo.toml",
                include_str!("../../Cargo.toml"),
            ),
            (
                "crates/plurx-worker-control/Cargo.toml",
                include_str!("../../../plurx-worker-control/Cargo.toml"),
            ),
            (
                "crates/plurxd/Cargo.toml",
                include_str!("../../../plurxd/Cargo.toml"),
            ),
            (
                "crates/plurx-worker-control/src/lib.rs",
                include_str!("../../../plurx-worker-control/src/lib.rs"),
            ),
            (
                "crates/plurx-worker-control/src/library_driver.rs",
                include_str!("../../../plurx-worker-control/src/library_driver.rs"),
            ),
            (
                "crates/plurx-worker-control/src/library_lifecycle.rs",
                include_str!("../../../plurx-worker-control/src/library_lifecycle.rs"),
            ),
            (
                "crates/plurx-worker-control/src/retry_policy.rs",
                include_str!("../../../plurx-worker-control/src/retry_policy.rs"),
            ),
            (
                "crates/plurxd/src/background_jobs.rs",
                include_str!("../../../plurxd/src/background_jobs.rs"),
            ),
            (
                "crates/plurxd/src/library_work.rs",
                include_str!("../../../plurxd/src/library_work.rs"),
            ),
            (
                "crates/plurxd/src/library_work/env.rs",
                include_str!("../../../plurxd/src/library_work/env.rs"),
            ),
            (
                "crates/plurxd/src/state.rs",
                include_str!("../../../plurxd/src/state.rs"),
            ),
        ] {
            sources.insert(path, format!("{:x}", Sha256::digest(bytes.as_bytes())));
        }
        sources
    }

    async fn observe(&mut self, episode: &Episode) -> Result<BoundaryObservation, String> {
        self.diagnostics.observations = self.diagnostics.observations.saturating_add(1);
        oracle_timed!(
            self.diagnostics,
            canonical,
            BoundaryObservation::observe(&self.fixture, episode)
        )
    }

    /// Induction certificate read set: invariants read selected job/attempt,
    /// reservation and request rows; actions additionally read domain ownership;
    /// projection reads those same rows and exact Episode captures/budgets/time.
    /// All are in the full-column snapshot. Fixture job/library IDs, schema,
    /// constants and mutation are fixed for this Oracle; omitted DV is freshly
    /// checked by observe, never inferred from a canonical match. Therefore
    /// exact equality permits substitution of derived predicates ONLY. It never
    /// substitutes production apply/replay, its complete outcome, or rollback.
    async fn check_endpoint(
        &mut self,
        handle: StoreHandle,
        observed: &BoundaryObservation,
    ) -> Result<(), String> {
        let expected = &self.states[handle];
        if observed.canonical != expected.canonical
            || serde_json::to_value(&observed.episode).expect("observed Episode")
                != serde_json::to_value(&expected.episode).expect("certified Episode")
        {
            return Err("MaterializationRoundTrip: complete canonical/Episode mismatch".into());
        }
        if expected.certified && self.reuse_certificates {
            self.diagnostics.certificate_reuses =
                self.diagnostics.certificate_reuses.saturating_add(1);
            return Ok(());
        }
        let view = projection(
            &self.fixture,
            &observed.episode,
            handle,
            &observed.canonical,
        )
        .await;
        let inputs = oracle_timed!(
            self.diagnostics,
            actions,
            actions(&self.fixture, &observed.episode)
        );
        oracle_timed!(
            self.diagnostics,
            invariants,
            invariants(&self.fixture, &observed.episode)
        )?;
        if inputs != expected.inputs || view != expected.view {
            return Err("StateCertificate: independently derived input/projection mismatch".into());
        }
        self.states[handle].certified = true;
        self.diagnostics.certifications = self.diagnostics.certifications.saturating_add(1);
        Ok(())
    }

    fn replay_edge(&self, from: StoreHandle, action: &Action) -> Result<OracleEdge, String> {
        // Fixtures may remove a memo lookup to force actual execution, but the
        // immutable certified edge still supplies the complete expected outcome.
        if let Some(id) = self.cache.get(&(from, action.clone())) {
            return Ok(self.edges[*id].clone());
        }
        self.edges
            .iter()
            .rev()
            .find(|edge| edge.from == from && &edge.action == action)
            .cloned()
            .ok_or_else(|| "ReplayCertificate: missing certified transition".into())
    }

    fn compare_replay(
        edge: &OracleEdge,
        applied: &Applied,
        episode: &Episode,
    ) -> Result<(), String> {
        let expected = Applied {
            result: edge.result.clone(),
            resolved_token: edge.resolved_token,
            claim_request: edge.claim_request.clone(),
            job_read: edge.job_read.clone(),
        };
        let claim_snapshot =
            if matches!(edge.action, Action::Claim { .. }) && applied.result == "claimed" {
                episode.claim_snapshots.last().cloned()
            } else {
                None
            };
        if &expected != applied || claim_snapshot != edge.claim_snapshot {
            return Err(
                "ReplayCertificate: complete Applied outcome/claim snapshot mismatch".into(),
            );
        }
        Ok(())
    }

    pub(super) async fn restore_root(&mut self) -> Result<(), String> {
        if self.materialization_failed {
            return Err("MaterializationPoisoned: prior restore failed".into());
        }
        let result = async {
            if self.root_savepoint {
                oracle_timed!(
                    self.diagnostics,
                    controls,
                    self.fixture.store.async_refinement_control(
                        "ROLLBACK TO async_product_root; RELEASE async_product_root;".into()
                    )
                )
                .map_err(|e| format!("RootRollback: {e}"))?;
            }
            let observed = self.observe(&Episode::initial()).await?;
            self.check_endpoint(0, &observed).await?;
            Ok(())
        }
        .await;
        if result.is_err() {
            self.materialization_failed = true;
        } else {
            self.prefix.clear();
            self.root_savepoint = false;
        }
        result
    }

    async fn materialize(&mut self, trace: &[Action]) -> Result<BoundaryObservation, String> {
        if self.materialization_failed {
            return Err("MaterializationPoisoned: prior failure".into());
        }
        let result = async {
            if !self.root_savepoint {
                oracle_timed!(
                    self.diagnostics,
                    controls,
                    self.fixture
                        .store
                        .async_refinement_control("SAVEPOINT async_product_root;".into())
                )
                .map_err(|e| format!("RootSavepoint: {e}"))?;
                self.root_savepoint = true;
            }
            let common = self
                .prefix
                .iter()
                .zip(trace)
                .take_while(|((old, _), new)| old == *new)
                .count();
            self.diagnostics.replay_saved = self.diagnostics.replay_saved.saturating_add(common);
            if common < self.prefix.len() {
                let point = if common == 0 {
                    "async_product_root".into()
                } else {
                    format!("async_product_prefix_{common}")
                };
                oracle_timed!(
                    self.diagnostics,
                    controls,
                    self.fixture
                        .store
                        .async_refinement_control(format!("ROLLBACK TO {point};"))
                )
                .map_err(|e| format!("PrefixRollback: {e}"))?;
                self.prefix.truncate(common);
            }
            let mut handle = 0;
            for action in &trace[..common] {
                handle = self.replay_edge(handle, action)?.to;
            }
            let mut episode = self
                .prefix
                .last()
                .map(|(_, ep)| ep.clone())
                .unwrap_or_else(Episode::initial);
            // Always read the actual starting/rolled-back endpoint. Never carry
            // an observation across calls, where fixture mutation could intervene.
            let mut observed = self.observe(&episode).await?;
            self.check_endpoint(handle, &observed).await?;
            for action in &trace[common..] {
                let expected = self.replay_edge(handle, action)?;
                let applied = oracle_timed!(
                    self.diagnostics,
                    replayed_apply,
                    apply_primitive(&self.fixture, &mut episode, action)
                )?;
                let after = self.observe(&episode).await?;
                observed.compare_action(action, &applied, &after)?;
                Self::compare_replay(&expected, &applied, &episode)?;
                self.diagnostics.replay_comparisons =
                    self.diagnostics.replay_comparisons.saturating_add(1);
                self.check_endpoint(expected.to, &after).await?;
                let depth = self.prefix.len() + 1;
                oracle_timed!(
                    self.diagnostics,
                    controls,
                    self.fixture.store.async_refinement_control(format!(
                        "SAVEPOINT async_product_prefix_{depth};"
                    ))
                )
                .map_err(|e| format!("PrefixSavepoint: {e}"))?;
                self.prefix.push((action.clone(), episode.clone()));
                self.diagnostics.replay_performed =
                    self.diagnostics.replay_performed.saturating_add(1);
                handle = expected.to;
                // Only SAVEPOINT (no SQL/Episode mutation) intervened.
                observed = after;
            }
            Ok(observed)
        }
        .await;
        if result.is_err() {
            self.materialization_failed = true;
        }
        result
    }

    pub(super) fn diagnostic_summary(&self) -> Value {
        let d = &self.diagnostics;
        json!({"hits":d.hits,"misses":d.misses,
            "replay_depth_total":d.replay_depth_total,"replay_depth_max":d.replay_depth_max,
            "replay_performed":d.replay_performed,"replay_saved":d.replay_saved,
            "replay_ms":d.replay.as_millis(),"requested_apply_ms":d.requested_apply.as_millis(),
            "requested_checks_ms":d.requested_checks.as_millis(),"rollback_ms":d.rollback.as_millis(),
            "canonical_ms":d.canonical.as_millis(),"actions_ms":d.actions.as_millis(),
            "invariants_ms":d.invariants.as_millis(),"controls_ms":d.controls.as_millis(),
            "replayed_apply_ms":d.replayed_apply.as_millis()})
    }

    pub(super) async fn apply(
        &mut self,
        handle: StoreHandle,
        input: &Action,
    ) -> Result<usize, String> {
        if self.materialization_failed {
            return Err("MaterializationPoisoned: prior failure".into());
        }
        let contextual = match input {
            Action::OrdinarySettle {
                job,
                token,
                kind,
                not_before_ms,
                request_now_ms,
                error_code,
                checkpoint,
            } => {
                *job < 2
                    && *token < self.states[handle].episode.tokens[*job].len()
                    && *not_before_ms >= 0
                    && *request_now_ms >= 0
                    && checkpoint.is_none()
                    && ((kind == "yield" && error_code.is_none())
                        || (kind == "retry"
                            && error_code.as_deref() == Some("library_execution_failed")))
            }
            Action::ResolveInitialClaim { job, request } => {
                *job < 2
                    && request.job_id == self.fixture.jobs[*job]
                    && request.fence.is_none()
                    && request.now_ms == self.states[handle].episode.now
                    && request.dispatched_at_ms >= 0
                    && request.now_ms >= request.dispatched_at_ms
                    && NODES.contains(&request.node_id.as_str())
                    && uuid::Uuid::parse_str(&request.boot_id).is_ok()
                    && uuid::Uuid::parse_str(&request.claim_id).is_ok()
            }
            Action::ReadClaimJob { job } => *job < 2,
            Action::ClockAdvance { now } => *now > self.states[handle].episode.now,
            _ => false,
        };
        if !contextual && !self.domain_inputs(handle).contains(input) {
            return Err("OracleInputDomain: demanded input is outside exact D".into());
        }
        if let Some(edge) = self.cache.get(&(handle, input.clone())) {
            self.diagnostics.hits = self.diagnostics.hits.saturating_add(1);
            return Ok(*edge);
        }
        self.diagnostics.misses = self.diagnostics.misses.saturating_add(1);
        let replay_started = Instant::now();
        let trace = self.trace(handle);
        self.diagnostics.replay_depth_total = self
            .diagnostics
            .replay_depth_total
            .saturating_add(trace.len());
        self.diagnostics.replay_depth_max = self.diagnostics.replay_depth_max.max(trace.len());
        let parent_observed = self.materialize(&trace).await?;
        if parent_observed.canonical != self.states[handle].canonical {
            self.materialization_failed = true;
            return Err("MaterializationRoundTrip: demanded parent differs".into());
        }
        let mut episode = parent_observed.episode.clone();
        let parent_canonical = self.states[handle].canonical.clone();
        let parent_view = self.states[handle].view.clone();
        oracle_timed!(
            self.diagnostics,
            controls,
            self.fixture
                .store
                .async_refinement_control("SAVEPOINT async_product_demand;".into())
        )
        .map_err(|e| {
            self.materialization_failed = true;
            format!("DemandSavepoint: {e}")
        })?;
        let evaluated: Result<_, String> = async {
            // The owned parent observation was checked at the final replay
            // endpoint; only SAVEPOINT intervened, with no SQL/Episode change.
            self.diagnostics.replay = self
                .diagnostics
                .replay
                .saturating_add(replay_started.elapsed());
            let applied = oracle_timed!(
                self.diagnostics,
                requested_apply,
                apply_primitive(&self.fixture, &mut episode, input)
            )?;
            let checks_started = Instant::now();
            let observed = self.observe(&episode).await?;
            parent_observed.compare_action(input, &applied, &observed)?;
            let next = observed.canonical;
            // Look up COMPLETE bytes before reusing only derived checks. Every
            // novel exact state still gets independent production-query checks.
            let known = self.index.get(&next).copied();
            let (view, inputs, invariant) = if let Some(to) =
                known.filter(|to| self.reuse_certificates && self.states[*to].certified)
            {
                self.diagnostics.certificate_reuses =
                    self.diagnostics.certificate_reuses.saturating_add(1);
                let mut view = self.states[to].view.clone();
                view["id"] = json!(self.states.len());
                (view, self.states[to].inputs.clone(), Ok(()))
            } else {
                let view = projection(&self.fixture, &episode, self.states.len(), &next).await;
                let inputs =
                    oracle_timed!(self.diagnostics, actions, actions(&self.fixture, &episode));
                let mut invariant = oracle_timed!(
                    self.diagnostics,
                    invariants,
                    invariants(&self.fixture, &episode)
                );
                if let Some(to) = known {
                    let mut expected = self.states[to].view.clone();
                    expected["id"] = json!(self.states.len());
                    if invariant.is_ok() && (inputs != self.states[to].inputs || view != expected) {
                        invariant = Err(
                            "StateCertificate: independently derived input/projection mismatch"
                                .into(),
                        );
                    }
                }
                if invariant.is_ok() {
                    self.diagnostics.certifications =
                        self.diagnostics.certifications.saturating_add(1);
                }
                (view, inputs, invariant)
            };
            self.diagnostics.requested_checks = self
                .diagnostics
                .requested_checks
                .saturating_add(checks_started.elapsed());
            Ok((
                next,
                episode.clone(),
                view,
                inputs,
                applied.result,
                applied.resolved_token,
                applied.claim_request,
                applied.job_read,
                invariant,
            ))
        }
        .await;
        let rollback_started = Instant::now();
        // Every Result failure still rolls back the actual writer, then reads
        // its entire endpoint. A panic aborts the run and cannot emit closure.
        oracle_timed!(
            self.diagnostics,
            controls,
            self.fixture.store.async_refinement_control(
                "ROLLBACK TO async_product_demand; RELEASE async_product_demand;".into()
            )
        )
        .map_err(|error| {
            self.materialization_failed = true;
            format!("RollbackRoundTrip: {error}")
        })?;
        let restored = async {
            let observed = self.observe(&parent_observed.episode).await?;
            if observed.omitted_dv != parent_observed.omitted_dv {
                return Err("OmittedDvState: demand rollback changed omitted rows".into());
            }
            self.check_endpoint(handle, &observed).await
        }
        .await;
        if let Err(error) = restored {
            self.materialization_failed = true;
            return Err(error);
        }
        self.diagnostics.rollback = self
            .diagnostics
            .rollback
            .saturating_add(rollback_started.elapsed());
        let (
            next,
            successor,
            mut view,
            inputs,
            result,
            resolved_token,
            claim_request,
            job_read,
            invariant,
        ) = evaluated.inspect_err(|_| {
            self.materialization_failed = true;
        })?;
        if invariant.is_err() || (!self.index.contains_key(&next) && self.states.len() >= STATE_CAP)
        {
            self.failure = Some(self.normalize(&json!({
                "complete": false, "from": handle, "action": input, "result": result,
                "predecessor_sha256": format!("{:x}", Sha256::digest(parent_canonical.as_bytes())),
                "successor_sha256": format!("{:x}", Sha256::digest(next.as_bytes())),
                "trace": trace, "predecessor": parent_view, "successor": view,
                "predecessor_canonical_json": parent_canonical, "successor_canonical_json": next,
            })));
        }
        if invariant.is_err() {
            self.materialization_failed = true;
        }
        invariant?;
        for (old, new) in parent_view["jobs"]
            .as_array()
            .expect("jobs")
            .iter()
            .zip(view["jobs"].as_array().expect("jobs"))
        {
            if new["fence"].as_i64() < old["fence"].as_i64() {
                return Err("MonotoneFence: oracle edge decreased job fence".into());
            }
        }
        if parent_view["domain"].is_object()
            && view["domain"].is_object()
            && view["domain"]["fence"].as_u64() < parent_view["domain"]["fence"].as_u64()
        {
            return Err("MonotoneFence: oracle edge decreased domain fence".into());
        }
        let claim_snapshot = if matches!(input, Action::Claim { .. }) && result == "claimed" {
            successor.claim_snapshots.last().cloned()
        } else {
            None
        };
        let to = if let Some(to) = self.index.get(&next) {
            let to = *to;
            self.states[to].certified = true;
            to
        } else {
            if self.states.len() >= STATE_CAP {
                return Err(format!(
                    "DomainStateCap: closure exceeded {STATE_CAP} concrete states; no closed relation emitted"
                ));
            }
            let to = self.states.len();
            view["id"] = json!(to);
            self.index.insert(next.clone(), to);
            self.states.push(OracleState {
                canonical: next,
                episode: successor,
                view,
                parent: Some((handle, input.clone())),
                inputs,
                certified: true,
            });
            to
        };
        let id = self.edges.len();
        self.edges.push(OracleEdge {
            from: handle,
            to,
            action: input.clone(),
            result: result.clone(),
            resolved_token,
            claim_snapshot,
            claim_request,
            job_read,
            error: match result.as_str() {
                "validation_error_yield_time" => {
                    Some("invalid background checkpoint or yield time".into())
                }
                "validation_error_retry_time" => Some("invalid background failure retry".into()),
                _ => None,
            },
        });
        self.cache.insert((handle, input.clone()), id);
        self.failure = None;
        Ok(id)
    }
}

#[derive(Clone, Copy, Debug)]
enum GuardedOperation {
    Renew,
    Settle,
    Bind,
    Complete,
}

async fn guarded_operation(
    fixture: &Fixture,
    operation: GuardedOperation,
    token: JobToken,
    lease: Lease,
    now_ms: i64,
) -> bool {
    match operation {
        GuardedOperation::Renew => matches!(
            fixture
                .store
                .renew_jobs(RenewJobs {
                    tokens: vec![token],
                    now_ms,
                })
                .await
                .expect("validated renewal partition")
                .as_slice(),
            [RenewOutcome::Renewed { .. }]
        ),
        GuardedOperation::Settle => fixture
            .store
            .settle_job(yielding(token, now_ms))
            .await
            .expect("validated settlement partition"),
        GuardedOperation::Bind => fixture
            .store
            .bind_library_job(BindLibraryJob {
                token,
                lease,
                now_ms,
            })
            .await
            .expect("validated binding partition"),
        GuardedOperation::Complete => fixture
            .store
            .complete_library_work(fixture.completion(token, lease, now_ms))
            .await
            .expect("validated completion partition"),
    }
}

/// Exactly 4 operations × 8 queue-token identity classes × 3 queue-expiry
/// neighbours = 96 cases. This is a labelled predicate-partition conformance
/// matrix around reachable seeds; it is not exhaustive relation coverage.
#[tokio::test]
async fn queue_identity_and_expiry_predicate_partitions_are_atomic() {
    let mut count = 0;
    for operation in [
        GuardedOperation::Renew,
        GuardedOperation::Settle,
        GuardedOperation::Bind,
        GuardedOperation::Complete,
    ] {
        for identity in [
            "current", "job", "node", "boot", "claim", "fence", "revision", "expiry",
        ] {
            for offset in [-1, 0, 1] {
                let fixture = Fixture::new().await;
                let (mut token, lease) = fixture.bound().await;
                let now = token.lease_expires_ms + offset;
                match identity {
                    "job" => token.job_id = fixture.jobs[1].clone(),
                    "node" => token.node_id = NODES[1].into(),
                    "boot" => token.boot_id = BOOTS[1].into(),
                    "claim" => token.claim_id = fixture.claim(0, 0, NOW, 1).claim_id,
                    "fence" => token.fence += 1,
                    "revision" => token.revision += 1,
                    "expiry" => token.lease_expires_ms += 1,
                    _ => {}
                }
                let before = snapshot(&fixture.store).await;
                let accepted = guarded_operation(&fixture, operation, token, lease, now).await;
                assert_eq!(
                    accepted,
                    identity == "current" && offset == -1,
                    "{operation:?} identity={identity} expiry_offset={offset}"
                );
                if !accepted {
                    assert_eq!(
                        snapshot(&fixture.store).await,
                        before,
                        "{operation:?} failed CAS mutated durable state ({identity}, {offset})"
                    );
                }
                count += 1;
            }
        }
    }
    assert_eq!(count, 96);
    eprintln!("async_store_predicate_partitions={{\"queue_identity_expiry_cases\":96,\"relation_coverage\":false}}");
}

#[tokio::test]
async fn domain_expiry_missing_resources_and_overflow_are_separate_partitions() {
    for operation in [GuardedOperation::Bind, GuardedOperation::Complete] {
        for offset in [-1, 0, 1] {
            let fixture = Fixture::new().await;
            let token = fixture.claimed(0).await;
            let LeaseClaim::Acquired(lease) = fixture
                .store
                .acquire_lease(
                    &format!("scan:library:{}", fixture.library),
                    NODES[0],
                    NOW,
                    NOW + 10_000,
                )
                .await
                .expect("short domain")
            else {
                panic!("domain");
            };
            assert!(fixture
                .store
                .bind_library_job(BindLibraryJob {
                    token: token.clone(),
                    lease: lease.clone(),
                    now_ms: NOW,
                })
                .await
                .expect("initial binding"));
            let now = lease.expires_at_unix_ms + offset;
            let before = snapshot(&fixture.store).await;
            let accepted = guarded_operation(&fixture, operation, token, lease, now).await;
            assert_eq!(
                accepted,
                offset == -1,
                "{operation:?} domain expiry offset={offset}"
            );
            if !accepted {
                assert_eq!(snapshot(&fixture.store).await, before);
            }
        }
    }
    for operation in [
        GuardedOperation::Renew,
        GuardedOperation::Settle,
        GuardedOperation::Bind,
        GuardedOperation::Complete,
    ] {
        let fixture = Fixture::new().await;
        let (token, lease) = fixture.bound().await;
        sql(&fixture.store, "DELETE FROM background_job_reservations WHERE job_id=json_extract($1,'$.job') AND resource_key='provider:tmdb'",
            json!({"job": fixture.jobs[0]})).await;
        let before = snapshot(&fixture.store).await;
        let accepted = guarded_operation(&fixture, operation, token, lease, NOW + 1).await;
        // Settlement retires an exact current claim even if a required
        // reservation has been lost; only authority-extending writes refuse.
        assert_eq!(
            accepted,
            matches!(operation, GuardedOperation::Settle),
            "{operation:?} resource guard"
        );
        if !accepted {
            assert_eq!(snapshot(&fixture.store).await, before);
        }

        let fixture = Fixture::new().await;
        let (mut token, lease) = fixture.bound().await;
        sql(&fixture.store, "UPDATE background_jobs SET revision=9223372036854775807 WHERE id=json_extract($1,'$.job')",
            json!({"job": fixture.jobs[0]})).await;
        token.revision = i64::MAX;
        let before = snapshot(&fixture.store).await;
        let accepted = guarded_operation(&fixture, operation, token, lease, NOW + 1).await;
        assert_eq!(
            accepted,
            matches!(operation, GuardedOperation::Bind),
            "{operation:?} MAX revision guard"
        );
        if !accepted {
            assert_eq!(snapshot(&fixture.store).await, before);
        }
    }
    for column in ["fence", "revision"] {
        let fixture = Fixture::new().await;
        sql(&fixture.store, &format!("UPDATE background_jobs SET {column}=9223372036854775807 WHERE id=json_extract($1,'$.job')"),
            json!({"job": fixture.jobs[0]})).await;
        let before = snapshot(&fixture.store).await;
        let revision = if column == "revision" { i64::MAX } else { 0 };
        assert!(matches!(
            fixture
                .store
                .claim_job(fixture.claim(0, revision, NOW, 0))
                .await
                .expect("MAX claim partition"),
            ClaimOutcome::Contended
        ));
        assert_eq!(
            snapshot(&fixture.store).await,
            before,
            "MAX {column} claim refusal"
        );
    }
}

#[tokio::test]
async fn claim_guard_predicate_partitions_refuse_without_mutation() {
    for guard in [
        "not_due",
        "retry_equality",
        "waiter_deadline_equality",
        "owner_removed",
        "target_node",
        "attempt_exhausted",
        "claim_identity_reuse",
        "provider_capacity",
    ] {
        let fixture = Fixture::new().await;
        let request = fixture.claim(0, 0, NOW, 0);
        let parameters = json!({"job": fixture.jobs[0], "other": fixture.jobs[1], "claim": request.claim_id,
            "boot": request.boot_id, "now": NOW, "later": NOW + 1});
        let statement = match guard {
            "not_due" => "UPDATE background_jobs SET not_before_ms=json_extract($1,'$.later') WHERE id=json_extract($1,'$.job')",
            "retry_equality" => "UPDATE background_jobs SET retry_deadline_ms=json_extract($1,'$.now') WHERE id=json_extract($1,'$.job')",
            "waiter_deadline_equality" => "UPDATE background_job_waiters SET deadline_ms=json_extract($1,'$.now') WHERE job_id=json_extract($1,'$.job')",
            "owner_removed" => "INSERT INTO settings(key,value,updated_at) VALUES('internal.cluster_job_owner_removed.node-a','true',json_extract($1,'$.now'))",
            "target_node" => "UPDATE background_jobs SET target_node_id='node-b' WHERE id=json_extract($1,'$.job')",
            "attempt_exhausted" => "UPDATE background_jobs SET failed_attempts=attempt_limit WHERE id=json_extract($1,'$.job')",
            "claim_identity_reuse" => "INSERT INTO background_job_attempts(job_id,fence,claim_id,owner_node_id,owner_boot_id,started_at_ms,resolve_until_ms,finished_at_ms,outcome) VALUES(json_extract($1,'$.job'),1,json_extract($1,'$.claim'),'node-a',json_extract($1,'$.boot'),0,0,1,'yielded')",
            _ => "INSERT INTO background_job_reservations(resource_key,slot,job_id,fence,expires_at_ms) SELECT 'provider:tmdb',slot,json_extract($1,'$.other'),1,json_extract($1,'$.later') FROM (SELECT 0 AS slot UNION ALL SELECT 1)",
        };
        sql(&fixture.store, statement, parameters).await;
        let before = snapshot(&fixture.store).await;
        assert!(
            !matches!(
                fixture
                    .store
                    .claim_job(request)
                    .await
                    .expect("claim guard partition"),
                ClaimOutcome::Claimed { .. }
            ),
            "{guard}"
        );
        assert_eq!(
            snapshot(&fixture.store).await,
            before,
            "claim guard {guard}"
        );
    }
}

/// Four inputs excluded by the ordinary selected-protocol graph are still
/// exercised against actual validation/SQL as explicit conformance rows.
#[tokio::test]
async fn protocol_excluded_domain_owner_and_terminal_settlements_are_conformance_only() {
    for operation in [GuardedOperation::Bind, GuardedOperation::Complete] {
        let fixture = Fixture::new().await;
        let (token, mut lease) = fixture.bound().await;
        lease.owner_node_id = NODES[1].into();
        let before = snapshot(&fixture.store).await;
        assert!(
            !guarded_operation(&fixture, operation, token, lease, NOW + 1).await,
            "{operation:?} wrong captured domain owner"
        );
        assert_eq!(snapshot(&fixture.store).await, before);
    }
    for terminal in ["fail", "stop"] {
        let fixture = Fixture::new().await;
        let (token, _) = fixture.bound().await;
        let settlement = if terminal == "fail" {
            JobSettlement::Fail {
                error_code: "permanent".into(),
            }
        } else {
            JobSettlement::Stop {
                error_code: "stopped".into(),
            }
        };
        assert!(fixture
            .store
            .settle_job(SettleJob {
                token: token.clone(),
                settlement,
                now_ms: NOW + 1
            })
            .await
            .expect("terminal settlement conformance"));
        let job = fixture
            .store
            .background_job(&fixture.jobs[0])
            .await
            .expect("terminal job")
            .expect("job");
        assert_eq!(
            job.state,
            if terminal == "fail" {
                JobState::Failed
            } else {
                JobState::Cancelled
            }
        );
        assert!(job.token.is_none());
        assert_eq!(
            reservations(&fixture.store, &fixture.jobs[0]).await,
            json!([])
        );
        let before = snapshot(&fixture.store).await;
        assert!(!fixture
            .store
            .settle_job(yielding(token, NOW + 2))
            .await
            .expect("terminal stale settlement"));
        assert_eq!(snapshot(&fixture.store).await, before);
    }
}

#[test]
fn snapshot_table_registry_matches_producer() {
    let registry: Value = serde_json::from_str(include_str!(
        "../../../../verification/async-driver-refinement/store-schema.json"
    ))
    .expect("Store schema registry JSON");
    assert_eq!(
        registry["snapshot_tables"],
        json!(SNAPSHOT_TABLES),
        "the source snapshot table list and closed registry must change together"
    );
}

/// These three histories execute real Store primitives. The daemon's mutex,
/// heartbeat drain and first/resolve/second call sequence remain separately
/// reviewed daemon conformance; this is not a second retirement interpreter.
#[tokio::test]
async fn retirement_settle_and_resolution_primitives_preserve_identity_and_rows() {
    let fixture = Fixture::new().await;
    let token = fixture.claimed(0).await;
    let mut episode = Episode::initial();
    episode.claims[0] = 1;
    remember(&mut episode.tokens[0], token.clone());
    assert_eq!(
        apply_graph(
            &fixture,
            &mut episode,
            &Action::Resolve { job: 0, token: 0 }
        )
        .await
        .expect("initial exact resolution"),
        ("running".into(), Some(0))
    );
    assert_eq!(
        apply_graph(&fixture, &mut episode, &Action::Renew { job: 0, token: 0 })
            .await
            .expect("actual renewal")
            .0,
        "renewed"
    );
    assert_eq!(episode.tokens[0].len(), 2);
    let before_stale = snapshot(&fixture.store).await;
    assert_eq!(
        apply_graph(
            &fixture,
            &mut episode,
            &Action::Settle {
                job: 0,
                token: 0,
                disposition: 3
            }
        )
        .await
        .expect("stale first retirement primitive")
        .0,
        "refused"
    );
    assert_eq!(snapshot(&fixture.store).await, before_stale);
    let (result, current) = apply_graph(
        &fixture,
        &mut episode,
        &Action::Resolve { job: 0, token: 0 },
    )
    .await
    .expect("exact original identity resolves newer revision");
    assert_eq!(result, "running");
    assert_eq!(current, Some(1));
    assert_eq!(
        episode.tokens[0][0], token,
        "capture prefixes remain stable"
    );
    assert_eq!(
        apply_graph(
            &fixture,
            &mut episode,
            &Action::Settle {
                job: 0,
                token: 1,
                disposition: 3
            }
        )
        .await
        .expect("current retirement Yield primitive")
        .0,
        "accepted"
    );
    let rows = fixture.store.queue_sql(
        "SELECT json_object('state',state,'error',last_error_code) FROM background_jobs WHERE id=json_extract($1,'$.job')".into(),
        json!({"job": fixture.jobs[0]}).to_string(), false, true).await.expect("retirement payload");
    assert_eq!(
        serde_json::from_str::<Value>(&rows[0]).expect("retirement row"),
        json!({"state": "queued", "error": "worker_retired"})
    );
    assert_eq!(
        apply_graph(
            &fixture,
            &mut episode,
            &Action::Resolve { job: 0, token: 0 }
        )
        .await
        .expect("settled exact identity"),
        ("settled".into(), None)
    );

    let fixture = Fixture::new().await;
    let (token, lease) = fixture.bound().await;
    let mut episode = Episode::initial();
    episode.claims[0] = 1;
    episode.domain_acquired[0] = true;
    remember(&mut episode.tokens[0], token);
    episode.leases.push(lease);
    assert_eq!(
        apply_graph(
            &fixture,
            &mut episode,
            &Action::Complete {
                job: 0,
                token: 0,
                lease: 0
            }
        )
        .await
        .expect("last request COMPLETE primitive")
        .0,
        "accepted"
    );
    let before = snapshot(&fixture.store).await;
    assert_eq!(
        apply_graph(
            &fixture,
            &mut episode,
            &Action::Settle {
                job: 0,
                token: 0,
                disposition: 3
            }
        )
        .await
        .expect("post-COMPLETE retirement refusal")
        .0,
        "refused"
    );
    assert_eq!(
        apply_graph(
            &fixture,
            &mut episode,
            &Action::Resolve { job: 0, token: 0 }
        )
        .await
        .expect("post-COMPLETE resolution"),
        ("settled".into(), None)
    );
    assert_eq!(snapshot(&fixture.store).await, before);
    assert_eq!(
        episode.tokens[0].len(),
        1,
        "COMPLETE does not erase local captures"
    );

    // Queue cancellation is deliberately outside selected D. Exercise the
    // actual CancelRequested API and Cancel primitive as conformance only.
    let fixture = Fixture::new().await;
    let token = fixture.claimed(0).await;
    let mut episode = Episode::initial();
    episode.claims[0] = 1;
    remember(&mut episode.tokens[0], token);
    fixture
        .store
        .cancel_job(super::background_jobs::CancelJob {
            job_id: fixture.jobs[0].clone(),
            now_ms: NOW,
        })
        .await
        .expect("outside-D cancellation conformance");
    let (result, current) = apply_graph(
        &fixture,
        &mut episode,
        &Action::Resolve { job: 0, token: 0 },
    )
    .await
    .expect("exact cancellation resolution");
    assert_eq!(result, "cancel_requested");
    let current = current.expect("actual cleanup token index");
    assert_eq!(
        apply_graph(
            &fixture,
            &mut episode,
            &Action::Settle {
                job: 0,
                token: current,
                disposition: 2
            }
        )
        .await
        .expect("actual cancellation settlement")
        .0,
        "accepted"
    );
    let cancelled = fixture
        .store
        .background_job(&fixture.jobs[0])
        .await
        .expect("cancelled job")
        .expect("job");
    assert_eq!(cancelled.state, JobState::Cancelled);
    assert!(cancelled.token.is_none());
    assert_eq!(
        reservations(&fixture.store, &fixture.jobs[0]).await,
        json!([])
    );
    eprintln!(
        "async_store_retirement_primitives={}",
        json!({
            "histories": 3, "scope": "actual primitive conformance; daemon sequence is a separate boundary",
            "logical_resolution_internal_reads": "conformance boundary",
            "cancellation": "outside selected D"
        })
    );
}

#[tokio::test]
async fn refusal_snapshot_guard_detects_mutation_in_apply() {
    let fixture = Fixture::new().await;
    let token = fixture.claimed(0).await;
    assert!(fixture
        .store
        .settle_job(yielding(token.clone(), NOW))
        .await
        .expect("retired claim"));
    let schema = super::background_jobs::SCHEMA;
    let trigger = schema
        .split("-- next statement")
        .find(|statement| {
            statement.contains("CREATE TRIGGER IF NOT EXISTS background_job_renew_command")
        })
        .expect("production renewal trigger");
    let removal = "    DELETE FROM background_job_commands WHERE id = NEW.id;";
    assert!(trigger.contains(removal), "mutation source anchor");
    let mutated = trigger.replace(removal,
        "    UPDATE background_jobs SET yield_count=yield_count+1 WHERE id=json_extract(NEW.request_json,'$.tokens[0].job_id');\n    DELETE FROM background_job_commands WHERE id = NEW.id;");
    fixture
        .store
        .async_refinement_control(format!(
            "DROP TRIGGER background_job_renew_command;\n{mutated}"
        ))
        .await
        .expect("install actual source-derived refusal mutant");
    let mut episode = Episode::initial();
    episode.claims[0] = 1;
    remember(&mut episode.tokens[0], token);
    let failure = apply(
        &fixture,
        &mut episode,
        &Action::Renew { job: 0, token: 0 },
        true,
    )
    .await
    .expect_err("apply itself must reject durable mutation on refusal");
    assert!(
        failure.starts_with("FailedCasAtomicity"),
        "observed {failure}"
    );
    let before = BoundaryObservation::observe(&fixture, &episode)
        .await
        .expect("oracle preimage");
    let action = Action::Renew { job: 0, token: 0 };
    let applied = apply_primitive(&fixture, &mut episode, &action)
        .await
        .expect("actual refused renewal");
    assert!(refused(&applied.result));
    let after = BoundaryObservation::observe(&fixture, &episode)
        .await
        .expect("oracle postimage");
    assert!(before
        .compare_action(&action, &applied, &after)
        .expect_err("shared actual boundary must retain refusal row observation")
        .starts_with("FailedCasAtomicity:"));
    eprintln!(
        "async_store_refusal_guard={}",
        json!({"named_failure": failure, "detected_in": "apply"})
    );
}

// Focused actual API definition, deliberately independent of external formal
// inputs and of the product traversal. The 120s query-window boundary below is
// conformance only; it does not introduce a contextual product clock step.
#[tokio::test]
async fn actual_initial_claim_recovery_queries_preserve_dispatch_and_read_rows() {
    let mut oracle = Oracle::new(None).await.expect("migrated actual fixture");
    let claim = oracle
        .apply(0, &Action::Claim { job: 0, node: 0 })
        .await
        .expect("actual claim");
    let claimed = oracle.edges[claim].clone();
    assert_eq!(claimed.result, "claimed");
    let actual: ClaimJob =
        serde_json::from_value(claimed.claim_request.clone().expect("exact passed request"))
            .expect("actual ClaimJob");
    let query = |now_ms, dispatched_at_ms| Action::ResolveInitialClaim {
        job: 0,
        request: InitialClaimResolution {
            job_id: actual.job_id.clone(),
            node_id: actual.node_id.clone(),
            boot_id: actual.boot_id.clone(),
            claim_id: actual.claim_id.clone(),
            fence: None,
            dispatched_at_ms,
            now_ms,
        },
    };
    let resolve = oracle
        .apply(claimed.to, &query(NOW, actual.dispatched_at_ms))
        .await
        .expect("actual fence-None query");
    let running = oracle.edges[resolve].clone();
    assert_eq!(running.result, "running");
    let token = oracle.states[running.to].episode.tokens[0]
        [running.resolved_token.expect("actual capture")]
    .clone();
    assert_eq!(
        &token,
        &claimed
            .claim_snapshot
            .as_ref()
            .expect("committed returned snapshot")
            .token_capture
    );
    assert_eq!(
        oracle.states[claimed.to].canonical,
        oracle.states[running.to].canonical
    );
    let read = oracle
        .apply(running.to, &Action::ReadClaimJob { job: 0 })
        .await
        .expect("actual committed reread");
    let edge = oracle.edges[read].clone();
    assert_eq!(edge.result, "present");
    let row: super::background_jobs::BackgroundJob =
        serde_json::from_value(edge.job_read.clone().expect("full actual row"))
            .expect("BackgroundJob wire");
    assert_eq!(row.id, actual.job_id);
    assert_eq!(row.token.as_ref(), Some(&token));
    assert!(row
        .supported_payload()
        .is_ok_and(|payload| payload.kind() == actual.kind));
    assert_eq!(
        oracle.states[edge.from].canonical,
        oracle.states[edge.to].canonical
    );
    assert_eq!(
        oracle
            .apply(edge.to, &Action::ReadClaimJob { job: 0 })
            .await
            .expect("stable repeated read"),
        read
    );
    assert_eq!(
        oracle.states[edge.to].episode.claim_snapshots.len(),
        1,
        "a read never fabricates ClaimOutcome history"
    );
    let yield_edge = oracle
        .apply(
            edge.to,
            &Action::Settle {
                job: 0,
                token: 0,
                disposition: 3,
            },
        )
        .await
        .expect("actual retirement yield");
    assert_eq!(oracle.edges[yield_edge].result, "accepted");
    let later = NOW + super::background_jobs::CLAIM_RESOLUTION_WINDOW_MS;
    let clock = oracle
        .apply(
            oracle.edges[yield_edge].to,
            &Action::ClockAdvance { now: later },
        )
        .await
        .expect("conformance request-window clock");
    let from = oracle.edges[clock].to;
    let old = oracle
        .apply(from, &query(later, actual.dispatched_at_ms))
        .await
        .expect("original dispatch window");
    assert_eq!(oracle.edges[old].result, "expired_or_pruned");
    let fresh = oracle
        .apply(from, &query(later, later))
        .await
        .expect("distinct query input");
    assert_eq!(
        oracle.edges[fresh].result, "settled",
        "initial resolution must not replace its original dispatch sample"
    );
    assert_eq!(
        oracle.states[from].canonical,
        oracle.states[oracle.edges[old].to].canonical
    );
    assert_eq!(
        oracle.states[from].canonical,
        oracle.states[oracle.edges[fresh].to].canonical
    );
    oracle.restore_root().await.expect("verified Init teardown");
}

#[test]
fn state_cap_diagnostic_is_bounded_and_marks_partial_trace() {
    let mut episode = Episode::initial();
    episode.now = i64::MAX;
    let action = Action::Complete {
        job: usize::MAX,
        token: usize::MAX,
        lease: usize::MAX,
    };
    let trace = vec![action.clone(); CAP_DIAGNOSTIC_TRACE_ACTIONS + 1];
    let predecessor_key = "private-row-value".repeat(1_000);
    let rejected_key = "different-private-row-value".repeat(1_000);
    let input = CapDiagnostic {
        counts: [usize::MAX; 4],
        from: usize::MAX,
        trace: &trace,
        predecessor: (&episode, &predecessor_key),
        rejected: (&episode, &rejected_key, &action),
    };
    let encoded = state_cap_diagnostic(&input);
    assert!(encoded.len() <= CAP_DIAGNOSTIC_MAX_BYTES);
    assert!(!encoded.contains("private-row-value"));
    let record: Value = serde_json::from_str(&encoded).expect("structured cap diagnostic");
    assert_eq!(record["complete"], false);
    assert_eq!(record["state_cap"], STATE_CAP);
    assert_eq!(record["retained_state_count"], usize::MAX);
    assert_eq!(
        record["trace_tail"].as_array().expect("tail").len(),
        CAP_DIAGNOSTIC_TRACE_ACTIONS
    );
    assert_eq!(record["trace_complete"], false);
    assert_eq!(record["trace_tail_start"], 1);
    assert_eq!(record["predecessor_depth"], trace.len());
    assert_eq!(record["rejected_depth"], trace.len() + 1);
    assert_eq!(record["rejected_action"]["action"]["op"], "complete");
    for field in ["predecessor_sha256", "rejected_successor_sha256"] {
        assert_eq!(record[field].as_str().expect("digest").len(), 64);
    }
    let short = CapDiagnostic {
        trace: &trace[..1],
        ..input
    };
    let short: Value = serde_json::from_str(&state_cap_diagnostic(&short)).expect("short trace");
    assert_eq!(short["trace_complete"], true);
    assert_eq!(short["trace_tail_start"], 0);
    let oversized = diagnostic_action(
        serde_json::to_string(&"x".repeat(CAP_DIAGNOSTIC_ACTION_BYTES + 1))
            .expect("oversized action JSON"),
    );
    assert!(oversized["action"].is_null());
    assert!(
        oversized["encoded_bytes"].as_u64().expect("length") > CAP_DIAGNOSTIC_ACTION_BYTES as u64
    );
    assert_eq!(oversized["sha256"].as_str().expect("digest").len(), 64);
}

#[tokio::test]
async fn oracle_prefix_reuse_matches_root_replay_with_refusal_branch_and_clock() {
    let mut oracle = Oracle::new(None)
        .await
        .expect("private actual SQLite fixture");
    let omitted = omitted_dv_state(&oracle.fixture.store)
        .await
        .expect("fixed Init DV state");
    let claim = Action::Claim { job: 0, node: 0 };
    let first = oracle.apply(0, &claim).await.expect("initial claim");
    let claimed = oracle.edges[first].to;
    assert_eq!(oracle.edges[first].result, "claimed");
    let cases = [
        (claimed, Action::ReadClaimJob { job: 0 }),
        (claimed, Action::Claim { job: 0, node: 1 }),
        (0, Action::Claim { job: 1, node: 0 }),
        (claimed, Action::ClockAdvance { now: NOW + 1 }),
        (claimed, Action::Resolve { job: 0, token: 0 }),
    ];
    let mut saw_refusal = false;
    for (from, input) in cases {
        oracle.restore_root().await.expect("baseline Init");
        oracle.cache.remove(&(from, input.clone()));
        oracle.reuse_certificates = false;
        let baseline = oracle
            .apply(from, &input)
            .await
            .expect("full root replay transition");
        let edge = serde_json::to_value(&oracle.edges[baseline]).expect("baseline exact edge");
        let state = oracle.export_state(oracle.edges[baseline].to);
        let inputs = oracle.states[oracle.edges[baseline].to].inputs.clone();
        let certifications = oracle.diagnostics.certifications;
        let observations = oracle.diagnostics.observations;
        assert_eq!(
            omitted_dv_state(&oracle.fixture.store)
                .await
                .expect("root replay DV state"),
            omitted
        );
        saw_refusal |= oracle.edges[baseline].result == "refused";
        let performed = oracle.diagnostics.replay_performed;
        oracle.cache.remove(&(from, input.clone()));
        oracle.reuse_certificates = true;
        let reused = oracle
            .apply(from, &input)
            .await
            .expect("retained prefix transition");
        assert_eq!(
            serde_json::to_value(&oracle.edges[reused]).expect("reused exact edge"),
            edge
        );
        assert_eq!(oracle.export_state(oracle.edges[reused].to), state);
        assert_eq!(oracle.states[oracle.edges[reused].to].inputs, inputs);
        assert_eq!(
            oracle.diagnostics.certifications, certifications,
            "exact known states reuse independently certified predicates"
        );
        assert_eq!(
            oracle.diagnostics.observations - observations,
            3,
            "actual parent, requested successor and rollback each observed once"
        );
        assert_eq!(
            omitted_dv_state(&oracle.fixture.store)
                .await
                .expect("retained-prefix DV state"),
            omitted
        );
        assert_eq!(
            oracle.diagnostics.replay_performed, performed,
            "same parent must not replay prefix"
        );
        assert_eq!(
            canonical(&oracle.fixture, &oracle.states[from].episode).await,
            oracle.states[from].canonical
        );
    }
    assert!(saw_refusal);
    assert!(oracle.diagnostics.replay_saved > 0);
    assert!(oracle.diagnostics.certificate_reuses > 0);
    assert!(oracle.diagnostics.replay_comparisons > 0);
    let host = oracle.diagnostic_summary();
    for field in [
        "canonical_ms",
        "actions_ms",
        "invariants_ms",
        "controls_ms",
        "replayed_apply_ms",
    ] {
        assert!(
            host[field].as_u64().is_some(),
            "bounded host timing {field}"
        );
    }
    // A deeper retained prefix must roll back to a nonzero LCP and restore
    // the historical clock and captured-token Episode, not just durable rows.
    let clock = oracle
        .apply(claimed, &Action::ClockAdvance { now: NOW + 1 })
        .await
        .expect("clock state");
    let clocked = oracle.edges[clock].to;
    oracle
        .apply(clocked, &Action::ReadClaimJob { job: 1 })
        .await
        .expect("deeper clock prefix");
    assert_eq!(oracle.prefix.last().expect("deep prefix").1.now, NOW + 1);
    assert_eq!(
        omitted_dv_state(&oracle.fixture.store)
            .await
            .expect("deep-prefix DV state"),
        omitted
    );
    oracle
        .cache
        .remove(&(claimed, Action::ReadClaimJob { job: 1 }));
    oracle
        .apply(claimed, &Action::ReadClaimJob { job: 1 })
        .await
        .expect("historical captured Episode");
    assert_eq!(oracle.prefix.last().expect("historical prefix").1.now, NOW);
    assert_eq!(
        omitted_dv_state(&oracle.fixture.store)
            .await
            .expect("nonzero LCP DV state"),
        omitted
    );
    assert_eq!(
        oracle.prefix.last().expect("historical prefix").1.tokens,
        oracle.states[claimed].episode.tokens
    );
    oracle.cache.remove(&(0, Action::ReadClaimJob { job: 1 }));
    oracle
        .apply(0, &Action::ReadClaimJob { job: 1 })
        .await
        .expect("LCP rollback to root");
    oracle
        .restore_root()
        .await
        .expect("normal verified teardown");
    assert_eq!(
        omitted_dv_state(&oracle.fixture.store)
            .await
            .expect("teardown DV state"),
        omitted
    );
}

#[tokio::test]
async fn oracle_prefix_reset_preserves_mutation_and_rejects_failed_rollback() {
    let mut oracle = Oracle::new(None).await.expect("actual SQLite fixture");
    let init = oracle.states[0].canonical.clone();
    let omitted = omitted_dv_state(&oracle.fixture.store)
        .await
        .expect("Init omitted state");
    let action = Action::Claim { job: 0, node: 0 };
    let first = oracle.apply(0, &action).await.expect("claim");
    let parent = oracle.edges[first].to;
    oracle
        .apply(parent, &Action::ReadClaimJob { job: 0 })
        .await
        .expect("retained nonroot prefix");
    assert!(!oracle.prefix.is_empty());
    oracle
        .begin_paired_mutation(TriggerMutation::CompletionRetirement)
        .await
        .expect("verified Init before schema mutation");
    assert!(oracle.prefix.is_empty());
    assert!(!oracle.root_savepoint);
    assert_eq!(oracle.states.len(), 1);
    assert!(oracle.states[0].certified);
    assert_eq!(oracle.diagnostics.invalidations, 1);
    assert!(
        oracle.diagnostics.certifications > 0,
        "paired mutation independently rebuilds the root certificate"
    );
    assert_eq!(
        omitted_dv_state(&oracle.fixture.store)
            .await
            .expect("mutation reset DV state"),
        omitted
    );
    assert_eq!(canonical(&oracle.fixture, &Episode::initial()).await, init);
    oracle
        .apply(0, &action)
        .await
        .expect("actual operation after schema mutation");
    oracle
        .fixture
        .store
        .async_refinement_control("RELEASE async_product_root;".into())
        .await
        .expect("deliberate fixture savepoint loss");
    assert!(oracle.restore_root().await.is_err());
    assert!(oracle.materialization_failed);
    assert!(
        oracle.apply(0, &action).await.is_err(),
        "a prior cached edge cannot hide failed restore"
    );

    // Every retained outcome component is authoritative, including captures
    // which do not change SQL rows. Actual replay must reject each corruption.
    for field in [
        "result",
        "resolved_token",
        "claim_request",
        "job_read",
        "claim_snapshot",
    ] {
        let mut replay = Oracle::new(None).await.expect("replay corruption fixture");
        let id = replay
            .apply(0, &action)
            .await
            .expect("certified actual claim");
        let to = replay.edges[id].to;
        let count = replay.states.len();
        match field {
            "result" => replay.edges[id].result = "refused".into(),
            "resolved_token" => replay.edges[id].resolved_token = Some(999),
            "claim_request" => replay.edges[id].claim_request = Some(json!({"corrupt": true})),
            "job_read" => replay.edges[id].job_read = Some(json!({"corrupt": true})),
            "claim_snapshot" => {
                replay.edges[id]
                    .claim_snapshot
                    .as_mut()
                    .expect("claim capture")
                    .failed_attempts += 1
            }
            _ => unreachable!(),
        }
        let error = replay
            .apply(to, &Action::ReadClaimJob { job: 0 })
            .await
            .expect_err("actual replay must compare the complete certified outcome");
        assert!(error.starts_with("ReplayCertificate:"), "{field}: {error}");
        assert!(replay.materialization_failed);
        assert_eq!(replay.states.len(), count, "no state published on mismatch");
        assert_eq!(replay.edges.len(), 1, "no edge published on mismatch");
        assert!(replay
            .apply(0, &action)
            .await
            .expect_err("no cached fallback")
            .starts_with("MaterializationPoisoned:"));
    }

    let mut uncertified = Oracle::new(None)
        .await
        .expect("failed first certification fixture");
    uncertified.fixture.store.async_refinement_control(
        "CREATE TRIGGER corrupt_first_certificate AFTER INSERT ON background_job_reservations BEGIN DELETE FROM background_job_reservations WHERE job_id = NEW.job_id; END;".into()
    ).await.expect("deliberate test-only resource loss");
    let error = uncertified
        .apply(0, &action)
        .await
        .expect_err("novel successor needs independent invariants");
    assert!(error.starts_with("ReservationConservation:"), "{error}");
    assert!(uncertified.materialization_failed);
    assert_eq!(uncertified.states.len(), 1);
    assert!(uncertified.edges.is_empty());
    assert!(uncertified.cache.is_empty());
    assert_eq!(
        canonical(&uncertified.fixture, &Episode::initial()).await,
        uncertified.states[0].canonical,
        "failed certification still rolls back actual SQL"
    );

    let mut hidden = Oracle::new(None).await.expect("omitted-state fixture");
    let claim = hidden.apply(0, &action).await.expect("cached claim");
    let parent = hidden.edges[claim].to;
    hidden
        .apply(parent, &Action::ReadClaimJob { job: 0 })
        .await
        .expect("retained parent");
    hidden
        .fixture
        .store
        .async_refinement_control(
            "INSERT INTO sqlite_sequence(name,seq) VALUES('dv_title_jobs_demand',314159);".into(),
        )
        .await
        .expect("deliberate omitted sequence perturbation");
    assert_eq!(
        canonical(&hidden.fixture, &hidden.states[parent].episode).await,
        hidden.states[parent].canonical,
        "the original nine-table canonical comparison cannot see omitted-state drift"
    );
    let error = hidden
        .apply(parent, &Action::ReadClaimJob { job: 1 })
        .await
        .expect_err("retained parent must reject hidden drift");
    assert!(error.starts_with("OmittedDvState:"), "{error}");
    assert!(hidden.materialization_failed);
    assert!(hidden
        .apply(0, &action)
        .await
        .expect_err("no cached fallback after hidden drift")
        .starts_with("MaterializationPoisoned:"));

    let mut reset = Oracle::new(None)
        .await
        .expect("root-reset omitted-state fixture");
    reset
        .fixture
        .store
        .async_refinement_control(
            "INSERT INTO sqlite_sequence(name,seq) VALUES('dv_title_jobs_demand',314159);".into(),
        )
        .await
        .expect("deliberate root omitted-state drift");
    assert_eq!(
        canonical(&reset.fixture, &Episode::initial()).await,
        reset.states[0].canonical
    );
    assert!(reset
        .restore_root()
        .await
        .expect_err("root reset must check omitted state")
        .starts_with("OmittedDvState:"));
    assert!(reset.materialization_failed);
}
