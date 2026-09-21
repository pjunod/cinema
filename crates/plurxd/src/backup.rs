//! Portable cluster backup ownership and scheduling.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::Json;
use plurx_core::cluster::migration::ClusterBackupManifest;
use plurx_core::error::StoreError;
use plurx_core::store::{keys, Store};

use crate::http::error::ApiError;
use crate::http::extract::AdminUser;
use crate::state::AppState;
use crate::state::JobManager;

const BACKUP_RESOURCE: &str = "backup:cluster";
const POINTER_MAX_BYTES: u64 = 128;

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
    client: Option<hiqlite::Client>,
    data_dir: PathBuf,
    credential_key_path: PathBuf,
    node_id: String,
    credential_key_id: String,
    snapshot_timeout: Duration,
    metrics: Arc<BackupMetrics>,
    last_schedule_day: AtomicI64,
}

impl BackupManager {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        store: Arc<dyn Store>,
        jobs: Arc<JobManager>,
        client: Option<hiqlite::Client>,
        data_dir: PathBuf,
        credential_key_path: PathBuf,
        node_id: String,
        credential_key_id: String,
        snapshot_timeout: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            jobs,
            client,
            data_dir,
            credential_key_path,
            node_id,
            credential_key_id,
            snapshot_timeout,
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
    ) -> Result<Option<(PathBuf, ClusterBackupManifest)>, StoreError> {
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
        let Some(client) = self.client.as_ref() else {
            self.metrics.error.fetch_add(1, Ordering::Relaxed);
            return Err(StoreError::Task(
                "portable cluster backup requires the activated replicated store".to_owned(),
            ));
        };
        let Some(lease) = self.jobs.acquire_job(BACKUP_RESOURCE.to_owned()).await? else {
            self.metrics.skipped.fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        };
        let result = self.build_under_lease(client, &destination).await;
        if let Err(error) = lease.release().await {
            tracing::warn!(error = %error, "backup lease cleanup was ambiguous");
        }
        match result {
            Ok((path, manifest)) => {
                self.metrics.ok.fetch_add(1, Ordering::Relaxed);
                self.metrics
                    .artifact_bytes
                    .store(manifest.image_bytes, Ordering::Relaxed);
                self.metrics.last_success_seconds.store(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .min(i64::MAX as u64) as i64,
                    Ordering::Relaxed,
                );
                let keep = self
                    .store
                    .get_setting(keys::BACKUP_KEEP)
                    .await?
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .filter(|keep| *keep > 0)
                    .unwrap_or(14);
                prune_artifacts(&destination, keep)?;
                Ok(Some((path, manifest)))
            }
            Err(error) => {
                self.metrics.error.fetch_add(1, Ordering::Relaxed);
                Err(error)
            }
        }
    }

    async fn build_under_lease(
        &self,
        client: &hiqlite::Client,
        destination: &Path,
    ) -> Result<(PathBuf, ClusterBackupManifest), StoreError> {
        let before = client.metrics_db().await.map_err(|error| {
            StoreError::Database(format!("reading local Raft metrics: {error}"))
        })?;
        if before.last_applied.as_ref().map(|log| log.index) != before.last_log_index {
            return Err(StoreError::Task(
                "local voter is behind its observed Raft log; backup skipped".to_owned(),
            ));
        }
        let applied = client.trigger_db_snapshot().await.map_err(|error| {
            StoreError::Database(format!("triggering database snapshot: {error}"))
        })?;
        let deadline = tokio::time::Instant::now() + self.snapshot_timeout;
        loop {
            let metrics = client.metrics_db().await.map_err(|error| {
                StoreError::Database(format!("waiting for database snapshot: {error}"))
            })?;
            if metrics
                .snapshot
                .as_ref()
                .is_some_and(|log| log.index >= applied)
            {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(StoreError::Task(format!(
                    "database snapshot did not publish applied index {applied} within {:?}",
                    self.snapshot_timeout
                )));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let snapshot_dir = self
            .data_dir
            .join("hiqlite")
            .join("state_machine")
            .join("snapshots");
        let pointer_path = snapshot_dir.join("current");
        let pointer_metadata = tokio::fs::metadata(&pointer_path)
            .await
            .map_err(|error| StoreError::Task(format!("reading snapshot pointer: {error}")))?;
        if pointer_metadata.len() > POINTER_MAX_BYTES {
            return Err(StoreError::Task("snapshot pointer is oversized".to_owned()));
        }
        let snapshot_id = tokio::fs::read_to_string(&pointer_path)
            .await
            .map_err(|error| StoreError::Task(format!("reading snapshot pointer: {error}")))?;
        let snapshot_id = snapshot_id.trim();
        if snapshot_id.is_empty()
            || snapshot_id == "none"
            || !matches!(
                Path::new(snapshot_id)
                    .components()
                    .collect::<Vec<_>>()
                    .as_slice(),
                [std::path::Component::Normal(_)]
            )
        {
            return Err(StoreError::Task(format!(
                "snapshot pointer contains invalid generation {snapshot_id:?}"
            )));
        }
        // Open before doing any other work. Snapshot cleanup can unlink this
        // exact pathname as soon as a later generation publishes.
        let snapshot = std::fs::File::open(snapshot_dir.join(snapshot_id)).map_err(|error| {
            StoreError::Task(format!("opening published snapshot {snapshot_id}: {error}"))
        })?;
        let destination = destination.to_path_buf();
        let data_dir = self.data_dir.clone();
        let credential_key_path = self.credential_key_path.clone();
        let node_id = self.node_id.clone();
        let key_id = self.credential_key_id.clone();
        tokio::task::spawn_blocking(move || {
            plurx_core::cluster::migration::build_cluster_backup_artifact(
                snapshot,
                &destination,
                &data_dir,
                Some(&credential_key_path),
                &node_id,
                Some(&key_id),
                crate::version::LONG,
                applied,
            )
        })
        .await
        .map_err(|error| StoreError::Task(format!("joining backup builder: {error}")))?
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
            if self.last_schedule_day.swap(day, Ordering::AcqRel) == day {
                continue;
            }
            if let Err(error) = self.build(Some(Path::new(destination.trim()))).await {
                tracing::warn!(error = %error, "scheduled portable backup failed");
            }
        }
    }
}

pub(crate) fn parse_schedule_minute(value: &str) -> Option<u64> {
    let (hour, minute) = value.trim().split_once(':')?;
    let hour = hour.parse::<u64>().ok()?;
    let minute = minute.parse::<u64>().ok()?;
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

fn prune_artifacts(destination: &Path, keep: usize) -> Result<(), StoreError> {
    let mut artifacts = std::fs::read_dir(destination)
        .map_err(|error| StoreError::Task(format!("listing backup destination: {error}")))?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("plurx-backup-")
        })
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
    manifest: Option<ClusterBackupManifest>,
}

pub(crate) async fn create(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(request): Json<BackupRequest>,
) -> Result<Json<BackupResponse>, ApiError> {
    match state.backup.build(request.output.as_deref()).await? {
        Some((path, manifest)) => Ok(Json(BackupResponse {
            status: "ok",
            path: Some(path),
            manifest: Some(manifest),
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

    #[test]
    fn schedule_is_one_strict_utc_minute() {
        assert_eq!(parse_schedule_minute("02:30"), Some(150));
        assert_eq!(parse_schedule_minute("23:59"), Some(1_439));
        assert_eq!(parse_schedule_minute("24:00"), None);
        assert_eq!(parse_schedule_minute("2:30:00"), None);
    }
}
