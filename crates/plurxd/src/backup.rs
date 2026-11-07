//! Backend-neutral portable backup ownership and scheduling.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::Json;
use plurx_core::error::StoreError;
use plurx_core::store::{keys, Store};

use crate::http::error::ApiError;
use crate::http::extract::AdminUser;
use crate::state::AppState;
use crate::state::JobManager;

#[cfg(feature = "cluster")]
pub(crate) mod cluster;
pub(crate) mod native;

#[cfg(all(test, feature = "cluster"))]
use cluster::{backup_phase_before_deadline, wait_for_local_backup_apply, BACKUP_RESOURCE};

pub(crate) struct BackupArtifact {
    pub path: PathBuf,
    pub manifest: serde_json::Value,
    pub image_bytes: u64,
}

#[async_trait::async_trait]
pub(crate) trait BackupProvider: Send + Sync {
    fn lease_resource(&self) -> &'static str;
    fn artifact_prefix(&self) -> &'static str;
    async fn build(&self, destination: &Path) -> Result<BackupArtifact, StoreError>;
}

#[derive(Default)]
pub(crate) struct BackupMetrics {
    last_success_seconds: AtomicI64,
    artifact_bytes: AtomicU64,
    ok: AtomicU64,
    error: AtomicU64,
    skipped: AtomicU64,
}

impl BackupMetrics {
    pub(crate) fn last_success_seconds(&self) -> i64 {
        self.last_success_seconds.load(Ordering::Relaxed)
    }

    pub(crate) fn prometheus(&self) -> String {
        format!(
            "# HELP plurx_backup_last_success_seconds Unix time of the last successful portable backup.\n\
             # TYPE plurx_backup_last_success_seconds gauge\n\
             plurx_backup_last_success_seconds {}\n\
             # HELP plurx_backup_runs_total Portable backup runs by outcome.\n\
             # TYPE plurx_backup_runs_total counter\n\
             plurx_backup_runs_total{{outcome=\"ok\"}} {}\n\
             plurx_backup_runs_total{{outcome=\"error\"}} {}\n\
             plurx_backup_runs_total{{outcome=\"skipped\"}} {}\n\
             # HELP plurx_backup_artifact_bytes Size of the last successful portable backup image.\n\
             # TYPE plurx_backup_artifact_bytes gauge\n\
             plurx_backup_artifact_bytes {}\n",
            self.last_success_seconds.load(Ordering::Relaxed),
            self.ok.load(Ordering::Relaxed),
            self.error.load(Ordering::Relaxed),
            self.skipped.load(Ordering::Relaxed),
            self.artifact_bytes.load(Ordering::Relaxed),
        )
    }
}

pub(crate) struct BackupManager {
    store: Arc<dyn Store>,
    jobs: Arc<JobManager>,
    provider: Arc<dyn BackupProvider>,
    metrics: Arc<BackupMetrics>,
    last_schedule_day: AtomicI64,
}

