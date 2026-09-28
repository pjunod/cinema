//! Shared fixture adapter: exercise common ownership in the older subtitle
//! coverage/repair scenarios without manufacturing an independent lease.
use plurx_core::error::StoreError;
use plurx_core::store::background_jobs::*;
use plurx_core::store::background_jobs_subtitle::{SubtitleJobWrite, WriteSubtitleJob};
use plurx_core::store::{AnalysisRequest, Store};

#[allow(async_fn_in_trait, dead_code)]
pub trait SubtitleFixture: Store {
    async fn claim_subtitle_fixture(
        &self,
        id: &str,
        node: &str,
        now: i64,
        _old_expiry: i64,
    ) -> Result<Option<AnalysisRequest>, StoreError> {
        let Some(request) = self.analysis_request(id).await? else {
            return Ok(None);
        };
        let admitted = self.enqueue_subtitle_job(request, now).await?;
        let id = match admitted {
            EnqueueOutcome::Accepted { job_id, .. } | EnqueueOutcome::Existing { job_id, .. } => {
                job_id
            }
            other => {
                return Err(StoreError::Task(format!(
                    "fixture subtitle admission: {other:?}"
                )))
            }
        };
        let job = self.background_job(&id).await?.expect("admitted job");
        match self
            .claim_artifact_job(ClaimJob {
                job_id: id,
                expected_revision: job.revision,
                node_id: node.into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::SubtitleExtract,
                payload_version: 1,
                now_ms: now,
                dispatched_at_ms: now,
            })
            .await?
        {
            ClaimOutcome::Claimed { job } => {
                let JobPayload::SubtitleExtract {
                    source_generation, ..
                } = job.supported_payload()?
                else {
                    unreachable!()
                };
                self.analysis_request(&source_generation).await
            }
            _ => Ok(None),
        }
    }

    async fn subtitle_fixture_token(
        &self,
        request: &AnalysisRequest,
    ) -> Result<JobToken, StoreError> {
        let page = self
            .list_jobs(JobQuery {
                state: None,
                kind: Some(JobKind::SubtitleExtract),
                after_id: None,
                limit: 128,
            })
            .await?;
        page.jobs.into_iter().find_map(|job| {
            matches!(job.supported_payload(), Ok(JobPayload::SubtitleExtract { source_generation, .. }) if source_generation == request.request_id)
                .then_some(job.token).flatten()
        }).ok_or_else(|| StoreError::Task("fixture subtitle owner missing".into()))
    }

    async fn complete_subtitle_fixture(
        &self,
        request: &AnalysisRequest,
        result_key: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        self.write_subtitle_job(WriteSubtitleJob {
            token: self.subtitle_fixture_token(request).await?,
            request: request.clone(),
            output: SubtitleJobWrite::Complete {
                result_key: result_key.into(),
            },
            now_ms,
        })
        .await
    }

    async fn fail_subtitle_fixture(
        &self,
        request: &AnalysisRequest,
        code: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        self.settle_job(SettleJob {
            token: self.subtitle_fixture_token(request).await?,
            settlement: JobSettlement::Fail {
                error_code: code.into(),
            },
            now_ms,
        })
        .await
    }
}
impl<T: Store + ?Sized> SubtitleFixture for T {}
