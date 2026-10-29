//! Domain-test fixtures backed by the common queue. These helpers create
//! owned artifacts for cache/offline/history contracts; queue ownership itself
//! is tested directly through BackgroundJobStore with full JobTokens.
use plurx_core::cluster::coordination::Lease;
use plurx_core::domain::{NewPretranscodeJob, PretranscodeJob, PretranscodeWorkerCapabilities};
use plurx_core::error::StoreError;
use plurx_core::store::background_jobs::*;
use plurx_core::store::Store;

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct FragmentFailureFixture {
    pub cache_key: String,
    pub target_node_id: String,
    pub node_id: String,
    pub fence: i64,
    pub code: plurx_core::content_analysis::IndexFailureCode,
    pub transient_allowlisted: bool,
    pub diagnostic: plurx_core::content_analysis::IndexDiagnostic,
    pub now_ms: i64,
}

async fn transcode_token<T: Store + ?Sized>(
    store: &T,
    job: &PretranscodeJob,
) -> Result<Option<JobToken>, StoreError> {
    Ok(store
        .background_job(&job.id)
        .await?
        .and_then(|current| current.token)
        .filter(|token| {
            token.node_id == job.owner_node_id
                && token.fence == job.fence
                && token.lease_expires_ms == job.lease_expires_ms
        }))
}

