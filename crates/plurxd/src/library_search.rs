//! Background metadata classification and optional local semantic search.
pub(crate) mod semantic;
use crate::{
    http::{
        error::ApiError,
        extract::{AdminUser, AuthUser},
    },
    state::AppState,
};
use axum::{
    extract::{Path, State},
    Json,
};
use plurx_core::{
    metadata::{
        classification::{self, Overrides},
        TmdbClient,
    },
    store::{
        classification::{Entry, Record, PROVIDER_REFRESH_SECS, PROVIDER_RETRY_SECS},
        classification_schedule::{
            self as schedule, ClassificationSchedule, Decision, ScheduleOutcome,
        },
        keys,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock, RwLock,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
pub const SEMANTIC_KEY: &str = "search.semantic.enabled";
#[derive(Default, Clone, Serialize)]
pub struct Status {
    pub processed: usize,
    pub labelled: usize,
    pub provider_error: Option<String>,
    pub scan_complete: bool,
}
static STATUS: OnceLock<RwLock<Status>> = OnceLock::new();
fn observation() -> &'static RwLock<Status> {
    STATUS.get_or_init(Default::default)
}
fn now() -> i64 {
    crate::media_sessions::unix_ms() / 1000
}
/// How the classification pass holds its lease, and whose clock it reads.
/// Production uses the ordinary cluster-job TTL and heartbeat and the wall
/// clock; the failover test shortens the lease.
#[derive(Clone)]
pub(crate) struct ClassificationPolicy {
    pub(crate) ttl: Duration,
    pub(crate) heartbeat: Duration,
    /// How often a node looks at a lease a peer visibly holds.
    pub(crate) lease_retry: Duration,
    /// Unix milliseconds, compared with the lease row's expiry.
    pub(crate) clock: std::sync::Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl ClassificationPolicy {
    pub(crate) fn production() -> Self {
        Self {
            ttl: crate::job_lease::JOB_LEASE_TTL,
            heartbeat: crate::job_lease::JOB_LEASE_HEARTBEAT,
            lease_retry: schedule::LEASE_RETRY,
            clock: std::sync::Arc::new(crate::media_sessions::unix_ms),
        }
    }
}

/// Per-worker counts. Production reads only the process-wide `TICKS`; the
/// tests read these to count what one worker did.
#[derive(Default)]
pub(crate) struct WorkerStats {
    /// `acquire_job` calls: each one is an `acquire_lease` proposal.
    pub(crate) lease_attempts: AtomicU64,
    pub(crate) passes_completed: AtomicU64,
    pub(crate) pages: AtomicU64,
}

/// `plurx_classification_ticks_total{outcome}`: why each scheduling decision
/// did or did not start a pass. Five fixed values.
static TICKS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

pub(crate) fn prometheus() -> String {
    let mut out = String::from(
        "# HELP plurx_classification_ticks_total Metadata classification scheduling decisions by outcome.\n\
         # TYPE plurx_classification_ticks_total counter\n",
    );
    for outcome in ScheduleOutcome::ALL {
        out.push_str(&format!(
            "plurx_classification_ticks_total{{outcome=\"{}\"}} {}\n",
            outcome.as_str(),
            TICKS[outcome.index()].load(Ordering::Relaxed)
        ));
    }
    out
}

pub async fn worker(state: AppState, shutdown: CancellationToken) {
    worker_with_policy(
        state,
        shutdown,
        ClassificationPolicy::production(),
        std::sync::Arc::new(WorkerStats::default()),
    )
    .await;
}

/// Classify the library in passes (K-10 §3.2).
///
/// One pass holds the `metadata-classification` cluster lease from its first
/// page to its last, where the worker used to take and release it around
/// every page. When a pass is worth starting is [`ClassificationSchedule`]'s
/// decision, from two local reads that never authorize anything: a lease a
/// peer visibly holds is not contested, and a library whose local replica
/// shows nothing to classify is not walked until the forced pass is owed.
/// The pass itself is unchanged: every page is read on the authority at the
/// same 1 s cadence, and every write stays fenced on revision and source.
pub(crate) async fn worker_with_policy(
    state: AppState,
    shutdown: CancellationToken,
    policy: ClassificationPolicy,
    stats: std::sync::Arc<WorkerStats>,
) {
    let mut schedule =
        ClassificationSchedule::with_lease_retry((policy.clock)(), policy.lease_retry);
    let mut cursor = 0;
    loop {
        let delay = if state.jobs.may_run_cluster_jobs().await {
            let decision = schedule
                .decide(state.store.as_ref(), (policy.clock)())
                .await;
            TICKS[decision.outcome().index()].fetch_add(1, Ordering::Relaxed);
            match decision {
                Decision::Wait(delay, _) => delay,
                Decision::Pass(_) => {
                    run_pass(
                        &state,
                        &shutdown,
                        &policy,
                        &stats,
                        &mut schedule,
                        &mut cursor,
                    )
                    .await
                }
            }
        } else {
            schedule::PASS_GAP
        };
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

/// Take the lease and walk the library from `cursor` to its end, one page
/// per [`schedule::PAGE_TICK`]. Returns how long to wait before the next
/// decision.
async fn run_pass(
    state: &AppState,
    shutdown: &CancellationToken,
    policy: &ClassificationPolicy,
    stats: &WorkerStats,
    schedule: &mut ClassificationSchedule,
    cursor: &mut i64,
) -> Duration {
    stats.lease_attempts.fetch_add(1, Ordering::Relaxed);
    let lease = match state
        .jobs
        .acquire_job_with_policy(schedule::RESOURCE.to_owned(), policy.ttl, policy.heartbeat)
        .await
    {
        Ok(Some(lease)) => lease,
        // Held by a peer the local replica had not shown yet, or this node
        // lost its vote since the decision.
        Ok(None) => return policy.lease_retry,
        Err(e) => {
            tracing::warn!(error=?e,"Metadata classification lease could not be taken");
            return policy.lease_retry;
        }
    };
    let lost = lease.loss_token();
    let mut did_work = false;
    let finished = loop {
        if lost.is_cancelled() || !state.jobs.may_run_cluster_jobs().await {
            break false;
        }
        stats.pages.fetch_add(1, Ordering::Relaxed);
        match classify_page(state, cursor).await {
            Ok(page) => {
                did_work |= page.worked();
                if page.complete {
                    break true;
                }
            }
            Err(e) => {
                tracing::warn!(error=?e,"Metadata classification pass failed");
                break false;
            }
        }
        tokio::select! {
            _ = shutdown.cancelled() => break false,
            _ = lost.cancelled() => break false,
            _ = tokio::time::sleep(schedule::PAGE_TICK) => {}
        }
    };
    // Best-effort: the lease has a bounded expiry and a failed early release
    // delays, but cannot lose, the next classification pass.
    if let Err(e) = lease.release().await {
        tracing::debug!(error=?e,"Releasing the metadata classification lease");
    }
    if finished {
        stats.passes_completed.fetch_add(1, Ordering::Relaxed);
        schedule.pass_finished((policy.clock)(), did_work);
        Duration::ZERO
    } else {
        // Lost, failed or no longer a voter: the cursor is kept, and the next
        // decision sees who holds the lease now.
        schedule::PAGE_TICK
    }
}

/// What one page did.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct PageReport {
    /// The page was empty: the pass reached the end of the library.
    pub(crate) complete: bool,
    pub(crate) written: usize,
    pub(crate) provider_requests: usize,
}

impl PageReport {
    fn worked(self) -> bool {
        self.written > 0 || self.provider_requests > 0
    }
}

pub(crate) async fn classify_page(
    state: &AppState,
    cursor: &mut i64,
) -> Result<PageReport, ApiError> {
    let mut report = PageReport::default();
    let entries = state.store.classification_page(*cursor, 32).await?;
    if entries.is_empty() {
        *cursor = 0;
        if let Ok(mut o) = observation().write() {
            o.scan_complete = true;
        }
        report.complete = true;
        return Ok(report);
    }
    if *cursor == 0 {
        if let Ok(mut o) = observation().write() {
            *o = Status::default();
        }
    }
    let key = state
        .store
        .get_setting(keys::TMDB_API_KEY)
        .await?
        .filter(|v| !v.is_empty());
    let provider = key.map(TmdbClient::new);
    let mut requests = 0;
    for entry in entries {
        let input = entry.input()?;
        *cursor = input.id;
        let old = entry.record.as_ref();
        let mut keywords = old
            .map(|r| r.classification.keywords.clone())
            .unwrap_or_default();
        let identity_changed = old
            .and_then(|r| {
                serde_json::from_str::<plurx_core::store::classification::Input>(&r.source_json)
                    .ok()
            })
            .is_some_and(|previous| {
                previous.tmdb_id != input.tmdb_id || previous.kind != input.kind
            });
        if identity_changed {
            keywords.clear();
        }
        let mut checked = old
            .map(|r| r.classification.provider_checked_at)
            .unwrap_or(0);
        let mut error = old.and_then(|r| r.classification.provider_error.clone());
        if identity_changed {
            checked = 0;
            error = None;
        }
        // The same rule `classification::hint_sql` mirrors on the replica.
        let due = now() - checked
            > if error.is_some() {
                PROVIDER_RETRY_SECS
            } else {
                PROVIDER_REFRESH_SECS
            };
        if requests < 4 && due && matches!(input.kind.as_str(), "movie" | "show") {
            if let (Some(provider), Some(id)) = (&provider, input.tmdb_id) {
                requests += 1;
                report.provider_requests += 1;
                checked = now();
                match tokio::time::timeout(Duration::from_secs(15), provider.keywords(id, &input.kind)).await {
                    Ok(Ok(values)) => { keywords=values; error=None; }
                    Ok(Err(_)) => error=Some("Metadata keyword provider could not be reached; stored labels remain available.".into()),
                    Err(_) => error=Some("Metadata keyword request timed out; stored labels remain available.".into()),
                }
            }
        }
        let unchanged = entry.indexed
            && old.is_some_and(|r| {
                r.source_json == entry.source_json
                    && r.classification.version == classification::VERSION
                    && r.classification.provider_checked_at == checked
            });
        let mut result = classification::classify(&input.metadata(), keywords);
        let labelled = !result
            .terms(&old.map(|r| r.overrides.clone()).unwrap_or_default())
            .is_empty();
        if !unchanged {
            result.provider_checked_at = checked;
            result.provider_error = error.clone();
            let record = Record {
                source_json: entry.source_json,
                classification: result,
                overrides: old.map(|r| r.overrides.clone()).unwrap_or_default(),
                revision: old.map(|r| r.revision).unwrap_or(0),
            };
            if state.jobs.may_run_cluster_jobs().await
                && state.store.write_classification(input.id, &record).await?
            {
                report.written += 1;
            }
        }
        if let Ok(mut o) = observation().write() {
            o.processed += 1;
            if error.is_some() {
                o.provider_error = error;
            }
            if labelled {
                o.labelled += 1;
            }
        }
    }
    Ok(report)
}
async fn entry(state: &AppState, id: i64) -> Result<Entry, ApiError> {
    state
        .store
        .classification_page(id.saturating_sub(1), 1)
        .await?
        .into_iter()
        .find(|e| e.input().is_ok_and(|i| i.id == id))
        .ok_or(ApiError::NotFound("media item"))
}
pub async fn get_classification(
    AuthUser(_): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let e = entry(&state, id).await?;
    let stale = e.record.as_ref().is_none_or(|r| {
        r.source_json != e.source_json || r.classification.version != classification::VERSION
    });
    Ok(Json(
        serde_json::json!({"classification":e.record,"pending":stale}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Correction {
    pub expected_revision: i64,
    #[serde(flatten)]
    pub overrides: Overrides,
}
pub async fn correct_classification(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(mut request): Json<Correction>,
) -> Result<Json<serde_json::Value>, ApiError> {
    request.overrides.validate().map_err(ApiError::BadRequest)?;
    let e = entry(&state, id).await?;
    let input = e.input()?;
    let record = e.record;
    let revision = record.as_ref().map(|r| r.revision).unwrap_or(0);
    if revision != request.expected_revision {
        return Err(ApiError::Conflict(
            "Classification changed; reload before correcting it.".into(),
        ));
    }
    let generated = record
        .filter(|r| r.source_json == e.source_json)
        .map(|r| r.classification)
        .unwrap_or_else(|| classification::classify(&input.metadata(), Vec::new()));
    let r = Record {
        source_json: e.source_json,
        classification: generated,
        overrides: request.overrides,
        revision,
    };
    if !state.store.write_classification(id, &r).await? {
        return Err(ApiError::Conflict(
            "Metadata changed; reload before correcting it.".into(),
        ));
    }
    Ok(Json(serde_json::json!({"revision":revision+1})))
}
pub async fn settings(
    AuthUser(_): AuthUser,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let enabled = state.store.get_setting(SEMANTIC_KEY).await?.as_deref() == Some("true");
    let status = observation().read().map(|v| v.clone()).unwrap_or_default();
    Ok(Json(
        serde_json::json!({"semantic_enabled":enabled,"classification":status,"semantic":semantic::status()}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub semantic_enabled: bool,
}
pub async fn update_settings(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Json(request): Json<Settings>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .store
        .put_setting(
            SEMANTIC_KEY,
            if request.semantic_enabled {
                "true"
            } else {
                "false"
            },
        )
        .await?;
    if !request.semantic_enabled {
        semantic::disable();
    }
    Ok(Json(
        serde_json::json!({"semantic_enabled":request.semantic_enabled}),
    ))
}

#[derive(Deserialize)]
pub struct RelatedQuery {
    pub q: Option<String>,
}
pub async fn related_search(
    AuthUser(_): AuthUser,
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<RelatedQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ids = semantic::related(&state, &query.q.unwrap_or_default()).await;
    let mut results = Vec::new();
    for id in ids {
        if let Some(item) = state.store.get_item(id).await? {
            results.push(crate::http::dto::ItemDto::from(item));
        }
    }
    Ok(Json(
        serde_json::json!({"results":results,"semantic":semantic::status()}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::cluster::coordination::LeaseClaim;
    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary};
    use plurx_core::store::{SqliteStore, Store};
    use std::sync::Arc;

    fn state_on(store: Arc<dyn Store>, node: &str) -> AppState {
        let root = crate::test_temp_path(format!("plurx-classification-{}", uuid::Uuid::new_v4()));
        AppState::new(
            "test".to_owned(),
            store,
            crate::state::Dirs {
                artwork: root.join("artwork"),
                transcode: root.join("transcode"),
                cache: root.join("cache"),
                subs: root.join("subs"),
                runtime_cache: root.join("runtime"),
                renditions: root.join("renditions"),
            },
            node.to_owned(),
            Default::default(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(64)),
        )
    }

    async fn seed(store: &Arc<dyn Store>, count: usize) -> i64 {
        let library = store
            .create_library(&NewLibrary {
                name: "Classified".into(),
                kind: LibraryKind::Movies,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("library");
        for index in 0..count {
            store
                .insert_item(&NewItem {
                    library_id: library.id,
                    kind: ItemKind::Movie,
                    parent_id: None,
                    title: format!("Film {index}"),
                    year: Some(2001),
                    season_number: None,
                    episode_number: None,
                })
                .await
                .expect("item");
        }
        library.id
    }

    async fn unclassified(store: &Arc<dyn Store>) -> usize {
        store
            .classification_page(0, 256)
            .await
            .expect("page")
            .iter()
            .filter(|entry| entry.record.is_none())
            .count()
    }

    /// A clock that moves with Tokio's, so a paused test can step the
    /// schedule's Unix-millisecond view of time as well as its sleeps.
    fn virtual_clock() -> Arc<dyn Fn() -> i64 + Send + Sync> {
        let base = crate::media_sessions::unix_ms();
        let start = tokio::time::Instant::now();
        Arc::new(move || base + i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX))
    }

    fn paused_policy() -> ClassificationPolicy {
        ClassificationPolicy {
            clock: virtual_clock(),
            ..ClassificationPolicy::production()
        }
    }

    async fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) -> Duration {
        let start = tokio::time::Instant::now();
        while !done() {
            assert!(start.elapsed() < limit, "{what} took longer than {limit:?}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        start.elapsed()
    }

    /// K-10 §3.2: a pass holds the lease from its first page to its last.
    ///
    /// Seventy items are three pages and an end-of-library read. The worker
    /// used to acquire and release `metadata-classification` around every one
    /// of them; now one acquire covers the pass and one release ends it.
    #[tokio::test(start_paused = true)]
    async fn classification_one_pass_holds_one_lease_for_every_page() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        seed(&store, 70).await;
        let stats = Arc::new(WorkerStats::default());
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(worker_with_policy(
            state_on(Arc::clone(&store), "node-a"),
            shutdown.clone(),
            paused_policy(),
            Arc::clone(&stats),
        ));
        wait_for("the first pass", Duration::from_secs(60), || {
            stats.passes_completed.load(Ordering::SeqCst) >= 1
        })
        .await;
        assert_eq!(unclassified(&store).await, 0);
        assert_eq!(
            stats.pages.load(Ordering::SeqCst),
            4,
            "three pages and the end"
        );
        assert_eq!(
            stats.lease_attempts.load(Ordering::SeqCst),
            1,
            "one lease for the whole pass, not one per page"
        );
        shutdown.cancel();
        let _ = task.await;
    }

    /// K-10 §3.2: an idle library is not walked and its lease not taken until
    /// something changes, and a change is classified within the idle
    /// ceiling.
    ///
    /// After the first pass, ten minutes of a classified library take no
    /// lease and read no page (the hint says nothing, and the forced pass is
    /// thirty minutes away). A new item then makes the local hint fire, and
    /// it is classified within the idle ceiling plus its pass.
    #[tokio::test(start_paused = true)]
    async fn classification_an_idle_library_takes_no_lease_until_it_changes() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let library = seed(&store, 40).await;
        let stats = Arc::new(WorkerStats::default());
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(worker_with_policy(
            state_on(Arc::clone(&store), "node-a"),
            shutdown.clone(),
            paused_policy(),
            Arc::clone(&stats),
        ));
        wait_for("the first pass", Duration::from_secs(60), || {
            stats.passes_completed.load(Ordering::SeqCst) >= 1
        })
        .await;
        let pages = stats.pages.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(600)).await;
        assert_eq!(
            stats.lease_attempts.load(Ordering::SeqCst),
            1,
            "an idle library takes no lease"
        );
        assert_eq!(
            stats.pages.load(Ordering::SeqCst),
            pages,
            "an idle library reads no page on the authority"
        );

        store
            .insert_item(&NewItem {
                library_id: library,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "A late arrival".into(),
                year: Some(2002),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("new item");
        let start = tokio::time::Instant::now();
        while unclassified(&store).await > 0 {
            assert!(
                start.elapsed() <= schedule::IDLE_TICK_MAX + Duration::from_secs(5),
                "a new item waited longer than the idle ceiling and its pass"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert_eq!(stats.lease_attempts.load(Ordering::SeqCst), 2);
        shutdown.cancel();
        let _ = task.await;
    }

    /// K-10 §3.2: two workers on one store walk the library once.
    ///
    /// Both start fresh and owe a forced pass. One takes the lease; the other
    /// sees it held on its local read and leaves it alone, then sees the
    /// released row as the pass it no longer owes. Over ten minutes there is
    /// one completed pass, every entry is written exactly once (revision 1),
    /// and at most one contest lost a race to the first acquire.
    #[tokio::test(start_paused = true)]
    async fn classification_two_workers_walk_the_library_once() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        seed(&store, 40).await;
        let shutdown = CancellationToken::new();
        let stats_a = Arc::new(WorkerStats::default());
        let stats_b = Arc::new(WorkerStats::default());
        let a = tokio::spawn(worker_with_policy(
            state_on(Arc::clone(&store), "node-a"),
            shutdown.clone(),
            paused_policy(),
            Arc::clone(&stats_a),
        ));
        let b = tokio::spawn(worker_with_policy(
            state_on(Arc::clone(&store), "node-b"),
            shutdown.clone(),
            paused_policy(),
            Arc::clone(&stats_b),
        ));
        tokio::time::sleep(Duration::from_secs(600)).await;
        let passes = stats_a.passes_completed.load(Ordering::SeqCst)
            + stats_b.passes_completed.load(Ordering::SeqCst);
        assert_eq!(passes, 1, "one walk of an unchanged library");
        let attempts = stats_a.lease_attempts.load(Ordering::SeqCst)
            + stats_b.lease_attempts.load(Ordering::SeqCst);
        assert!(attempts <= 2, "{attempts} lease attempts");
        for entry in store.classification_page(0, 256).await.expect("page") {
            assert_eq!(entry.record.expect("classified").revision, 1);
        }
        shutdown.cancel();
        let _ = a.await;
        let _ = b.await;
    }

    /// K-10 §3.2: a dead peer's lease is taken over once it expires, and not
    /// contested before.
    ///
    /// Real time with a shortened lease: a peer took the lease and died. The
    /// worker reads the live row locally and proposes nothing until it
    /// lapses, then takes the lease and classifies the library.
    #[tokio::test]
    async fn classification_takes_over_a_dead_peers_lease_when_it_expires() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        seed(&store, 40).await;
        let now = crate::media_sessions::unix_ms();
        let LeaseClaim::Acquired(_dead) = store
            .acquire_lease(schedule::RESOURCE, "dead-node", now, now + 1_500)
            .await
            .expect("the peer's lease")
        else {
            panic!("an absent lease must be acquired");
        };
        let stats = Arc::new(WorkerStats::default());
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(worker_with_policy(
            state_on(Arc::clone(&store), "node-b"),
            shutdown.clone(),
            ClassificationPolicy {
                ttl: Duration::from_secs(2),
                heartbeat: Duration::from_millis(500),
                lease_retry: Duration::from_millis(200),
                clock: Arc::new(crate::media_sessions::unix_ms),
            },
            Arc::clone(&stats),
        ));
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert_eq!(
            stats.lease_attempts.load(Ordering::SeqCst),
            0,
            "a lease the local replica shows live is not contested"
        );
        let started = std::time::Instant::now();
        while unclassified(&store).await > 0 {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the successor never classified the library"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(stats.lease_attempts.load(Ordering::SeqCst), 1);
        shutdown.cancel();
        let _ = task.await;
    }
}
