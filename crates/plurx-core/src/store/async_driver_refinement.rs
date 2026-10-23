//! Actual common-SQL conformance for the async library-worker boundary.
//!
//! The graph covers only explicitly bounded selected-protocol inputs when
//! its BFS closes; predicate-partition cases are separate conformance.
//! Neither result proves arbitrary production histories or SQLite/Raft internals.
//! QueueSql below delegates to the real backend; it never copies production
//! transition SQL, validation, schema, or trigger definitions.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::OnceLock;

use serde::Serialize;
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
}

impl Fixture {
    async fn new() -> Self {
        Self::seed(SqliteStore::open_in_memory().expect("production SQLite schema")).await
    }

    async fn seed(store: SqliteStore) -> Self {
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
struct Episode {
    now: i64,
    claims: [u8; 2],
    tokens: [Vec<JobToken>; 2],
    renewed_claims: Vec<String>,
    leases: Vec<Lease>,
    domain_acquired: [bool; 3],
    removed: [bool; 2],
}

impl Episode {
    fn initial() -> Self {
        Self {
            now: NOW,
            claims: [0; 2],
            tokens: [vec![], vec![]],
            renewed_claims: vec![],
            leases: vec![],
            domain_acquired: [false; 3],
            removed: [false; 2],
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Action {
    Claim {
        job: usize,
        node: usize,
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
            if episode.tokens[job][token + 1..]
                .iter()
                .any(|later| later.claim_id == captured.claim_id)
            {
                continue;
            }
            if !episode.renewed_claims.contains(&captured.claim_id) {
                result.push(Action::Renew { job, token });
            }
            for disposition in 0..3 {
                result.push(Action::Settle {
                    job,
                    token,
                    disposition,
                });
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
    apply(fixture, episode, action, false).await
}

async fn apply(
    fixture: &Fixture,
    episode: &mut Episode,
    action: &Action,
    observe_refusals: bool,
) -> Result<(String, Option<usize>), String> {
    let now = episode.now;
    // The BFS already compares every refused edge's full canonical state.
    // Direct conformance callers can enable the same observation here;
    // logical resolution always checks durable rows independently of captures.
    let before_rows = if matches!(action, Action::Resolve { .. })
        || (observe_refusals
            && !matches!(
                action,
                Action::RemoveOwner { .. } | Action::ClockAdvance { .. }
            )) {
        Some(snapshot(&fixture.store).await)
    } else {
        None
    };
    let mut resolved_token = None;
    let result = match *action {
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
                    remember(
                        &mut episode.tokens[job],
                        claimed.token.expect("claimed token"),
                    );
                    episode.claims[job] += 1;
                    "claimed"
                }
                ClaimOutcome::Contended => "refused",
                ClaimOutcome::Cancelled => "cancelled",
                ClaimOutcome::ExpiredOrPruned => "expired_or_pruned",
                other => return Err(format!("UnexpectedClaimOutcome: {other:?}")),
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
    // Observe durable no-apply behavior here, not merely when BFS discovers
    // a new state or reaches its cap. Resolution may append a local capture,
    // but every actual API read must leave all durable snapshot rows equal.
    let read_only = matches!(action, Action::Resolve { .. });
    let refused = matches!(
        result,
        "refused" | "cancelled" | "expired_or_pruned" | "held"
    );
    if read_only || (observe_refusals && refused) {
        let before = before_rows.as_ref().expect("guarded primitive preimage");
        if snapshot(&fixture.store).await != *before {
            return Err(if read_only {
                "ResolutionReadOnly: logical resolution changed durable rows".into()
            } else {
                "FailedCasAtomicity: refused primitive changed durable rows".into()
            });
        }
    }
    Ok((result.into(), resolved_token))
}

async fn invariants(fixture: &Fixture, episode: &Episode) -> Result<(), String> {
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

async fn canonical(fixture: &Fixture, episode: &Episode) -> String {
    let mut key =
        serde_json::to_string(&json!({"rows": snapshot(&fixture.store).await, "episode": episode}))
            .expect("canonical JSON");
    // UUIDs are opaque equality-only identifiers in D; no numeric fence,
    // revision, expiry, timestamp, order, or gap is renamed.
    for (index, job) in fixture.jobs.iter().enumerate() {
        key = key.replace(job, &format!("J{index}"));
    }
    key
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
            "failed_attempts": row.failed_attempts, "active": row.token.as_ref().is_some_and(|token| row.state == JobState::Running && token.lease_expires_ms > episode.now),
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
        cutoffs.push("no_future_boundary".into());
    }
    json!({"id": id, "now": episode.now, "jobs": jobs, "domain": domain_row(fixture).await,
        "captured_tokens": episode.tokens, "captured_leases": episode.leases,
        "budgets": {"claims": episode.claims, "renewed_claims": episode.renewed_claims,
            "domain_acquired": episode.domain_acquired, "removed": episode.removed},
        "cutoffs": cutoffs, "canonical_sha256": format!("{:x}", Sha256::digest(key.as_bytes()))})
}

#[derive(Clone, Copy)]
enum TriggerMutation {
    CompletionRetirement,
    ReservationRelease,
}

async fn install_mutation(fixture: &Fixture, mutation: TriggerMutation) {
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

fn diagnostic_action(encoded: String) -> Value {
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

async fn build_graph(mutation: Option<TriggerMutation>) -> Result<Value, String> {
    let fixture = Fixture::new().await;
    if let Some(mutation) = mutation {
        install_mutation(&fixture, mutation).await;
    }
    let initial = Episode::initial();
    let first = canonical(&fixture, &initial).await;
    let mut index = HashMap::from([(first.clone(), 0usize)]);
    let mut states = vec![projection(&fixture, &initial, 0, &first).await];
    let mut frontier = VecDeque::from([(0usize, Vec::<Action>::new())]);
    let mut edges = Vec::new();
    let mut by_op = BTreeMap::<String, usize>::new();
    let mut by_result = BTreeMap::<String, usize>::new();
    let mut expanded_states = 0usize;
    while let Some((from, trace)) = frontier.pop_front() {
        fixture
            .store
            .async_refinement_control("SAVEPOINT async_refinement_materialize;".into())
            .await
            .expect("materialization savepoint");
        let mut episode = initial.clone();
        for action in &trace {
            apply_graph(&fixture, &mut episode, action).await?;
        }
        let key = canonical(&fixture, &episode).await;
        if index.get(&key) != Some(&from) {
            return Err(
                "MaterializationRoundTrip: replay changed canonical concrete rows/tokens".into(),
            );
        }
        invariants(&fixture, &episode).await?;
        for action in actions(&fixture, &episode).await {
            fixture
                .store
                .async_refinement_control("SAVEPOINT async_refinement_edge;".into())
                .await
                .expect("edge savepoint");
            let mut successor = episode.clone();
            let (result, resolved_token) = apply_graph(&fixture, &mut successor, &action).await?;
            let next = canonical(&fixture, &successor).await;
            // Source-specific no-apply results only: claim's cancelled/
            // pruned cases return from its read-only precheck; other refused
            // cases are zero-row CAS or an ephemeral renewal command. Held
            // is the backend acquire CAS refusal plus a read-only reread.
            if matches!(
                result.as_str(),
                "refused" | "cancelled" | "expired_or_pruned" | "held"
            ) && next != key
            {
                return Err(
                    "FailedCasAtomicity: refused operation changed durable rows or captured state"
                        .into(),
                );
            }
            let to = if let Some(to) = index.get(&next) {
                *to
            } else {
                if states.len() >= STATE_CAP {
                    eprintln!(
                        "async_store_cap_diagnostic={}",
                        state_cap_diagnostic(&CapDiagnostic {
                            counts: [states.len(), edges.len(), frontier.len(), expanded_states],
                            from,
                            trace: &trace,
                            predecessor: (&episode, &key),
                            rejected: (&successor, &next, &action),
                        })
                    );
                    return Err(format!("DomainStateCap: closure exceeded {STATE_CAP} concrete states; no closed relation emitted"));
                }
                invariants(&fixture, &successor).await?;
                let to = states.len();
                index.insert(next.clone(), to);
                states.push(projection(&fixture, &successor, to, &next).await);
                let mut path = trace.clone();
                path.push(action.clone());
                frontier.push_back((to, path));
                to
            };
            for (previous, successor) in states[from]["jobs"]
                .as_array()
                .expect("previous jobs")
                .iter()
                .zip(states[to]["jobs"].as_array().expect("successor jobs"))
            {
                if successor["fence"].as_i64().expect("successor fence")
                    < previous["fence"].as_i64().expect("previous fence")
                {
                    return Err("MonotoneFence: graph edge decreased a durable job fence".into());
                }
            }
            if states[from]["domain"].is_object()
                && states[to]["domain"].is_object()
                && states[to]["domain"]["fence"]
                    .as_u64()
                    .expect("successor domain fence")
                    < states[from]["domain"]["fence"]
                        .as_u64()
                        .expect("previous domain fence")
            {
                return Err("MonotoneFence: graph edge decreased a durable domain fence".into());
            }
            let action_json = serde_json::to_value(&action).expect("action JSON");
            let op = action_json["op"]
                .as_str()
                .expect("string in typed JSON projection")
                .to_owned();
            *by_op.entry(op).or_default() += 1;
            *by_result.entry(result.clone()).or_default() += 1;
            edges.push(json!({"from": from, "to": to, "action": action_json, "result": result, "resolved_token": resolved_token}));
            fixture
                .store
                .async_refinement_control(
                    "ROLLBACK TO async_refinement_edge; RELEASE async_refinement_edge;".into(),
                )
                .await
                .expect("edge rollback must not be skipped");
            if canonical(&fixture, &episode).await != key {
                return Err("RollbackRoundTrip: edge rollback changed concrete state".into());
            }
        }
        fixture
            .store
            .async_refinement_control(
                "ROLLBACK TO async_refinement_materialize; RELEASE async_refinement_materialize;"
                    .into(),
            )
            .await
            .expect("materialization rollback must not be skipped");
        if canonical(&fixture, &initial).await != first {
            return Err("RollbackRoundTrip: materialization rollback changed Init".into());
        }
        expanded_states += 1;
    }
    for op in [
        "claim",
        "renew",
        "settle",
        "resolve",
        "bind",
        "complete",
        "acquire_domain",
        "release_domain",
        "remove_owner",
        "clock_advance",
    ] {
        if !by_op.contains_key(op) {
            return Err(format!("DomainCoverage: missing operation {op}"));
        }
    }
    let mut cutoff_counts = BTreeMap::<String, usize>::new();
    for state in &states {
        for cutoff in state["cutoffs"]
            .as_array()
            .expect("array in typed JSON projection")
        {
            *cutoff_counts
                .entry(
                    cutoff
                        .as_str()
                        .expect("string in typed JSON projection")
                        .into(),
                )
                .or_default() += 1;
        }
    }
    let mut domain = json!({"jobs": 2, "nodes": NODES, "physical_capacity_per_node": 1,
            "attempts_per_job": ATTEMPTS_PER_JOB, "renewals_per_attempt": 1, "rearrival": false,
            "ordinary_token_inputs": "latest captured revision per claim; queue-token Mutex adapter contract; historical revisions remain captured and conformance-tested",
            "binding_completion_inputs": "captured domain lease owner equals captured queue-token node; wrong-owner inputs are separate conformance",
            "settlement_dispositions": ["yield", "retry", "cancel", "retirement_yield"],
            "node_domain_acquisition_inputs": "node has selected running durable queue ownership, including expired retained ownership for dispatch/application races; foreign holder is independent",
            "domain_acquisitions_per_owner": 1, "domain_owners": DOMAIN_OWNERS,
            "selected_attempt_ids": 4, "reserved_boundary_ids": 2,
            "allocator_exhaustion": "outside D: the attempt budget is exhausted before the six-ID namespace",
            "numeric_quotient": "none: every i64/u64 and captured token remains exact; opaque job UUIDs alone are renamed",
            "clock": "next concrete captured/current authority expiry minus one/equality/plus one; no horizon clipping",
            "publication": "opaque conformance boundary; no item-insertion SQL or retry proof in this relation",
            "reply_faults": "split application/reply/reconciliation in temporal composition; separate bridge-boundary conformance histories",
            "cap": STATE_CAP,
            "premises": ["completion opportunity before attempt/domain/fault budget exhaustion", "two fixed selected requests; no infinite fresh-progress conclusion"],
            "savepoints": "same production writer connection, sequential harness; transaction/commit visibility and SQLite internals remain assumptions; any SQL/ABORT/ROLLBACK error aborts closure"});
    let boundary_metadata = json!({
        "retirement_inputs": "disposition 3 uses Some(worker_retired), None checkpoint and not_before_ms=current episode.now for every captured original token; ordinary dispositions 0/1/2 retain latest-revision inputs",
        "resolution_read_boundary": "resolve_claim is one logical actual-API pure-read action; its two authoritative QueueSql awaits and internal interleavings are a named conformance boundary, not closed read-interleaving coverage",
        "primitive_clock_boundary": "each generated primitive request uses current episode.now; the daemon's three retirement wall samples and dispatch/application races are reviewed adapter/conformance boundaries, not source-derived clock-race coverage",
        "reservation_conservation_boundary": "conservation is checked for unexpired queue authority only; expired Running rows and residue remain represented, with no independent complete-residue conservation guarantee until reclaim/prune",
        "domain_reacquisition_boundary": "each owner can acquire at most once; release or expiry does not reset its budget, so same-owner release-to-reacquire cycles are excluded",
        "foreign_holder_boundary": "foreign-holder is a third adversarial domain-lease owner, not either worker node, with one independent acquisition budget",
        "retirement_sequence_boundary": "the graph supplies separate actual settle and logical resolve primitives; daemon drain/local-token/first-settle/resolve/optional-second-settle sequencing is a reviewed source and daemon-conformance boundary composed by the temporal model",
        "cleanup_boundary": "local cleanup needs bounded retirement calls to return, not eventual successful acknowledgement; errors are logged and finish returns while durable authority may remain",
        "snapshot_tables": SNAPSHOT_TABLES,
        "dispatch_time_noninterference": "for retirement fence Some(F), resolve_claim reads dispatched_at_ms only in its skipped fence-is-none window branch; neither SQL query nor later classification reads it, so a representative dispatch sample preserves results at fixed now/read outcomes; this is a reviewed source lemma, not a machine proof",
        "queue_sql_boundary": "all generated queue primitives use QueueSql::queue_sql, not queue_transaction; ReplyBoundary passes queue_transaction through without fault injection, so multi-statement transaction faults are outside this slice",
        "schema_snapshot_boundary": "canonical durable rows cover exactly the nine snapshot_tables; all installed schema objects are hash-bound for identity, not complete row snapshots or proofs of unrelated tables",
        "savepoint_boundary": "no closure transition invokes queue_transaction or nested explicit transactions; test savepoints bracket sequential primitives on the production writer and any savepoint/SQL failure aborts closure",
        "refusal_observation": "the existing BFS full-canonical equality guard already rejects every refused edge mutation; direct non-graph conformance can additionally enable apply's durable snapshot observer; no pre-existing closure gap is claimed",
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
    let graph = json!({"schema_version": 1, "scope": "closed bounded selected-protocol actual-SQL relation, not arbitrary production histories",
        "initial": 0, "source_bindings": source_bindings(), "schema_bindings": schema_bindings(&fixture).await,
        "domain": domain,
        "coverage": {"closed": true, "no_skipped_edges": true, "state_count": states.len(), "edge_count": edges.len(),
            "by_op": by_op, "by_result": by_result, "cutoff_counts": cutoff_counts},
        "states": states, "edges": edges});
    let mut graph = serde_json::to_string(&graph).expect("graph JSON");
    for (index, job) in fixture.jobs.iter().enumerate() {
        graph = graph.replace(job, &format!("J{index}"));
    }
    Ok(serde_json::from_str(&graph).expect("renamed graph JSON"))
}

#[tokio::test]
async fn actual_sql_bounded_episode_graph_is_closed() {
    let graph = build_graph(None)
        .await
        .expect("actual SQL closure and named invariants");
    if let Ok(path) = std::env::var("PLURX_ASYNC_STORE_GRAPH") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&graph).expect("graph output"),
        )
        .expect("write graph");
    }
    eprintln!("async_store_graph_coverage={}", graph["coverage"]);
}

#[tokio::test]
async fn closure_completion_trigger_mutation_fails_last_request_atomic_retirement() {
    let failure = build_graph(Some(TriggerMutation::CompletionRetirement))
        .await
        .expect_err("C4 mutated closure must be rejected");
    assert!(
        failure.starts_with("LastRequestAtomicRetirement"),
        "C4 observed {failure}"
    );
    eprintln!(
        "async_store_control={}",
        json!({"control": "C4", "named_failure": failure})
    );
}

#[tokio::test]
async fn closure_reservation_release_mutation_fails_reservation_conservation() {
    let failure = build_graph(Some(TriggerMutation::ReservationRelease))
        .await
        .expect_err("C9 mutated closure must be rejected");
    assert!(
        failure.starts_with("ReservationConservation"),
        "C9 observed {failure}"
    );
    eprintln!(
        "async_store_control={}",
        json!({"control": "C9", "named_failure": failure})
    );
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
    eprintln!(
        "async_store_refusal_guard={}",
        json!({"named_failure": failure, "detected_in": "apply"})
    );
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