#[allow(dead_code, clippy::too_many_arguments)]
pub(crate) trait QueueFixture: Store {
    async fn fixture_enqueue_pretranscode_job(
        &self,
        job: &NewPretranscodeJob,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError> {
        let mut request = plurx_core::store::background_jobs_pretranscode::enqueue_request(job)?;
        // Capability pagination deliberately seeds more than one candidate
        // page. Use manual fixture demand rather than the 64 automatic-intent
        // budget; the production budget is covered by its own queue contract.
        if job.dedupe_key.starts_with("claim-pagination-") {
            request.request.scope = "fixture:pagination".into();
        }
        // These domain fixtures often use synthetic claim clocks. Producer
        // publication itself uses wall time, so its demand must remain live.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| StoreError::Task(error.to_string()))?
            .as_millis() as i64;
        request.request.deadline_ms = Some(now.saturating_add(86_400_000));
        match self
            .enqueue_job_fenced(request, lease.clone(), replacement.clone())
            .await?
        {
            EnqueueOutcome::Accepted { .. } => Ok(true),
            EnqueueOutcome::ProducerFenced => Err(StoreError::FenceRejected {
                resource: lease.resource.clone(),
                owner_node_id: lease.owner_node_id.clone(),
                fence: lease.fence,
            }),
            _ => Ok(false),
        }
    }
    async fn fixture_claim_pretranscode_job(
        &self,
        node: &str,
        caps: &PretranscodeWorkerCapabilities,
        excluded: &[String],
        now_ms: i64,
        _old_lease_expiry: i64,
    ) -> Result<Option<PretranscodeJob>, StoreError> {
        if !caps.validate() {
            return Err(StoreError::Task("invalid worker capabilities".into()));
        }
        let mut after = None;
        loop {
            let page = self
                .job_candidates(CandidateQuery {
                    node_id: node.into(),
                    kinds: vec![JobKind::TranscodePrepare],
                    after,
                    now_ms,
                    limit: 128,
                })
                .await?;
            for candidate in page.jobs {
                if excluded.contains(&candidate.id) {
                    continue;
                }
                let JobPayload::TranscodePrepare {
                    requirements,
                    target_height,
                    ..
                } = candidate.supported_payload()?
                else {
                    continue;
                };
                if !requirements.compatible_with(caps)
                    || i64::from(target_height) > caps.max_target_height
                {
                    continue;
                }
                let outcome = self
                    .claim_job(ClaimJob {
                        job_id: candidate.id,
                        expected_revision: candidate.revision,
                        node_id: node.into(),
                        boot_id: uuid::Uuid::new_v4().to_string(),
                        claim_id: uuid::Uuid::new_v4().to_string(),
                        kind: JobKind::TranscodePrepare,
                        payload_version: 1,
                        now_ms,
                        dispatched_at_ms: now_ms,
                    })
                    .await?;
                if let ClaimOutcome::Claimed { job } = outcome {
                    return plurx_core::store::background_jobs_pretranscode::projection(&job)
                        .map(Some);
                }
            }
            after = page.next;
            if after.is_none() {
                return Ok(None);
            }
        }
    }
    async fn fixture_active_pretranscode_job_ids(&self) -> Result<Vec<String>, StoreError> {
        let mut ids = Vec::new();
        let mut after_id = None;
        loop {
            let page = self
                .list_jobs(JobQuery {
                    node_id: None,
                    state: None,
                    kind: Some(JobKind::TranscodePrepare),
                    after_id,
                    limit: 128,
                })
                .await?;
            ids.extend(
                page.jobs
                    .into_iter()
                    .filter(|j| {
                        matches!(
                            j.state,
                            JobState::Queued | JobState::Running | JobState::Cancelling
                        )
                    })
                    .map(|j| j.id),
            );
            after_id = page.next_after_id;
            if after_id.is_none() {
                return Ok(ids);
            }
        }
    }
    async fn fixture_renew_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        now_ms: i64,
        _old_lease_expiry: i64,
    ) -> Result<Option<PretranscodeJob>, StoreError> {
        let Some(token) = transcode_token(self, job).await? else {
            return Ok(None);
        };
        let results = self
            .renew_jobs(RenewJobs {
                tokens: vec![token],
                now_ms,
            })
            .await?;
        if !matches!(results.first(), Some(RenewOutcome::Renewed { .. })) {
            return Ok(None);
        }
        self.background_job(&job.id)
            .await?
            .map(|job| plurx_core::store::background_jobs_pretranscode::projection(&job))
            .transpose()
    }
    async fn fixture_yield_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        now_ms: i64,
        not_before_ms: i64,
    ) -> Result<bool, StoreError> {
        let Some(token) = transcode_token(self, job).await? else {
            return Ok(false);
        };
        self.settle_job(SettleJob {
            token,
            settlement: JobSettlement::Yield {
                error_code: None,
                checkpoint: None,
                not_before_ms,
            },
            now_ms,
        })
        .await
    }
    async fn fixture_fail_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        error_code: &str,
        now_ms: i64,
        not_before_ms: i64,
    ) -> Result<bool, StoreError> {
        let Some(token) = transcode_token(self, job).await? else {
            return Ok(false);
        };
        self.settle_job(SettleJob {
            token,
            settlement: JobSettlement::Retry {
                error_code: error_code.into(),
                not_before_ms,
            },
            now_ms,
        })
        .await
    }
    async fn fixture_cancel_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        error_code: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let Some(token) = transcode_token(self, job).await? else {
            return Ok(false);
        };
        self.settle_job(SettleJob {
            token,
            settlement: JobSettlement::Stop {
                error_code: error_code.into(),
            },
            now_ms,
        })
        .await
    }
    async fn fixture_complete_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        recipe_hash: &str,
        recipe_version: i64,
        relative_dir: &str,
        bytes: i64,
        expected_previous_bytes: Option<i64>,
        manifest_digest: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let Some(token) = transcode_token(self, job).await? else {
            return Ok(false);
        };
        let outcome = self
            .publish_transcode_job(PublishTranscodeJob {
                token,
                output: TranscodeJobOutput {
                    recipe_hash: recipe_hash.into(),
                    recipe_version,
                    relative_dir: relative_dir.into(),
                    bytes,
                    expected_previous_bytes,
                    manifest_digest: manifest_digest.into(),
                },
                now_ms,
            })
            .await?;
        Ok(matches!(outcome, JobPublishOutcome::Published { .. }))
    }
    async fn fixture_claim_cluster_fragment_index(
        &self,
        node: &str,
        excluded: &[String],
        now_ms: i64,
        _old_lease_expiry: i64,
    ) -> Result<Option<plurx_core::store::ClusterFragmentIndexJob>, StoreError> {
        for delivery in self.delivery_intents(now_ms).await? {
            self.enqueue_delivery(delivery, now_ms).await?;
        }
        let mut after = None;
        loop {
            let page = self
                .job_candidates(CandidateQuery {
                    node_id: node.into(),
                    kinds: vec![JobKind::FragmentIndexBuild, JobKind::ArtifactHydrate],
                    after,
                    now_ms,
                    limit: 128,
                })
                .await?;
            for candidate in page.jobs {
                let payload = candidate.supported_payload()?;
                let (key, kind) = match &payload {
                    JobPayload::FragmentIndexBuild { cache_key, .. } => {
                        (cache_key.clone(), JobKind::FragmentIndexBuild)
                    }
                    JobPayload::ArtifactHydrate { artifact_key, .. } => (
                        artifact_key
                            .strip_prefix("fragment:")
                            .unwrap_or(artifact_key)
                            .to_owned(),
                        JobKind::ArtifactHydrate,
                    ),
                    _ => continue,
                };
                if excluded.contains(&key) {
                    continue;
                }
                let result = self
                    .claim_job(ClaimJob {
                        job_id: candidate.id,
                        expected_revision: candidate.revision,
                        node_id: node.into(),
                        boot_id: uuid::Uuid::new_v4().to_string(),
                        claim_id: uuid::Uuid::new_v4().to_string(),
                        kind,
                        payload_version: 1,
                        now_ms,
                        dispatched_at_ms: now_ms,
                    })
                    .await?;
                if let ClaimOutcome::Claimed { job } = result {
                    let artifact = self.cluster_fragment_index_artifact(&key).await?;
                    let mut projection =
                        plurx_core::store::background_jobs_fragment_admission::projection(
                            &job,
                            artifact.as_ref(),
                        )?;
                    let waiters = self
                        .job_waiters(WaiterQuery {
                            job_id: job.id.clone(),
                            after: None,
                            limit: 128,
                        })
                        .await?;
                    if projection.target_node_id.is_empty() {
                        projection.target_node_id = waiters
                            .waiters
                            .iter()
                            .find(|w| w.target_node_id.as_deref() == Some(node))
                            .or_else(|| waiters.waiters.first())
                            .and_then(|w| w.target_node_id.clone())
                            .unwrap_or_default();
                    }
                    return Ok(Some(projection));
                }
            }
            after = page.next;
            if after.is_none() {
                return Ok(None);
            }
        }
    }
    async fn fixture_fragment_token(
        &self,
        key: &str,
        node: &str,
        fence: i64,
    ) -> Result<Option<JobToken>, StoreError> {
        let mut after_id = None;
        loop {
            let page = self
                .list_jobs(JobQuery {
                    node_id: None,
                    state: Some(JobState::Running),
                    kind: None,
                    after_id,
                    limit: 128,
                })
                .await?;
            for job in page.jobs {
                let matches_key = match job.supported_payload()? {
                    JobPayload::FragmentIndexBuild { cache_key, .. } => cache_key == key,
                    JobPayload::ArtifactHydrate { artifact_key, .. } => {
                        artifact_key == format!("fragment:{key}")
                    }
                    _ => false,
                };
                if matches_key {
                    if let Some(token) = job.token.filter(|t| t.node_id == node && t.fence == fence)
                    {
                        return Ok(Some(token));
                    }
                }
            }
            after_id = page.next_after_id;
            if after_id.is_none() {
                return Ok(None);
            }
        }
    }
    async fn fixture_renew_cluster_fragment_index(
        &self,
        key: &str,
        _target: &str,
        node: &str,
        fence: i64,
        now_ms: i64,
        _old_lease_expiry: i64,
    ) -> Result<bool, StoreError> {
        let Some(token) = self.fixture_fragment_token(key, node, fence).await? else {
            return Ok(false);
        };
        Ok(matches!(
            self.renew_jobs(RenewJobs {
                tokens: vec![token],
                now_ms
            })
            .await?
            .first(),
            Some(RenewOutcome::Renewed { .. })
        ))
    }
    async fn fixture_yield_cluster_fragment_index(
        &self,
        key: &str,
        _target: &str,
        node: &str,
        fence: i64,
        now_ms: i64,
        retry_at_ms: i64,
    ) -> Result<bool, StoreError> {
        let Some(token) = self.fixture_fragment_token(key, node, fence).await? else {
            return Ok(false);
        };
        self.settle_job(SettleJob {
            token,
            settlement: JobSettlement::Yield {
                error_code: None,
                checkpoint: None,
                not_before_ms: retry_at_ms,
            },
            now_ms,
        })
        .await
    }
    async fn fixture_fail_cluster_fragment_index(
        &self,
        key: &str,
        _target: &str,
        node: &str,
        fence: i64,
        error_code: &str,
        retryable: bool,
        now_ms: i64,
        retry_at_ms: i64,
    ) -> Result<bool, StoreError> {
        let Some(token) = self.fixture_fragment_token(key, node, fence).await? else {
            return Ok(false);
        };
        let settlement = if retryable {
            JobSettlement::Retry {
                error_code: error_code.into(),
                not_before_ms: retry_at_ms,
            }
        } else {
            JobSettlement::Fail {
                error_code: error_code.into(),
            }
        };
        self.settle_job(SettleJob {
            token,
            settlement,
            now_ms,
        })
        .await
    }
    async fn fixture_fail_cluster_fragment_index_typed(
        &self,
        failure: &FragmentFailureFixture,
    ) -> Result<bool, StoreError> {
        let Some(token) = self
            .fixture_fragment_token(&failure.cache_key, &failure.node_id, failure.fence)
            .await?
        else {
            return Ok(false);
        };
        self.fail_fragment_job(FragmentJobFailure {
            token,
            code: failure.code,
            transient_allowlisted: failure.transient_allowlisted,
            diagnostic: failure.diagnostic.clone(),
            now_ms: failure.now_ms,
        })
        .await
    }
    async fn fixture_complete_cluster_fragment_index(
        &self,
        job: &plurx_core::store::ClusterFragmentIndexJob,
        artifact: &plurx_core::store::ClusterFragmentIndexArtifact,
        location: &plurx_core::store::ClusterFragmentIndexLocation,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let Some(token) = self
            .fixture_fragment_token(&job.cache_key, &job.owner_node_id, job.fence)
            .await?
        else {
            return Ok(false);
        };
        if location.node_id != token.node_id || location.cache_key != artifact.cache_key {
            return Ok(false);
        }
        let outcome = self
            .publish_fragment_job(PublishFragmentJob {
                token,
                artifact: artifact.clone(),
                now_ms,
            })
            .await?;
        Ok(matches!(
            outcome,
            JobPublishOutcome::Published { .. } | JobPublishOutcome::AlreadyPublished { .. }
        ))
    }
    async fn fixture_complete_cluster_fragment_index_by_hydration(
        &self,
        job: &plurx_core::store::ClusterFragmentIndexJob,
        artifact: &plurx_core::store::ClusterFragmentIndexArtifact,
        location: &plurx_core::store::ClusterFragmentIndexLocation,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        self.fixture_complete_cluster_fragment_index(job, artifact, location, now_ms)
            .await
    }
}
impl<T: Store + ?Sized> QueueFixture for T {}

