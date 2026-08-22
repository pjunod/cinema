use crate::snapshot_metrics::{SnapshotOperation, SnapshotTimer};
use crate::store::state_machine::sqlite::TypeConfigSqlite;
use crate::store::state_machine::sqlite::state_machine::StateMachineSqlite;
use crate::store::state_machine::sqlite::writer::{SnapshotRequest, WriterRequest};
use crate::{Node, NodeId};
use openraft::{
    RaftSnapshotBuilder, Snapshot, SnapshotMeta, StorageError, StorageIOError, StoredMembership,
};
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot};
use tokio::{fs, task};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SQLiteSnapshotBuilder {
    // pub last_applied_log_id: Option<LogId<NodeId>>,
    // pub last_membership: StoredMembership<NodeId, Node>,
    #[cfg(feature = "backup")]
    pub path_backups: String,
    pub path_snapshots: String,
    pub write_tx: flume::Sender<WriterRequest>,
    pub(crate) snapshot_files: Arc<Mutex<SnapshotFileState>>,
}

#[derive(Debug, Default)]
pub(crate) struct SnapshotFileState {
    pub(crate) current_id: Option<String>,
}

impl RaftSnapshotBuilder<TypeConfigSqlite> for SQLiteSnapshotBuilder {
    #[tracing::instrument(level = "trace", skip(self))]
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfigSqlite>, StorageError<NodeId>> {
        let timer = SnapshotTimer::start(SnapshotOperation::Build);
        // Snapshot construction and installation both replace the current
        // state-machine image. Serialize their file publication so cleanup
        // always reads the operation that actually completed last.
        let snapshot_files = self.snapshot_files.clone();
        let mut snapshot_files_guard = snapshot_files.lock().await;
        // - build new snapshot id
        // - make sure target path exists
        // - send snapshot request to db writer
        // - await vaccuum response
        // - open db snapshot file
        // - return snapshot handle

        let snapshot_id = Uuid::now_v7();

        let path = format!("{}/{}", self.path_snapshots, snapshot_id);
        let path_temp = format!("{path}.temp");
        let (ack, rx) = oneshot::channel();
        let req = WriterRequest::Snapshot(SnapshotRequest {
            snapshot_id,
            // last_membership: self.last_membership.clone(),
            path: path_temp.clone(),
            ack,
        });
        self.write_tx
            .send_async(req)
            .await
            .expect("Sender to always be listening");

        let resp = rx.await.expect("to always receive a snapshot response")?;
        fs::copy(path_temp, &path)
            .await
            .map_err(|err| StorageError::IO {
                source: StorageIOError::write_state_machine(&err),
            })?;
        let snapshot = fs::File::open(path).await.map_err(|err| StorageError::IO {
            source: StorageIOError::read_state_machine(&err),
        })?;

        let snapshot = Snapshot {
            meta: SnapshotMeta {
                last_log_id: resp.meta.last_applied_log_id,
                last_membership: resp.meta.last_membership,
                snapshot_id: snapshot_id.to_string(),
            },
            snapshot: Box::new(snapshot),
        };

        snapshot_files_guard.current_id = Some(snapshot_id.to_string());
        drop(snapshot_files_guard);
        // Cleanup can happen in the background, but it resolves the current
        // id under the same lock as later builds and installs instead of
        // retaining this task's now-stale captured id.
        task::spawn(snapshots_cleanup(
            self.path_snapshots.clone(),
            #[cfg(feature = "backup")]
            self.path_backups.clone(),
            snapshot_files,
        ));
        timer.success();
        Ok(snapshot)
    }
}

pub(crate) async fn snapshots_cleanup(
    path_snapshots: String,
    #[cfg(feature = "backup")] path_backups: String,
    snapshot_files: Arc<Mutex<SnapshotFileState>>,
) -> Result<(), StorageError<NodeId>> {
    let snapshot_files = snapshot_files.lock().await;
    let keep_id = snapshot_files.current_id.as_deref();
    let mut list = tokio::fs::read_dir(&path_snapshots)
        .await
        .map_err(|err| StorageError::IO {
            source: StorageIOError::read(&err),
        })?;

    let mut deletes = Vec::new();
    while let Ok(Some(entry)) = list.next_entry().await {
        let file_name = entry.file_name();
        let name = file_name.to_str().unwrap_or("UNKNOWN");

        let meta = entry.metadata().await.map_err(|err| StorageError::IO {
            source: StorageIOError::read(&err),
        })?;

        // we only expect sub-dirs in the snapshot dir
        if meta.is_dir() {
            warn!("Invalid folder in snapshots dir: {name}");
            continue;
        }

        // `begin_receiving_snapshot` owns `temp`; the shared current id owns
        // whichever local build or peer install actually completed last. A
        // cleanup task may run long after the build that spawned it, so its
        // captured build id is not a safe retention decision.
        if Some(name) != keep_id && name != "temp" {
            deletes.push(name.to_string());
        }
    }

    #[cfg(feature = "backup")]
    {
        debug!("Cleaning up possibly existing old backup restore files");
        let restore_path = format!("{}/{}", path_backups, crate::backup::BACKUP_DB_NAME);
        let _ = fs::remove_file(&restore_path).await;
    }

    for file_name in deletes {
        let path = format!("{path_snapshots}/{file_name}");
        if let Err(err) = fs::remove_file(path).await {
            error!("Error removing old snapshot {file_name}: {err}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod snapshot_metrics_cleanup_contract {
    use super::*;

    #[tokio::test]
    async fn snapshot_metrics_cleanup_preserves_a_different_installed_id() {
        let root =
            std::env::temp_dir().join(format!("hiqlite-snapshot-cleanup-{}", Uuid::now_v7()));
        fs::create_dir_all(&root)
            .await
            .expect("create snapshot cleanup root");
        let local_build_id = Uuid::now_v7();
        let installed_id = Uuid::now_v7();
        let local_build_path = root.join(local_build_id.to_string());
        let installed_path = root.join(installed_id.to_string());
        let receive_path = root.join("temp");
        let old_path = root.join(Uuid::now_v7().to_string());
        fs::write(&local_build_path, b"superseded local build")
            .await
            .expect("write local snapshot");
        fs::write(&installed_path, b"installed peer snapshot")
            .await
            .expect("write installed snapshot");
        fs::write(&receive_path, b"receiving")
            .await
            .expect("write inflight install");
        fs::write(&old_path, b"old")
            .await
            .expect("write old snapshot");

        #[cfg(feature = "backup")]
        let backups = root.join("backups");
        #[cfg(feature = "backup")]
        fs::create_dir_all(&backups)
            .await
            .expect("create backup cleanup root");

        let snapshot_files = Arc::new(Mutex::new(SnapshotFileState {
            current_id: Some(installed_id.to_string()),
        }));
        snapshots_cleanup(
            root.to_str().expect("UTF-8 snapshot cleanup root").to_owned(),
            #[cfg(feature = "backup")]
            backups
                .to_str()
                .expect("UTF-8 backup cleanup root")
                .to_owned(),
            snapshot_files,
        )
        .await
        .expect("clean old snapshots");

        assert!(!local_build_path.exists());
        assert!(installed_path.exists());
        assert!(receive_path.exists());
        assert!(!old_path.exists());
        fs::remove_dir_all(root)
            .await
            .expect("remove snapshot cleanup root");
    }
}
