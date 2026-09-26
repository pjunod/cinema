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
        let request = plurx_core::store::background_jobs_pretranscode::enqueue_request(job)?;
        Ok(matches!(
            self.enqueue_job_fenced(request, lease.clone(), replacement.clone())
                .await?,
            EnqueueOutcome::Accepted { .. }
        ))
    }
    async fn fixture_claim_pretranscode_job(
        &self,
        node: &str,
        caps: &PretranscodeWorkerCapabilities,
        excluded: &[String],
        now_ms: i64,
        _old_lease_expiry: i64,
    ) -> Result<Option<PretranscodeJob>, StoreError> {
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