#[allow(dead_code)]
/// Remove what v98–v101 (Jellyfin compatibility) created, so a fixture that
/// winds a current database back by hand replays those migrations against a
/// database that does not already have them. Triggers go first: the watch
/// triggers name the revision columns the last two statements drop.
pub(crate) fn remove_jellyfin_compatibility_schema(connection: &rusqlite::Connection) {
    // Every caller rewinds below v98, so remove the later v104–v105 sharing
    // tables too. Leaving them behind impersonates an ambiguous private lineage.
    remove_cinema_schema(connection);
    remove_sharing_schema(connection);
    connection
        .execute_batch(
            "DROP TRIGGER IF EXISTS jellyfin_watch_own_insert;
             DROP TRIGGER IF EXISTS jellyfin_watch_own_update;
             DROP TRIGGER IF EXISTS jellyfin_watch_final_insert;
             DROP TRIGGER IF EXISTS jellyfin_watch_final_update;
             DROP TABLE IF EXISTS jellyfin_plays;
             DROP TABLE IF EXISTS jellyfin_login_tokens;
             DROP TRIGGER IF EXISTS jellyfin_retire_file;
             DROP TRIGGER IF EXISTS jellyfin_retire_item;
             DROP TRIGGER IF EXISTS jellyfin_retire_library;
             DROP TRIGGER IF EXISTS jellyfin_retire_user;
             DROP TABLE IF EXISTS jellyfin_entity_ids;
             ALTER TABLE watch_state DROP COLUMN manual_origin;
             ALTER TABLE watch_state DROP COLUMN manual_revision;",
        )
        .expect("remove v98-v101 Jellyfin compatibility schema");
}

/// The published v112 object witness shared by SQLite and replicated fixtures.
#[allow(dead_code)]
pub(crate) const TRANSLATION_FIXTURE_SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE (name GLOB 'subtitle_translation_*' OR tbl_name GLOB 'subtitle_translation_*') AND sql IS NOT NULL ORDER BY name";

#[allow(dead_code)]
pub(crate) fn translation_fixture_shape() -> Vec<(String, String)> {
    translation_fixture_shape_at(0)
}

