//! Adapter for the existing replicated backup cut and snapshot ownership.
use super::{BackupArtifact, BackupManager, BackupProvider};
use crate::state::JobManager;
use plurx_core::cluster::migration::ClusterBackupManifest;
use plurx_core::error::StoreError;
use plurx_core::store::Store;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub(super) const BACKUP_RESOURCE: &str = "backup:cluster";
const BACKUP_ANCHOR_SQL: &str = "UPDATE settings SET updated_at = updated_at WHERE 0";
const POINTER_MAX_BYTES: u64 = 128;

pub(crate) struct ClusterBackupProvider {
    client: hiqlite::Client,
    data_dir: PathBuf,
    credential_key_path: PathBuf,
    node_id: String,
    credential_key_id: String,
    snapshot_timeout: Duration,
}
impl ClusterBackupProvider {
    pub(crate) fn new(
        client: hiqlite::Client,
        data_dir: PathBuf,
        credential_key_path: PathBuf,
        node_id: String,
        credential_key_id: String,
        snapshot_timeout: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            data_dir,
            credential_key_path,
            node_id,
            credential_key_id,
            snapshot_timeout,
        })
    }
    async fn build_cluster(
        &self,
        client: &hiqlite::Client,
        destination: &Path,
    ) -> Result<(PathBuf, ClusterBackupManifest), StoreError> {
        // One budget belongs to this already-acquired backup lease. Catch-up,
        // the one snapshot trigger and publication all consume this deadline.
        let deadline = tokio::time::Instant::now() + self.snapshot_timeout;
        let local_progress = || async {
            let metrics = client.metrics_db().await.map_err(|error| {
                StoreError::Database(format!("reading local Raft metrics: {error}"))
            })?;
            Ok((
                metrics.last_applied.as_ref().map(|log| log.index),
                metrics.last_log_index,
            ))
        };
        wait_for_local_backup_apply(deadline, 0, local_progress).await?;
        // The lease holder is any voter, and only the leader may propose an
        // entry itself. The anchor therefore goes through the write path that
        // forwards to the leader and reports the committed index; this node
        // then applies through it and cuts its own snapshot there.
        let applied = backup_phase_before_deadline(deadline, "snapshot anchor", async {
            client
                .execute_acked(BACKUP_ANCHOR_SQL, hiqlite::macros::params!())
                .await
                .map_err(|error| {
                    StoreError::Database(format!("anchoring the backup cut: {error}"))
                })?
                .log_index
                .ok_or_else(|| {
                    StoreError::Task(
                        "the leader did not report the backup anchor's log index".to_owned(),
                    )
                })
        })
        .await?;
        wait_for_local_backup_apply(deadline, applied, local_progress).await?;
        backup_phase_before_deadline(deadline, "snapshot trigger", async {
            client.trigger_local_db_snapshot().await.map_err(|error| {
                StoreError::Database(format!("triggering database snapshot: {error}"))
            })
        })
        .await?;
        loop {
            let metrics = backup_phase_before_deadline(deadline, "snapshot publication", async {
                client.metrics_db().await.map_err(|error| {
                    StoreError::Database(format!("waiting for database snapshot: {error}"))
                })
            })
            .await?;
            if metrics
                .snapshot
                .as_ref()
                .is_some_and(|log| log.index >= applied)
            {
                break;
            }
            tokio::time::sleep_until(
                deadline.min(tokio::time::Instant::now() + Duration::from_millis(50)),
            )
            .await;
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
}

#[async_trait::async_trait]
impl BackupProvider for ClusterBackupProvider {
    fn lease_resource(&self) -> &'static str {
        BACKUP_RESOURCE
    }
    fn artifact_prefix(&self) -> &'static str {
        "plurx-backup-"
    }
    async fn build(&self, destination: &Path) -> Result<BackupArtifact, StoreError> {
        let (path, manifest) = self.build_cluster(&self.client, destination).await?;
        let image_bytes = manifest.image_bytes;
        let manifest = serde_json::to_value(manifest).map_err(|error| {
            StoreError::Task(format!("encoding cluster backup response: {error}"))
        })?;
        Ok(BackupArtifact {
            path,
            manifest,
            image_bytes,
        })
    }
}

/// Compatibility only for callers still supplying the former optional client.
/// Normal daemon composition injects its actual provider explicitly.
impl BackupManager {
    #[allow(clippy::too_many_arguments, dead_code)]
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
        let provider: Arc<dyn BackupProvider> = match client {
            Some(client) => ClusterBackupProvider::new(
                client,
                data_dir,
                credential_key_path,
                node_id,
                credential_key_id,
                snapshot_timeout,
            ),
            None => Arc::new(UnavailableLegacyProvider),
        };
        Self::new_with_provider(store, jobs, provider)
    }
}
struct UnavailableLegacyProvider;
#[async_trait::async_trait]
impl BackupProvider for UnavailableLegacyProvider {
    fn lease_resource(&self) -> &'static str {
        BACKUP_RESOURCE
    }
    fn artifact_prefix(&self) -> &'static str {
        "plurx-backup-"
    }
    async fn build(&self, _destination: &Path) -> Result<BackupArtifact, StoreError> {
        Err(StoreError::Task(
            "backup provider was not selected for this runtime".to_owned(),
        ))
    }
}

pub(super) async fn backup_phase_before_deadline<T>(
    deadline: tokio::time::Instant,
    phase: &str,
    work: impl Future<Output = Result<T, StoreError>>,
) -> Result<T, StoreError> {
    if tokio::time::Instant::now() >= deadline {
        return Err(StoreError::Task(format!(
            "backup {phase} exceeded the original snapshot deadline"
        )));
    }
    let result = tokio::time::timeout_at(deadline, work).await.map_err(|_| {
        StoreError::Task(format!(
            "backup {phase} exceeded the original snapshot deadline"
        ))
    })?;
    // A ready future can win a poll against its simultaneously ready timer.
    // Even then, completion at/after the original deadline is not admission.
    if tokio::time::Instant::now() >= deadline {
        return Err(StoreError::Task(format!(
            "backup {phase} exceeded the original snapshot deadline"
        )));
    }
    result
}

/// Observe only local metrics: no write, snapshot trigger, lease acquisition
/// or deadline refresh is allowed while the local apply queue catches up.
/// The wait ends once this replica has applied everything it holds and at
/// least `through`, an index a follower may not have received yet.
pub(super) async fn wait_for_local_backup_apply<F, Fut>(
    deadline: tokio::time::Instant,
    through: u64,
    mut read_progress: F,
) -> Result<(), StoreError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(Option<u64>, Option<u64>), StoreError>>,
{
    loop {
        let (applied, last_log) =
            backup_phase_before_deadline(deadline, "local Raft catch-up", read_progress()).await?;
        if applied == last_log && applied.unwrap_or(0) >= through {
            return Ok(());
        }
        tokio::time::sleep_until(
            deadline.min(tokio::time::Instant::now() + Duration::from_millis(50)),
        )
        .await;
    }
}