impl BackupManager {
    pub(crate) fn new_with_provider(
        store: Arc<dyn Store>,
        jobs: Arc<JobManager>,
        provider: Arc<dyn BackupProvider>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            jobs,
            provider,
            metrics: Arc::new(BackupMetrics::default()),
            last_schedule_day: AtomicI64::new(-1),
        })
    }

    pub(crate) fn metrics(&self) -> Arc<BackupMetrics> {
        Arc::clone(&self.metrics)
    }

    pub(crate) async fn build(
        &self,
        destination_override: Option<&Path>,
    ) -> Result<Option<BackupArtifact>, StoreError> {
        let destination = match destination_override {
            Some(path) => path.to_path_buf(),
            None => self
                .store
                .get_setting(keys::BACKUP_DESTINATION)
                .await?
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| {
                    StoreError::Task(
                        "backup.destination is empty; pass --output or configure it first"
                            .to_owned(),
                    )
                })?,
        };
        let Some(lease) = self
            .jobs
            .acquire_job(self.provider.lease_resource().to_owned())
            .await?
        else {
            self.metrics.skipped.fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        };
        let result = self.provider.build(&destination).await;
        if let Err(error) = lease.release().await {
            tracing::warn!(error = %error, "backup lease cleanup was ambiguous");
        }
        match result {
            Ok(artifact) => {
                self.metrics.ok.fetch_add(1, Ordering::Relaxed);
                self.metrics
                    .artifact_bytes
                    .store(artifact.image_bytes, Ordering::Relaxed);
                self.metrics.last_success_seconds.store(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .min(i64::MAX as u64) as i64,
                    Ordering::Relaxed,
                );
                let keep = match self.store.get_setting(keys::BACKUP_KEEP).await {
                    Ok(value) => value
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .filter(|keep| *keep > 0)
                        .unwrap_or(14),
                    Err(error) => {
                        // The archive is already durably published. Retention
                        // lookup cannot turn that success into a retry that
                        // emits a duplicate artefact.
                        tracing::warn!(error = %error, "backup retention setting read failed");
                        14
                    }
                };
                if let Err(error) =
                    prune_artifacts(&destination, keep, self.provider.artifact_prefix())
                {
                    // Same boundary as above: pruning is maintenance after a
                    // successful publish, not part of publication itself.
                    tracing::warn!(error = %error, "backup retention prune failed");
                }
                Ok(Some(artifact))
            }
            Err(error) => {
                self.metrics.error.fetch_add(1, Ordering::Relaxed);
                Err(error)
            }
        }
    }

    pub(crate) async fn schedule_loop(
        self: Arc<Self>,
        shutdown: tokio_util::sync::CancellationToken,
    ) {
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(Duration::from_secs(60)) => {}
            }
            let (schedule, destination) = match self
                .store
                .get_setting_pair(keys::BACKUP_SCHEDULE_UTC, keys::BACKUP_DESTINATION)
                .await
            {
                Ok(pair) => pair,
                Err(error) => {
                    tracing::warn!(error = %error, "scheduled backup settings read failed");
                    continue;
                }
            };
            let schedule = schedule.unwrap_or_else(|| "02:30".to_owned());
            let Some(destination) = destination.filter(|value| !value.trim().is_empty()) else {
                continue;
            };
            let Some(minute) = parse_schedule_minute(&schedule) else {
                tracing::warn!(
                    schedule,
                    "backup.schedule_utc is invalid; expected HH:MM UTC"
                );
                continue;
            };
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if now / 60 % 1_440 != minute {
                continue;
            }
            let day = (now / 86_400).min(i64::MAX as u64) as i64;
            if !schedule_day_is_pending(&self.last_schedule_day, day) {
                continue;
            }
            let scheduled_minute = now / 60;
            loop {
                match self.build(Some(Path::new(destination.trim()))).await {
                    Ok(Some(_)) => {
                        record_schedule_success(&self.last_schedule_day, day);
                        break;
                    }
                    // Another voter owns the lease. It alone decides whether
                    // this tick succeeded; retrying behind a successful owner
                    // would publish a duplicate artefact.
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(error = %error, "scheduled portable backup failed");
                    }
                }

                // A transient winner failure must not spend the UTC day. Give
                // this same owner another chance while the configured minute
                // is still open; the durable backup lease continues to serialize all
                // eligible builders. A crash remains fenced by the 90-second lease.
                let retry_now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if retry_now / 60 != scheduled_minute {
                    break;
                }
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    () = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
                let after_sleep = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if after_sleep / 60 != scheduled_minute {
                    break;
                }
            }
        }
    }
}

fn schedule_day_is_pending(last_schedule_day: &AtomicI64, day: i64) -> bool {
    last_schedule_day.load(Ordering::Acquire) != day
}

fn record_schedule_success(last_schedule_day: &AtomicI64, day: i64) {
    last_schedule_day.store(day, Ordering::Release);
}