fn translation_fixture_shape_at(version: u8) -> Vec<(String, String)> {
    assert!(
        version <= 2,
        "only published translation shapes are recognized"
    );
    let expected = rusqlite::Connection::open_in_memory().expect("translation shape witness");
    expected.execute_batch("CREATE TABLE files(id INTEGER,size INTEGER,mtime INTEGER); CREATE TABLE background_jobs(kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT);")
        .expect("translation metadata dependencies");
    expected
        .execute_batch(include_str!("../../src/store/subtitle_translation.sql"))
        .expect("published legacy translation objects");
    if version >= 1 {
        expected
            .execute_batch(include_str!(
                "../../src/store/subtitle_translation_retarget.sql"
            ))
            .expect("published retarget translation objects");
    }
    if version == 2 {
        expected
            .execute_batch(include_str!(
                "../../src/store/subtitle_translation_continuity.sql"
            ))
            .expect("published continuity translation objects");
    }
    let shape = expected
        .prepare(TRANSLATION_FIXTURE_SHAPE_SQL)
        .expect("translation shape query")
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("translation shape rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("complete translation shape");
    shape
}

fn translation_fixture_version(actual: &[(String, String)]) -> u8 {
    (0..=2)
        .find(|version| actual == translation_fixture_shape_at(*version))
        .expect("only an exact published translation shape may be rewound")
}

/// A read-only, closed drop plan. Every table in the selected published layout
/// is inventoried: its exact marker remains and every other table is empty.
#[allow(dead_code)]
pub(crate) fn validated_empty_translation_rewind(
    actual: &[(String, String)],
    counts: &[(String, i64)],
    accepted_jobs: i64,
) -> Vec<String> {
    assert_eq!(
        accepted_jobs, 0,
        "historical fixture must not discard accepted translation work"
    );
    let version = translation_fixture_version(actual);
    let tables: std::collections::BTreeSet<_> = actual
        .iter()
        .filter(|(_, sql)| sql.starts_with("CREATE TABLE"))
        .map(|(name, _)| name.as_str())
        .collect();
    let counted: std::collections::BTreeMap<_, _> = counts
        .iter()
        .map(|(name, count)| (name.as_str(), *count))
        .collect();
    assert_eq!(
        counts.len(),
        counted.len(),
        "each translation table is inventoried once"
    );
    assert_eq!(
        tables,
        counted.keys().copied().collect(),
        "every translation table must be inventoried"
    );
    for (name, count) in counted {
        assert_eq!(count, if name == "subtitle_translation_schema" {1} else {0}, "historical fixture must retain its exact marker and must not discard translation data in {name}");
    }
    let mut drops: Vec<_> = actual
        .iter()
        .filter(|(_, sql)| sql.starts_with("CREATE TRIGGER"))
        .map(|(name, _)| format!("DROP TRIGGER {name}"))
        .collect();
    // Triggers first, then explicit FK children before assets/operations. This
    // is not a generic down-migration or a production migration tolerance.
    let mut ordered = vec![
        "subtitle_translation_read_leases",
        "subtitle_translation_snapshots",
        "subtitle_translation_segments",
        "subtitle_translation_ranges",
        "subtitle_translation_source_bindings",
        "subtitle_translation_demands",
        "subtitle_translation_assets",
        "subtitle_translation_requests",
        "subtitle_translation_capabilities",
        "subtitle_translation_commands",
        "subtitle_translation_operations",
        "subtitle_translation_schema",
    ];
    if version == 2 {
        ordered.insert(0, "subtitle_translation_range_continuities");
    }
    assert_eq!(
        tables,
        ordered.iter().copied().collect(),
        "every translation table is explicitly removed"
    );
    drops.extend(ordered.into_iter().map(|name| format!("DROP TABLE {name}")));
    drops
}

fn require_fixture_marker(connection: &rusqlite::Connection, table: &str, version: i64) {
    let counts: (i64, i64) = connection
        .query_row(
            &format!("SELECT count(*),COALESCE(sum(singleton=1 AND version=?1),0) FROM {table}"),
            [version],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("fixture marker inventory");
    assert_eq!(
        counts,
        (1, 1),
        "one exact published marker required for {table}"
    );
}

/// Closed, read-only removal plan for the empty v115 playback handoff
/// extension: the SQL that drops it and the version the next rewind sees.
#[allow(dead_code)]
fn validated_handoff_rewind(connection: &rusqlite::Connection, version: i64) -> (String, i64) {
    const SHAPE: &str = "SELECT name,sql FROM sqlite_master WHERE (name GLOB 'playback_handoffs*' OR tbl_name GLOB 'playback_handoffs*') AND sql IS NOT NULL ORDER BY name";
    let shape = |conn: &rusqlite::Connection| {
        conn.prepare(SHAPE)
            .expect("handoff shape query")
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("handoff shape rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("complete handoff shape")
    };
    let actual = shape(connection);
    if actual.is_empty() {
        assert!(
            version < 115,
            "current handoff fixture cannot be missing its extension"
        );
        return (String::new(), version);
    }
    assert_eq!(
        version, 115,
        "only the current handoff fixture may be rewound"
    );
    let expected = rusqlite::Connection::open_in_memory().expect("handoff shape witness");
    expected
        .execute_batch("CREATE TABLE users(id INTEGER PRIMARY KEY);")
        .expect("handoff witness dependencies");
    expected
        .execute_batch(include_str!("../../src/store/playback_handoffs.sql"))
        .expect("published handoff objects");
    assert_eq!(
        actual,
        shape(&expected),
        "exact current handoff shape required"
    );
    require_fixture_marker(connection, "playback_handoffs_schema", 1);
    let rows: i64 = connection
        .query_row(
            "SELECT (SELECT count(*) FROM playback_handoffs) + (SELECT count(*) FROM playback_handoffs_tokens)",
            [],
            |row| row.get(0),
        )
        .expect("handoff fixture data");
    assert_eq!(rows, 0, "handoff codes and tokens cannot be rewound");
    (
        "DROP TABLE playback_handoffs_tokens; DROP TABLE playback_handoffs; DROP TABLE playback_handoffs_schema;"
            .into(),
        114,
    )
}

/// Closed, read-only removal plan for the empty v114 title extension.
#[allow(dead_code)]
pub(crate) const TITLE_FIXTURE_SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'dv_title_jobs_*' AND sql IS NOT NULL ORDER BY name";
pub(crate) const ANNOTATIONS_FIXTURE_SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'library_annotations_*' AND sql IS NOT NULL ORDER BY name";

/// Canonical read-only removal plan shared by SQLite and replicated fixtures.
#[allow(dead_code)]
pub(crate) fn validated_empty_title_rewind(
    actual: &[(String, String)],
    marker: i64,
    retained: i64,
) -> Vec<String> {
    const SHAPE: &str = TITLE_FIXTURE_SHAPE_SQL;
    let shape = |conn: &rusqlite::Connection| {
        conn.prepare(SHAPE)
            .expect("title shape query")
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("title shape rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("complete title shape")
    };
    let expected = rusqlite::Connection::open_in_memory().expect("title shape witness");
    expected.execute_batch("CREATE TABLE files(id INTEGER PRIMARY KEY,path TEXT,size INTEGER,mtime INTEGER,video_codec TEXT,dv_profile INTEGER,dv_rpu_present INTEGER,probe_json TEXT,scanned_at INTEGER); CREATE TABLE background_jobs(id TEXT,kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT,owner_node_id TEXT,owner_boot_id TEXT,claim_id TEXT,lease_expires_ms INTEGER,result_ref TEXT,updated_at_ms INTEGER); CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT); CREATE TABLE background_job_waiters(job_id TEXT,state TEXT,result_ref TEXT,updated_at_ms INTEGER);").expect("title witness dependencies");
    expected
        .execute_batch(include_str!("../../src/store/dv_title_jobs.sql"))
        .expect("published title objects");
    assert_eq!(
        actual,
        shape(&expected),
        "exact current title shape required"
    );
    assert_eq!(marker, 1, "current title marker required");
    assert_eq!(
        retained, 0,
        "title demand, accepted work and aggregates cannot be rewound"
    );
    let mut drops = actual
        .iter()
        .filter(|(_, sql)| sql.starts_with("CREATE TRIGGER"))
        .map(|(name, _)| format!("DROP TRIGGER {name}"))
        .collect::<Vec<_>>();
    drops.push("DROP TABLE dv_title_jobs_demand".into());
    drops.push("DROP TABLE dv_title_jobs_schema".into());
    drops
}

#[allow(dead_code)]
fn validated_title_rewind(connection: &rusqlite::Connection, version: i64) -> (String, i64) {
    const SHAPE: &str = TITLE_FIXTURE_SHAPE_SQL;
    let shape = |conn: &rusqlite::Connection| {
        conn.prepare(SHAPE)
            .expect("title shape query")
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("title shape rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("complete title shape")
    };
    let actual = shape(connection);
    if actual.is_empty() {
        assert!(
            version < 114,
            "current title fixture cannot be missing its extension"
        );
        return (String::new(), version);
    }
    assert_eq!(
        version, 114,
        "only the current title fixture may be rewound"
    );
    let marker: i64 = connection
        .query_row(
            "SELECT count(*) FROM dv_title_jobs_schema WHERE singleton=1 AND version=1",
            [],
            |row| row.get(0),
        )
        .expect("title marker");
    let retained: i64 = connection.query_row("SELECT (SELECT count(*) FROM dv_title_jobs_demand) + (SELECT count(*) FROM background_jobs WHERE kind='dv_title_scan') + (SELECT count(*) FROM background_job_commands WHERE operation IN ('bind_dv_title','publish_dv_title')) + (SELECT count(*) FROM files WHERE json_valid(probe_json) AND json_type(probe_json,'$.plurx_dv_title_v1') IS NOT NULL)", [], |row| row.get(0)).expect("title fixture data");
    let drops = validated_empty_title_rewind(&actual, marker, retained);
    (format!("{};", drops.join(";")), 113)
}

#[test]
fn historical_title_rewind_refuses_partial_schema_without_mutation() {
    let dir = tempfile::tempdir().expect("historical fixture");
    let path = dir.path().join("partial-title.db");
    drop(plurx_core::store::SqliteStore::open(&path).expect("current fixture"));
    let connection = rusqlite::Connection::open(path).expect("raw fixture");
    connection
        .execute_batch("DROP TRIGGER dv_title_jobs_bind")
        .expect("partial shape");
    let objects = || {
        connection
            .query_row(
                "SELECT group_concat(name,'|') FROM (SELECT name FROM sqlite_master ORDER BY name)",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("schema snapshot")
    };
    let before = objects();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rewind_empty_managed_subtitle_schema(&connection)
    }))
    .is_err());
    assert_eq!(objects(), before, "refusal precedes every fixture mutation");
}

/// Rewind only an exact, empty v113 ratings/lists extension so the older
/// translation and managed rewinds below see the schema they were written for.
#[allow(dead_code)]
pub(crate) const ANNOTATIONS_FIXTURE_DATA_TABLES: [&str; 3] = [
    "library_annotations_lists",
    "library_annotations_ratings",
    "library_annotations_rating_fetches",
];

/// Canonical exact-shape/empty-data annotation plan; performs no mutation.
#[allow(dead_code)]
pub(crate) fn validated_empty_annotations_rewind(
    actual: &[(String, String)],
    counts: &[(String, i64)],
) -> Vec<String> {
    const SHAPE: &str = ANNOTATIONS_FIXTURE_SHAPE_SQL;
    let shape = |conn: &rusqlite::Connection| {
        conn.prepare(SHAPE)
            .expect("annotations shape query")
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("annotations shape rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("complete annotations shape")
    };
    let expected = rusqlite::Connection::open_in_memory().expect("annotations shape witness");
    expected
        .execute_batch(include_str!("../../src/store/library_annotations.sql"))
        .expect("published current annotations objects");
    assert_eq!(
        actual,
        shape(&expected),
        "exact current annotations shape required"
    );
    let tables = [
        "library_annotations_lists",
        "library_annotations_ratings",
        "library_annotations_rating_fetches",
        "library_annotations_schema",
    ];
    let declared = actual
        .iter()
        .filter(|(_, sql)| sql.starts_with("CREATE TABLE"))
        .map(|(name, _)| name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        declared,
        tables.into_iter().collect(),
        "every annotations table is explicitly removed"
    );
    let counted = counts
        .iter()
        .cloned()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        counts.len(),
        ANNOTATIONS_FIXTURE_DATA_TABLES.len(),
        "complete annotation table inventory required"
    );
    assert_eq!(
        counted.len(),
        ANNOTATIONS_FIXTURE_DATA_TABLES.len(),
        "distinct annotation table inventory required"
    );
    for table in ANNOTATIONS_FIXTURE_DATA_TABLES {
        assert_eq!(
            *counted.get(table).expect("every annotation table counted"),
            0,
            "populated annotations table {table} cannot be rewound"
        );
    }
    tables.map(|name| format!("DROP TABLE {name}")).to_vec()
}

#[allow(dead_code)]
fn validated_annotations_rewind(connection: &rusqlite::Connection, version: i64) -> String {
    const SHAPE: &str = ANNOTATIONS_FIXTURE_SHAPE_SQL;
    let shape = |conn: &rusqlite::Connection| {
        conn.prepare(SHAPE)
            .expect("annotations shape query")
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("annotations shape rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("complete annotations shape")
    };
    let actual = shape(connection);
    if actual.is_empty() {
        assert!(
            version < 113,
            "current annotations fixture cannot be missing its extension"
        );
        return String::new();
    }
    assert_eq!(
        version, 113,
        "only the current annotations fixture may be rewound"
    );
    let mut counts = Vec::new();
    for table in ANNOTATIONS_FIXTURE_DATA_TABLES {
        let rows: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("annotations fixture data");
        counts.push((table.to_owned(), rows));
    }
    format!(
        "{};",
        validated_empty_annotations_rewind(&actual, &counts).join(";")
    )
}

#[allow(dead_code)]
fn validated_translation_rewind(connection: &rusqlite::Connection, version: i64) -> String {
    let actual = connection
        .prepare(TRANSLATION_FIXTURE_SHAPE_SQL)
        .expect("translation shape query")
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("translation shape rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("complete translation shape");
    if actual.is_empty() {
        assert!(
            version < 112,
            "current translation fixture cannot be missing its extension"
        );
        return String::new();
    }
    // 113 only with the exact empty annotations extension, which the caller
    // has already validated and rewinds first.
    assert!(
        matches!(version, 112 | 113),
        "only the current translation fixture may be rewound"
    );
    // Validate identifiers/DDL before using any names in data inventory SQL.
    translation_fixture_version(&actual);
    require_fixture_marker(connection, "subtitle_translation_schema", 1);
    let counts = actual
        .iter()
        .filter(|(_, sql)| sql.starts_with("CREATE TABLE"))
        .map(|(name, _)| {
            let count: i64 = connection
                .query_row(&format!("SELECT count(*) FROM {name}"), [], |row| {
                    row.get(0)
                })
                .expect("translation fixture data");
            (name.clone(), count)
        })
        .collect::<Vec<_>>();
    let accepted_jobs: i64 = connection.query_row("SELECT count(*) FROM background_jobs WHERE kind IN ('subtitle_translation_prepare','subtitle_translation_range')", [], |row| row.get(0)).expect("accepted translation job inventory");
    let drops = validated_empty_translation_rewind(&actual, &counts, accepted_jobs);
    // No mutation: the caller must validate the managed extension as well,
    // then execute both plans in one savepoint.
    format!("{};", drops.join(";"))
}

/// Rewind only an exact empty managed extension in an owned historical fixture.
#[allow(dead_code)]
pub(crate) const MANAGED_FIXTURE_SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE (name GLOB 'subtitle_managed_*' OR name GLOB 'subtitle_model_receipts*' OR tbl_name GLOB 'subtitle_managed_*' OR tbl_name GLOB 'subtitle_model_receipts*' OR name='background_job_required_resources') AND sql IS NOT NULL ORDER BY name";
pub(crate) const MANAGED_FIXTURE_DATA_SQL: &str = "SELECT (SELECT count(*) FROM subtitle_model_receipts) + (SELECT count(*) FROM background_jobs WHERE kind IN ('subtitle_model_operation','subtitle_transcribe_managed','subtitle_text_model_operation','subtitle_text_translate'))";

fn managed_fixture_connection(version: i64) -> rusqlite::Connection {
    assert!(
        matches!(version, 1 | 2),
        "only published managed fixture versions are recognized"
    );
    let expected = rusqlite::Connection::open_in_memory().expect("closed managed metadata witness");
    expected.execute_batch("CREATE TABLE files(id INTEGER,size INTEGER,mtime INTEGER); CREATE TABLE background_jobs(id TEXT,kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT,target_node_id TEXT); CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT); CREATE VIEW background_job_required_resources AS SELECT 1;").expect("closed metadata dependencies");
    expected
        .execute_batch(include_str!("../../src/store/subtitle_managed_schema.sql"))
        .expect("published managed v1 objects");
    if version == 2 {
        expected
            .execute_batch(include_str!(
                "../../src/store/subtitle_managed_resources_v2.sql"
            ))
            .expect("published managed v2 objects");
    }
    expected
}
fn managed_fixture_rows(connection: &rusqlite::Connection) -> Vec<(String, String)> {
    connection
        .prepare(MANAGED_FIXTURE_SHAPE_SQL)
        .expect("managed shape query")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("managed shape rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("complete managed shape")
}
/// Historical v1 and current v2 each require their own exact DDL and marker.
/// This pure plan cannot discard accepted work or silently repair schema drift.
pub(crate) fn validated_empty_managed_rewind(
    shape: &[(String, String)],
    marker_version: i64,
    data_count: i64,
) -> Vec<String> {
    assert_eq!(
        shape,
        managed_fixture_rows(&managed_fixture_connection(marker_version)),
        "only the exact published managed shape may be rewound"
    );
    assert_eq!(
        data_count, 0,
        "accepted managed work or receipts cannot be rewound"
    );
    let old_view = include_str!("../../src/store/background_jobs_probe.sql")
        .split("-- next statement")
        .find(|sql| {
            sql.trim_start()
                .starts_with("CREATE VIEW IF NOT EXISTS background_job_required_resources")
        })
        .expect("published predecessor resource view");
    let mut statements = [
        "DROP TRIGGER subtitle_managed_config_write",
        "DROP TRIGGER subtitle_managed_source_deleted",
        "DROP TRIGGER subtitle_managed_source_changed",
        "DROP VIEW background_job_required_resources",
        "DROP TABLE subtitle_model_receipts",
        "DROP TABLE subtitle_managed_schema",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    statements.push(old_view.to_owned());
    statements
}

#[test]
fn managed_fixture_rewind_accepts_exact_empty_v1_and_v2() {
    for version in [1, 2] {
        let connection = managed_fixture_connection(version);
        let statements =
            validated_empty_managed_rewind(&managed_fixture_rows(&connection), version, 0);
        connection
            .execute_batch(&format!(
                "SAVEPOINT managed_test; {}; RELEASE managed_test;",
                statements.join(";")
            ))
            .expect("apply complete validated rewind");
        let remaining:i64=connection.query_row("SELECT count(*) FROM sqlite_master WHERE name LIKE 'subtitle_managed_%' OR name='subtitle_model_receipts'",[],|row|row.get(0)).expect("removed managed objects");
        assert_eq!(remaining, 0);
        let view: String = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name='background_job_required_resources'",
                [],
                |row| row.get(0),
            )
            .expect("complete historical resource view");
        assert!(!view.contains("subtitle_model_operation"));
    }
}

#[test]
fn managed_fixture_rewind_refuses_v2_drift_marker_and_populated_work_without_mutation() {
    for damage in ["drift", "receipt", "text-job", "marker", "wrong-marker"] {
        let connection = managed_fixture_connection(2);
        match damage {
            "drift"=>connection.execute_batch("DROP VIEW background_job_required_resources; CREATE VIEW background_job_required_resources AS SELECT 1 AS job_id,'source_io' AS resource_key").expect("drift"),
            "receipt"=>{connection.execute("INSERT INTO subtitle_model_receipts VALUES('node','model','{}')",[]).expect("receipt");},
            "text-job"=>{connection.execute("INSERT INTO background_jobs(id,kind) VALUES('accepted','subtitle_text_model_operation')",[]).expect("accepted model work");},
            "marker"=>{connection.execute("DELETE FROM subtitle_managed_schema",[]).expect("missing marker");},
            "wrong-marker"=>{},
            _=>unreachable!(),
        }
        let snapshot = || {
            let shape = managed_fixture_rows(&connection);
            let data: i64 = connection
                .query_row(MANAGED_FIXTURE_DATA_SQL, [], |row| row.get(0))
                .expect("data inventory");
            let marker:i64=connection.query_row("SELECT COALESCE(max(version),0) FROM subtitle_managed_schema WHERE singleton=1",[],|row|row.get(0)).expect("marker inventory");
            (shape, data, marker)
        };
        let before = snapshot();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                validated_empty_managed_rewind(
                    &before.0,
                    if damage == "wrong-marker" {
                        1
                    } else {
                        before.2
                    },
                    before.1,
                )
            }))
            .is_err(),
            "{damage} must refuse"
        );
        assert_eq!(
            snapshot(),
            before,
            "refusal preserves DDL, marker and accepted work"
        );
    }
}

/// Read-only plan for the exact empty v116-v120 tail of a current fixture.
/// This recognizes published layouts, not arbitrary later schema versions.
fn validated_current_tail_rewind(connection: &rusqlite::Connection) -> (String, i64) {
    const SHAPE: &str = "SELECT name,sql FROM sqlite_master WHERE (name GLOB 'subtitle_text_translation_*' OR name GLOB 'shared_caption_*' OR tbl_name GLOB 'subtitle_text_translation_*' OR tbl_name GLOB 'shared_caption_*') AND sql IS NOT NULL ORDER BY name";
    let shape = |conn: &rusqlite::Connection| {
        conn.prepare(SHAPE)
            .expect("tail shape query")
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("tail shape rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("complete tail shape")
    };
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("fixture version");
    let actual = shape(connection);
    if version <= 115 {
        assert!(
            actual.is_empty(),
            "historical fixture cannot carry later translation or caption objects"
        );
        let translation = connection
            .prepare(TRANSLATION_FIXTURE_SHAPE_SQL)
            .expect("historical translation query")
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("historical translation rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("complete historical translation shape");
        if !translation.is_empty() {
            assert_eq!(
                translation_fixture_version(&translation),
                0,
                "historical SQLite fixture requires legacy translation"
            );
        }
        return (String::new(), version);
    }
    assert_eq!(
        version, 120,
        "only the exact current fixture tail may be rewound"
    );
    let expected = rusqlite::Connection::open_in_memory().expect("current tail witness");
    expected.execute_batch("CREATE TABLE background_jobs(id TEXT PRIMARY KEY); CREATE TABLE subtitle_translation_assets(id TEXT PRIMARY KEY);")
        .expect("current tail dependencies");
    expected
        .execute_batch(include_str!(
            "../../src/store/subtitle_text_translation.sql"
        ))
        .expect("published text translation objects");
    expected
        .execute_batch(include_str!("../../src/store/shared_caption_read.sql"))
        .expect("published caption objects");
    assert_eq!(
        actual,
        shape(&expected),
        "exact current tail shape required"
    );
    for table in ["subtitle_text_translation_schema", "shared_caption_schema"] {
        require_fixture_marker(connection, table, 1);
    }
    let retained: i64 = connection.query_row(
        "SELECT (SELECT count(*) FROM subtitle_text_translation_cues) + (SELECT count(*) FROM shared_caption_read_leases)",
        [], |row| row.get(0)).expect("tail retained data");
    assert_eq!(
        retained, 0,
        "text output and caption leases cannot be rewound"
    );
    let translation = connection
        .prepare(TRANSLATION_FIXTURE_SHAPE_SQL)
        .expect("current translation query")
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("current translation rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("complete current translation shape");
    assert_eq!(
        translation_fixture_version(&translation),
        2,
        "current tail requires continuity translation"
    );
    let marker: i64 = connection
        .query_row(
            "SELECT COALESCE(max(version),0) FROM subtitle_managed_schema WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .expect("current managed marker");
    assert_eq!(marker, 2, "current tail requires managed v2");
    require_fixture_marker(connection, "subtitle_managed_schema", 2);
    let managed: i64 = connection
        .query_row(MANAGED_FIXTURE_DATA_SQL, [], |row| row.get(0))
        .expect("current managed data");
    validated_empty_managed_rewind(&managed_fixture_rows(connection), marker, managed);
    (
        "DROP TABLE shared_caption_read_leases; DROP TABLE shared_caption_schema; DROP TABLE subtitle_text_translation_cues; DROP TABLE subtitle_text_translation_schema;".into(),
        115,
    )
}

pub(crate) fn rewind_empty_managed_subtitle_schema(connection: &rusqlite::Connection) {
    let (tail_rewind, version) = validated_current_tail_rewind(connection);
    let (handoff_rewind, version) = validated_handoff_rewind(connection, version);
    let (title_rewind, version) = validated_title_rewind(connection, version);
    let annotations_rewind = validated_annotations_rewind(connection, version);
    let translation_rewind = validated_translation_rewind(connection, version);
    let objects: i64 = connection.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('subtitle_managed_schema','subtitle_model_receipts','subtitle_managed_source_changed','subtitle_managed_source_deleted','subtitle_managed_config_write')", [], |row|row.get(0)).expect("managed fixture inventory");
    if objects == 0 {
        assert!(
            version < 111,
            "current managed fixture cannot be missing its extension"
        );
        return;
    }
    assert!(
        matches!(version, 112 | 113),
        "only the current SQLite fixture may be rewound"
    );
    assert_eq!(objects, 5, "partial managed fixture cannot be rewound");
    let source: i64 = connection.query_row("SELECT (SELECT count(*) FROM sqlite_master WHERE name='sharing_source_schema_installation') + (SELECT count(*) FROM pragma_table_info('media_sessions') WHERE name='principal_kind')", [], |row|row.get(0)).expect("unactivated managed fixture");
    assert_eq!(source, 0, "Source installation cannot be rewound");
    let marker: i64 = connection
        .query_row(
            "SELECT version FROM subtitle_managed_schema WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .expect("managed marker");
    let data: i64 = connection
        .query_row(MANAGED_FIXTURE_DATA_SQL, [], |row| row.get(0))
        .expect("managed fixture data");
    let managed_rewind =
        validated_empty_managed_rewind(&managed_fixture_rows(connection), marker, data).join(";");
    // Preserve a complete predecessor while the common-queue removal follows.
    // A savepoint keeps this atomic even inside an outer fixture transaction.
    connection.execute_batch(&format!("SAVEPOINT rewind_managed_fixture; {tail_rewind} {handoff_rewind} {title_rewind} {annotations_rewind} {translation_rewind} {managed_rewind}; RELEASE rewind_managed_fixture;")).expect("atomic empty managed fixture rewind");
}

fn complete_fixture_snapshot(connection: &rusqlite::Connection) -> String {
    let mut snapshot = String::new();
    for name in ["user_version", "schema_version", "foreign_keys"] {
        let value: i64 = connection
            .pragma_query_value(None, name, |row| row.get(0))
            .expect("fixture pragma snapshot");
        snapshot.push_str(&format!("{name}={value};"));
    }
    let objects = connection
        .prepare("SELECT type,name,COALESCE(sql,'') FROM sqlite_master ORDER BY type,name")
        .expect("fixture DDL snapshot")
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .expect("fixture DDL rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("complete fixture DDL");
    for (kind, name, sql) in objects {
        snapshot.push_str(&format!("{kind:?}:{name:?}:{sql:?};"));
        if kind == "table" {
            let escaped = name.replace('"', "\"\"");
            let mut statement = connection
                .prepare(&format!("SELECT * FROM \"{escaped}\""))
                .expect("fixture retained data snapshot");
            let columns = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| row.get::<_, rusqlite::types::Value>(column))
                        .collect::<Result<Vec<_>, _>>()
                })
                .expect("fixture retained rows")
                .collect::<Result<Vec<_>, _>>()
                .expect("complete retained data");
            snapshot.push_str(&format!("{rows:?};"));
        }
    }
    snapshot
}

#[test]
fn current_fixture_rewind_accepts_exact_empty_v120_with_foreign_keys() {
    let dir = tempfile::tempdir().expect("current fixture");
    let path = dir.path().join("store.sqlite");
    drop(plurx_core::store::SqliteStore::open(&path).expect("current fixture schema"));
    let connection = rusqlite::Connection::open(path).expect("fixture connection");
    connection
        .execute_batch("PRAGMA foreign_keys=ON;")
        .expect("enforce fixture foreign keys");
    rewind_empty_managed_subtitle_schema(&connection);
    let remaining: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name GLOB 'subtitle_translation_*' OR name GLOB 'subtitle_text_translation_*' OR name GLOB 'shared_caption_*' OR name GLOB 'subtitle_managed_*' OR name='subtitle_model_receipts'",
        [], |row| row.get(0)).expect("removed fixture inventory");
    assert_eq!(remaining, 0);
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("unchanged checkpoint"),
        120
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("fixture foreign key consistency"),
        0
    );
}

#[test]
fn current_fixture_rewind_refuses_drift_and_retained_tail_without_mutation() {
    for damage in [
        "future-version",
        "unsupported-tail-version",
        "text-marker",
        "caption-marker",
        "wrong-text-marker",
        "wrong-caption-marker",
        "managed-marker",
        "wrong-managed-marker",
        "missing-index",
        "translation-trigger",
        "managed-view",
        "extra-object",
        "text-cue",
        "caption-lease",
        "continuity-capture",
        "translation-command",
        "managed-receipt",
        "text-model-job",
        "text-translation-job",
        "partial-handoff",
        "handoff-marker",
        "wrong-translation-marker",
        "extra-text-marker",
        "extra-managed-object",
        "unowned-text-index",
        "unowned-managed-index",
        "legacy-with-current-translation",
    ] {
        let dir = tempfile::tempdir().expect("refusal fixture");
        let path = dir.path().join("store.sqlite");
        drop(plurx_core::store::SqliteStore::open(&path).expect("current refusal schema"));
        let connection = rusqlite::Connection::open(path).expect("refusal connection");
        connection
            .execute_batch("PRAGMA foreign_keys=OFF;")
            .expect("prepare hostile retained fixture rows");
        let sql = match damage {
            "future-version" => "PRAGMA user_version=121;",
            "unsupported-tail-version" => "PRAGMA user_version=119;",
            "text-marker" => "DELETE FROM subtitle_text_translation_schema;",
            "caption-marker" => "DELETE FROM shared_caption_schema;",
            "managed-marker" => "DELETE FROM subtitle_managed_schema;",
            "wrong-text-marker" => "PRAGMA ignore_check_constraints=ON; UPDATE subtitle_text_translation_schema SET version=2; PRAGMA ignore_check_constraints=OFF;",
            "wrong-caption-marker" => "PRAGMA ignore_check_constraints=ON; UPDATE shared_caption_schema SET version=2; PRAGMA ignore_check_constraints=OFF;",
            "wrong-managed-marker" => "PRAGMA ignore_check_constraints=ON; UPDATE subtitle_managed_schema SET version=1; PRAGMA ignore_check_constraints=OFF;",
            "missing-index" => "DROP INDEX shared_caption_read_expiry;",
            "translation-trigger" => "DROP TRIGGER subtitle_translation_mutate; CREATE TRIGGER subtitle_translation_mutate AFTER INSERT ON subtitle_translation_commands BEGIN SELECT 1; END;",
            "managed-view" => "DROP VIEW background_job_required_resources; CREATE VIEW background_job_required_resources AS SELECT 1;",
            "extra-object" => "CREATE TABLE subtitle_text_translation_unknown(value TEXT);",
            // Hostile retained rows need no live parent to require refusal.
            // Re-enable foreign keys before the planner runs and snapshot them.
            "text-cue" => "INSERT INTO subtitle_text_translation_cues VALUES('retained',printf('%064d',0),0,printf('%064d',0),0,1,'{}',0);",
            "caption-lease" => "INSERT INTO shared_caption_read_leases VALUES('retained','00000000-0000-0000-0000-000000000000',printf('%064d',0),1,'retained',0,1,0);",
            "continuity-capture" => "INSERT INTO subtitle_translation_range_continuities VALUES('retained',1,'retained','00000000-0000-0000-0000-000000000000');",
            "translation-command" => "INSERT INTO subtitle_translation_commands VALUES('retained','unknown','{}','{}');",
            "managed-receipt" => "INSERT INTO subtitle_model_receipts VALUES('node','model','{}');",
            "text-model-job" | "text-translation-job" => {
                let kind = if damage == "text-model-job" { "subtitle_text_model_operation" } else { "subtitle_text_translate" };
                connection.execute("INSERT INTO background_jobs(id,kind,payload_version,payload_json,dedupe_key,priority,state,not_before_ms,created_at_ms,updated_at_ms) VALUES('retained',?1,1,'{}','retained',0,'queued',0,0,0)", [kind]).expect("retained accepted text work");
                ""
            }
            "partial-handoff" => "DROP TABLE playback_handoffs_tokens;",
            "handoff-marker" => "DELETE FROM playback_handoffs_schema;",
            "wrong-translation-marker" => "PRAGMA ignore_check_constraints=ON; UPDATE subtitle_translation_schema SET version=2; PRAGMA ignore_check_constraints=OFF;",
            "extra-text-marker" => "PRAGMA ignore_check_constraints=ON; INSERT INTO subtitle_text_translation_schema VALUES(2,1); PRAGMA ignore_check_constraints=OFF;",
            "extra-managed-object" => "CREATE TABLE subtitle_managed_unknown(value TEXT);",
            "unowned-text-index" => "CREATE INDEX unexpected_text_index ON subtitle_text_translation_cues(cue_index);",
            "unowned-managed-index" => "CREATE INDEX unexpected_receipt_index ON subtitle_model_receipts(model_id);",
            "legacy-with-current-translation" => "DROP TABLE shared_caption_read_leases; DROP TABLE shared_caption_schema; DROP TABLE subtitle_text_translation_cues; DROP TABLE subtitle_text_translation_schema; PRAGMA user_version=115;",
            _ => unreachable!(),
        };
        connection.execute_batch(sql).expect("damage owned fixture");
        connection
            .execute_batch("PRAGMA foreign_keys=ON;")
            .expect("enforce refusal foreign keys");
        let before = complete_fixture_snapshot(&connection);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                rewind_empty_managed_subtitle_schema(&connection);
            }))
            .is_err(),
            "{damage} must refuse"
        );
        assert_eq!(
            complete_fixture_snapshot(&connection),
            before,
            "{damage}: refusal preserves all DDL, versions, markers and retained data"
        );
    }
}

/// Wind a current test database back across the durable-queue introduction.
/// This touches the common queue and its v87 analysis-source reservations,
/// never production migration.
#[allow(dead_code)]
pub(crate) fn remove_common_queue_schema(connection: &rusqlite::Connection) {
    // Refuse to discard managed subtitle model receipts before any rewind,
    // even in a bare fixture that carries only the receipt table.
    let receipt_table: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='subtitle_model_receipts'",
            [],
            |row| row.get(0),
        )
        .expect("managed subtitle receipt table inventory before fixture rewind");
    if receipt_table == 1 {
        let managed_receipts: i64 = connection
            .query_row("SELECT count(*) FROM subtitle_model_receipts", [], |row| {
                row.get(0)
            })
            .expect("managed subtitle receipt count before fixture rewind");
        assert_eq!(
            managed_receipts, 0,
            "historical fixture must not discard managed subtitle model receipts"
        );
    }
    rewind_empty_managed_subtitle_schema(connection);
    connection
        .execute_batch("ALTER TABLE dv_conversions DROP COLUMN requested_manually;")
        .expect("remove later DV request provenance");
    for kind in ["trigger", "view", "table"] {
        let names: Vec<String> = connection
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = ?1
               AND (name GLOB 'background_*' OR name GLOB 'analysis_source_*'
                    OR name = 'analysis_required_resources') ORDER BY name",
            )
            .expect("queue schema objects")
            .query_map([kind], |row| row.get(0))
            .expect("queue schema query")
            .collect::<Result<_, _>>()
            .expect("queue schema names");
        for name in names {
            let quoted = name.replace('"', "\"\"");
            connection
                .execute_batch(&format!(
                    "DROP {} \"{}\"",
                    kind.to_ascii_uppercase(),
                    quoted
                ))
                .expect("drop common queue fixture object");
        }
    }
}