pub(crate) fn parse_schedule_minute(value: &str) -> Option<u64> {
    let (hour, minute) = value.trim().split_once(':')?;
    let hour = hour.parse::<u64>().ok()?;
    let minute = minute.parse::<u64>().ok()?;
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

fn prune_artifacts(destination: &Path, keep: usize, prefix: &str) -> Result<(), StoreError> {
    let mut artifacts = std::fs::read_dir(destination)
        .map_err(|error| StoreError::Task(format!("listing backup destination: {error}")))?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
        .collect::<Vec<_>>();
    artifacts.sort_by_key(std::fs::DirEntry::file_name);
    let remove = artifacts.len().saturating_sub(keep);
    for entry in artifacts.into_iter().take(remove) {
        std::fs::remove_dir_all(entry.path()).map_err(|error| {
            StoreError::Task(format!(
                "pruning backup {}: {error}",
                entry.path().display()
            ))
        })?;
    }
    Ok(())
}

#[derive(serde::Deserialize)]
pub(crate) struct BackupRequest {
    #[serde(default)]
    output: Option<PathBuf>,
}

#[derive(serde::Serialize)]
pub(crate) struct BackupResponse {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest: Option<serde_json::Value>,
}

pub(crate) async fn create(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(request): Json<BackupRequest>,
) -> Result<Json<BackupResponse>, ApiError> {
    match state.backup.build(request.output.as_deref()).await? {
        Some(artifact) => Ok(Json(BackupResponse {
            status: "ok",
            path: Some(artifact.path),
            manifest: Some(artifact.manifest),
        })),
        None => Ok(Json(BackupResponse {
            status: "skipped",
            path: None,
            manifest: None,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "cluster")]
    use plurx_core::cluster::coordination::{StoreCoordinator, UnclusteredJobAuthority};
    #[cfg(feature = "cluster")]
    use plurx_core::cluster::membership::ClusterRole;
    #[cfg(feature = "cluster")]
    use plurx_core::store::SqliteStore;

    #[cfg(feature = "cluster")]
    use crate::job_lease::{acquire_cluster_job, AdmittedRoleJobAuthority};

    #[tokio::test(start_paused = true)]
    #[cfg(feature = "cluster")]
    async fn backup_waits_for_the_current_log_apply() {
        let mut samples = [(Some(1), Some(2)), (Some(2), Some(3)), (Some(3), Some(3))].into_iter();
        let start = tokio::time::Instant::now();
        wait_for_local_backup_apply(start + Duration::from_secs(1), 0, || {
            let sample = samples.next().expect("only three passive observations");
            async move { Ok(sample) }
        })
        .await
        .expect("the current log has applied");
        assert_eq!(
            tokio::time::Instant::now() - start,
            Duration::from_millis(100)
        );
        assert!(samples.next().is_none());
    }

    /// CLUSTER-DRILLS-2026-10-11 F1: on a follower the anchor is committed
    /// by the leader before this replica has even received it, so a replica
    /// that has applied everything it holds is not yet at the cut.
    #[tokio::test(start_paused = true)]
    #[cfg(feature = "cluster")]
    async fn backup_waits_for_the_anchor_to_reach_this_replica() {
        let mut samples = [(Some(5), Some(5)), (Some(5), Some(7)), (Some(7), Some(7))].into_iter();
        let start = tokio::time::Instant::now();
        wait_for_local_backup_apply(start + Duration::from_secs(1), 7, || {
            let sample = samples.next().expect("only three passive observations");
            async move { Ok(sample) }
        })
        .await
        .expect("the anchor has applied here");
        assert_eq!(
            tokio::time::Instant::now() - start,
            Duration::from_millis(100)
        );
        assert!(samples.next().is_none());
    }

    #[tokio::test(start_paused = true)]
    #[cfg(feature = "cluster")]
    async fn backup_lag_cannot_refresh_the_original_deadline() {
        let start = tokio::time::Instant::now();
        let mut log = 2;
        let error = wait_for_local_backup_apply(start + Duration::from_millis(120), 0, || {
            log += 1;
            let sample = (Some(log - 1), Some(log));
            async move { Ok(sample) }
        })
        .await
        .expect_err("continuous lag must expire");
        assert!(error.to_string().contains("original snapshot deadline"));
        assert_eq!(
            tokio::time::Instant::now() - start,
            Duration::from_millis(120)
        );
    }

    #[tokio::test(start_paused = true)]
    #[cfg(feature = "cluster")]
    async fn backup_stalled_metrics_read_is_bounded() {
        let start = tokio::time::Instant::now();
        let error = wait_for_local_backup_apply(start + Duration::from_millis(120), 0, || {
            std::future::pending::<Result<(Option<u64>, Option<u64>), StoreError>>()
        })
        .await
        .expect_err("a stalled metrics read must expire");
        assert!(error.to_string().contains("local Raft catch-up"));
        assert_eq!(
            tokio::time::Instant::now() - start,
            Duration::from_millis(120)
        );
    }

    #[tokio::test(start_paused = true)]
    #[cfg(feature = "cluster")]
    async fn backup_metrics_error_is_not_retried() {
        let start = tokio::time::Instant::now();
        let mut reads = 0;
        let error = wait_for_local_backup_apply(start + Duration::from_secs(1), 0, || {
            reads += 1;
            async { Err(StoreError::Database("metrics unavailable".to_owned())) }
        })
        .await
        .expect_err("a database error must remain visible");
        assert_eq!(reads, 1);
        assert!(error.to_string().contains("metrics unavailable"));
        assert_eq!(tokio::time::Instant::now(), start);
    }

    #[tokio::test(start_paused = true)]
    #[cfg(feature = "cluster")]
    async fn backup_phase_rejects_completion_at_the_original_deadline() {
        let start = tokio::time::Instant::now();
        let error = backup_phase_before_deadline(
            start + Duration::from_millis(120),
            "snapshot publication",
            async {
                tokio::time::advance(Duration::from_millis(120)).await;
                Ok(())
            },
        )
        .await
        .expect_err("late ready completion cannot count as timely admission");
        assert!(error.to_string().contains("original snapshot deadline"));
    }

    #[tokio::test(start_paused = true)]
    #[cfg(feature = "cluster")]
    async fn backup_snapshot_trigger_inherits_the_spent_catchup_budget() {
        let start = tokio::time::Instant::now();
        let deadline = start + Duration::from_millis(120);
        wait_for_local_backup_apply(deadline, 0, || async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok((Some(3), Some(3)))
        })
        .await
        .expect("catch-up leaves only twenty milliseconds");
        let error = backup_phase_before_deadline(deadline, "snapshot trigger", async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            Ok(())
        })
        .await
        .expect_err("the trigger cannot acquire a new budget");
        assert!(error.to_string().contains("snapshot trigger"));
        assert_eq!(
            tokio::time::Instant::now() - start,
            Duration::from_millis(120)
        );
    }

    #[test]
    fn schedule_is_one_strict_utc_minute() {
        assert_eq!(parse_schedule_minute("02:30"), Some(150));
        assert_eq!(parse_schedule_minute("23:59"), Some(1_439));
        assert_eq!(parse_schedule_minute("24:00"), None);
        assert_eq!(parse_schedule_minute("2:30:00"), None);
    }

    #[test]
    fn failed_schedule_attempt_keeps_the_day_retriable_until_success() {
        let fence = AtomicI64::new(-1);
        let day = 42;

        assert!(schedule_day_is_pending(&fence, day));
        // Failure and lease-skipped outcomes deliberately do not mutate the
        // fence. Only publication calls record_schedule_success.
        assert!(schedule_day_is_pending(&fence, day));
        record_schedule_success(&fence, day);
        assert!(!schedule_day_is_pending(&fence, day));
        assert!(schedule_day_is_pending(&fence, day + 1));
    }

    #[tokio::test]
    #[cfg(feature = "cluster")]
    async fn backup_ticks_are_one_voter_only_and_a_learner_never_claims() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("lease store"));
        let first = StoreCoordinator::new(Arc::clone(&store), "backup-node-a".to_owned())
            .expect("first coordinator");
        let second = StoreCoordinator::new(Arc::clone(&store), "backup-node-b".to_owned())
            .expect("second coordinator");
        let voter = UnclusteredJobAuthority;

        let (first_claim, second_claim) = tokio::join!(
            acquire_cluster_job(&first, &voter, BACKUP_RESOURCE.to_owned()),
            acquire_cluster_job(&second, &voter, BACKUP_RESOURCE.to_owned())
        );
        let first_claim = first_claim.expect("first tick");
        let second_claim = second_claim.expect("second tick");
        assert_eq!(
            usize::from(first_claim.is_some()) + usize::from(second_claim.is_some()),
            1,
            "simultaneous voter ticks must admit exactly one builder"
        );
        if let Some(lease) = first_claim {
            lease.release().await.expect("release first lease");
        }
        if let Some(lease) = second_claim {
            lease.release().await.expect("release second lease");
        }

        let learner = AdmittedRoleJobAuthority::new(ClusterRole::Learner);
        let learner_claim = acquire_cluster_job(&first, &learner, BACKUP_RESOURCE.to_owned())
            .await
            .expect("learner refusal");
        assert!(
            learner_claim.is_none(),
            "a learner claimed the backup lease"
        );
    }
}