/// Remove v104–v105 before rewinding a current SQLite test fixture.
#[allow(dead_code)]
pub(crate) fn remove_sharing_schema(connection: &rusqlite::Connection) {
    for table in [
        "sharing_ingress_custody",
        "sharing_delivery_grants",
        "sharing_relay_upstream",
        "sharing_endpoint_manifest",
        "sharing_catalogue_revisions",
        "sharing_import_rotations",
        "sharing_rotations",
        "sharing_watch",
        "sharing_assignments",
        "sharing_viewers",
        "sharing_imports",
        "sharing_export_libraries",
        "sharing_exports",
        "sharing_invitations",
        "sharing_identity",
    ] {
        connection
            .execute_batch(&format!("DROP TABLE IF EXISTS {table};"))
            .expect("remove sharing fixture schema");
    }
}

/// Remove additive Cinema v106-v110 before replaying a historical fixture.
/// Both v43 guard and v14 import fixtures reach this through the Jellyfin helper.
#[allow(dead_code)]
pub(crate) fn remove_cinema_schema(connection: &rusqlite::Connection) {
    connection
        .execute_batch(
            "DROP TRIGGER IF EXISTS invitation_cleanup_guard;
         DROP TRIGGER IF EXISTS invitation_consent_cleanup;
         DROP TRIGGER IF EXISTS invitation_phone_rebind;
         DROP TABLE IF EXISTS invitation_events;
         DROP TABLE IF EXISTS invitation_cooldowns;
         DROP TABLE IF EXISTS invitation_consents;
         DROP TABLE IF EXISTS invitation_broker_revocations;
         DROP TABLE IF EXISTS invitation_phones;
         DROP TABLE IF EXISTS invitation_schema;
         DROP TABLE IF EXISTS remote_grants;
         DROP TABLE IF EXISTS remote_receivers;
         DROP TABLE IF EXISTS remote_claim_budget;
         DROP TABLE IF EXISTS remote_schema;",
        )
        .expect("remove v106-v110 Cinema fixture schema");
}
