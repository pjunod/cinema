//! Private staged-startup ownership; never a membership-removal shortcut.
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Actual staged listeners and their spawned connections survive Client handoff.
#[derive(Default)]
pub(crate) struct StartupListenerOwner {
    listeners: Vec<(
        tokio::task::JoinHandle<()>,
        axum_server::Handle<std::net::SocketAddr>,
    )>,
}

impl StartupListenerOwner {
    pub(crate) fn track(
        &mut self,
        task: tokio::task::JoinHandle<()>,
        handle: axum_server::Handle<std::net::SocketAddr>,
    ) {
        self.listeners.push((task, handle));
    }

    pub(crate) fn stop_admitting(&self) {
        for (_, handle) in &self.listeners {
            handle.graceful_shutdown(None);
        }
    }

    pub(crate) async fn finish(&mut self) -> Result<(), crate::Error> {
        for (_, handle) in &self.listeners {
            // Durable work has drained before residual idle transports close.
            handle.shutdown();
        }
        for (task, handle) in &mut self.listeners {
            task.await
                .map_err(|error| crate::Error::Error(error.to_string().into()))?;
            // Server shutdown may return before its connection tasks. The
            // actual watcher count, including bounded TLS handshakes, is the fence.
            while handle.connection_count() != 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        self.listeners.clear();
        Ok(())
    }
}

pub(crate) struct StartupStorageOwner {
    enabled: bool,
    cleanup: Vec<Pin<Box<dyn Future<Output = ()> + Send>>>,
}

impl StartupStorageOwner {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            cleanup: Vec::new(),
        }
    }

    pub(crate) fn handoff(&mut self) {
        self.cleanup.clear();
    }

    pub(crate) fn protect_wal(&mut self, wal: hiqlite_wal::ShutdownHandle) {
        if self.enabled {
            self.cleanup.push(Box::pin(async move {
                if let Err(error) = wal.shutdown().await {
                    tracing::error!(?error, "staged partial WAL cleanup failed");
                }
            }));
        }
    }

    #[cfg(feature = "sqlite")]
    pub(crate) fn protect_writer(
        &mut self,
        writer: flume::Sender<crate::store::state_machine::sqlite::writer::WriterRequest>,
    ) {
        if self.enabled {
            self.cleanup.push(Box::pin(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if writer
                    .send_async(
                        crate::store::state_machine::sqlite::writer::WriterRequest::Shutdown(tx),
                    )
                    .await
                    .is_err()
                    || rx.await.is_err()
                {
                    tracing::error!("staged partial SQLite writer cleanup failed");
                }
            }));
        }
    }

    #[cfg(feature = "sqlite")]
    pub(crate) fn protect_db(&mut self, state: &crate::app_state::StateRaftDB) {
        if !self.enabled {
            return;
        }
        let stopped = state.is_raft_stopped.clone();
        let snapshots = state.snapshot_executor.clone();
        let raft = state.raft.clone();
        let wal = state.shutdown_handle.clone();
        let writer = state.sql_writer.clone();
        self.cleanup.push(Box::pin(async move {
            stopped.store(true, Ordering::Relaxed);
            snapshots.request_shutdown();
            // Never cancel an accepted durable snapshot to satisfy a caller
            // deadline. Retain this owner until it releases storage ownership.
            while !snapshots.wait_for_shutdown(Duration::from_secs(5)).await {
                tracing::warn!("staged SQLite cleanup retains accepted snapshot ownership");
            }
            if let Err(error) = raft.shutdown().await {
                tracing::error!(
                    ?error,
                    "staged SQLite Raft cleanup failed; no clean termination claim"
                );
                return;
            }
            if let Err(error) = wal.shutdown().await {
                tracing::error!(
                    ?error,
                    "staged SQLite WAL cleanup failed; no clean termination claim"
                );
                return;
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            if writer
                .send_async(
                    crate::store::state_machine::sqlite::writer::WriterRequest::Shutdown(tx),
                )
                .await
                .is_err()
                || rx.await.is_err()
            {
                tracing::error!("staged SQLite writer cleanup failed; no clean termination claim");
            }
        }));
    }

    #[cfg(feature = "cache")]
    pub(crate) fn protect_cache(&mut self, state: &crate::app_state::StateRaftCache) {
        if !self.enabled {
            return;
        }
        let stopped = state.is_raft_stopped.clone();
        let snapshots = state.snapshot_executor.clone();
        let raft = state.raft.clone();
        let wal = state.shutdown_handle.clone();
        self.cleanup.push(Box::pin(async move {
            stopped.store(true, Ordering::Relaxed);
            snapshots.request_shutdown();
            while !snapshots.wait_for_shutdown(Duration::from_secs(5)).await {
                tracing::warn!("staged cache cleanup retains accepted snapshot ownership");
            }
            if let Err(error) = raft.shutdown().await {
                tracing::error!(
                    ?error,
                    "staged cache Raft cleanup failed; no clean termination claim"
                );
                return;
            }
            if let Some(wal) = wal
                && let Err(error) = wal.shutdown().await
            {
                tracing::error!(
                    ?error,
                    "staged cache WAL cleanup failed; no clean termination claim"
                );
            }
            // No leave/remove proposal: durable fenced reduction is separate.
        }));
    }
}

/// The blocking WAL constructor cannot be interrupted safely. Its owned task
/// must deliver the result to either startup or cancellation cleanup, never
/// to a dropped JoinHandle while the interval syncer retains the writer.
struct PendingWal<C: openraft::RaftTypeConfig> {
    task: Option<
        tokio::task::JoinHandle<Result<hiqlite_wal::LogStore<C>, hiqlite_wal::error::Error>>,
    >,
}

impl<C: openraft::RaftTypeConfig> Drop for PendingWal<C> {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            tokio::spawn(async move {
                match task.await {
                    Ok(Ok(store)) => {
                        if let Err(error) = store.shutdown_handle().shutdown().await {
                            tracing::error!(?error, "cancelled WAL constructor cleanup failed");
                        }
                    }
                    Ok(Err(error)) => tracing::error!(?error, "cancelled WAL constructor failed"),
                    Err(error) => tracing::error!(?error, "cancelled WAL constructor task failed"),
                }
            });
        }
    }
}

pub(crate) async fn start_wal<C: openraft::RaftTypeConfig>(
    path: String,
    sync: hiqlite_wal::LogSync,
    size: u32,
    staged: bool,
) -> Result<hiqlite_wal::LogStore<C>, crate::Error> {
    if !staged {
        return Ok(hiqlite_wal::LogStore::<C>::start(path, sync, size).await?);
    }
    let mut pending = PendingWal {
        task: Some(tokio::spawn(hiqlite_wal::LogStore::<C>::start_staged(
            path, sync, size,
        ))),
    };
    let result = pending.task.as_mut().expect("owned constructor task").await;
    pending.task.take();
    Ok(result??)
}

#[cfg(feature = "sqlite")]
type SqliteState = crate::store::state_machine::sqlite::state_machine::StateMachineSqlite;

#[cfg(feature = "sqlite")]
struct PendingSqlite {
    task: Option<tokio::task::JoinHandle<Result<SqliteState, crate::Error>>>,
}

#[cfg(feature = "sqlite")]
impl Drop for PendingSqlite {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            tokio::spawn(async move {
                match task.await {
                    Ok(Ok(state)) => {
                        // Constructor-owned snapshot recovery is complete
                        // before its writer can be drained; never abort it.
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        if state.write_tx.send_async(crate::store::state_machine::sqlite::writer::WriterRequest::Shutdown(tx)).await.is_err()
                            || rx.await.is_err() {
                            tracing::error!("cancelled SQLite constructor writer cleanup failed");
                        }
                    }
                    Ok(Err(error)) => {
                        tracing::error!(?error, "cancelled SQLite constructor failed")
                    }
                    Err(error) => {
                        tracing::error!(?error, "cancelled SQLite constructor task failed")
                    }
                }
            });
        }
    }
}

#[cfg(feature = "sqlite")]
pub(crate) async fn construct_sqlite<F>(constructor: F) -> Result<SqliteState, crate::Error>
where
    F: Future<Output = Result<SqliteState, crate::Error>> + Send + 'static,
{
    let mut pending = PendingSqlite {
        task: Some(tokio::spawn(constructor)),
    };
    let result = pending
        .task
        .as_mut()
        .expect("owned SQLite constructor task")
        .await;
    pending.task.take();
    result?
}

impl Drop for StartupStorageOwner {
    fn drop(&mut self) {
        for cleanup in self.cleanup.drain(..) {
            // This owner survives cancellation of the requesting startup
            // future. Shutdown ordering never depends on that future polling.
            tokio::spawn(cleanup);
        }
    }
}

#[cfg(feature = "cache")]
type CacheState = crate::store::state_machine::memory::state_machine::StateMachineMemory;

#[cfg(feature = "cache")]
struct PendingCache {
    task: Option<tokio::task::JoinHandle<Result<CacheState, crate::Error>>>,
}

#[cfg(feature = "cache")]
impl Drop for PendingCache {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            tokio::spawn(async move {
                match task.await {
                    // Finish accepted snapshot restoration before dropping
                    // sender ownership. TTL receivers then close and release
                    // their KV senders; no restoration task is aborted.
                    Ok(Ok(state)) => drop(state),
                    Ok(Err(error)) => tracing::error!(?error, "cancelled cache constructor failed"),
                    Err(error) => {
                        tracing::error!(?error, "cancelled cache constructor task failed")
                    }
                }
            });
        }
    }
}

#[cfg(feature = "cache")]
pub(crate) async fn construct_cache<F>(constructor: F) -> Result<CacheState, crate::Error>
where
    F: Future<Output = Result<CacheState, crate::Error>> + Send + 'static,
{
    let mut pending = PendingCache {
        task: Some(tokio::spawn(constructor)),
    };
    let result = pending
        .task
        .as_mut()
        .expect("owned cache constructor task")
        .await;
    pending.task.take();
    result?
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn k06_handed_off_tls_drain_retains_lease_until_real_writer_and_connections_stop() {
        use crate::store::state_machine::sqlite::writer::WriterRequest;
        use std::fs::OpenOptions;
        use std::net::TcpListener;
        use std::time::Duration;
        let root = std::env::temp_dir().join(format!("k06-client-drain-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let lease_path = root.join("daemon.lock");
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lease_path)
            .unwrap();
        lease.lock().unwrap();
        let reserve = || {
            TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
        };
        let raft = reserve();
        let mut api = reserve();
        while api == raft {
            api = reserve();
        }
        let config = crate::NodeConfig {
            node_id: 1,
            nodes: vec![crate::Node {
                id: 1,
                addr_raft: raft.to_string(),
                addr_api: api.to_string(),
            }],
            data_dir: root.join("db").to_string_lossy().into_owned().into(),
            listen_addr_api: "127.0.0.1".into(),
            listen_addr_raft: "127.0.0.1".into(),
            secret_raft: "k06-drain-raft-secret".into(),
            secret_api: "k06-drain-api-secret".into(),
            tls_api: Some(crate::tls::ServerTlsConfig::TlsAutoCertificates),
            tls_raft: Some(crate::tls::ServerTlsConfig::TlsAutoCertificates),
            ..Default::default()
        };
        // This private factory fixture proves resource ownership, not clock
        // admission. No observer, fabricated sample or promotion is supplied.
        let client = crate::start::start_node_inner::<crate::empty::Empty>(
            Box::new(config),
            None,
            crate::start::StartupPhase::ClockObservation,
        )
        .await
        .unwrap();
        client.wait_until_healthy_db().await;
        assert!(client.has_retained_startup_resources());
        let state = client.inner.state.as_ref().unwrap();
        let wal = state.raft_db.wal_status.clone();
        let membership = client
            .local_db_raft_metrics()
            .unwrap()
            .membership_snapshot();
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap();
        assert!(
            http.get(format!("https://{api}/ping"))
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
        // Real socket accepted by the TLS listener; its handshake is unfinished.
        let pending_tls = tokio::net::TcpStream::connect(api).await.unwrap();
        let (entered, accepted) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        state
            .raft_db
            .sql_writer
            .send_async(WriterRequest::HoldForStartupDrain {
                entered,
                release: release_rx,
            })
            .await
            .unwrap();
        accepted.await.unwrap();
        let (done, drained) = tokio::sync::oneshot::channel();
        let cleanup_client = client.clone();
        let cleanup = tokio::spawn(async move {
            cleanup_client.shutdown_retained_startup().await.unwrap();
            drop(lease);
            let _ = done.send(());
        });
        let requester = tokio::spawn(async move { cleanup.await });
        requester.abort();
        assert!(requester.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if tokio::net::TcpStream::connect(api).await.is_err()
                    && tokio::net::TcpStream::connect(raft).await.is_err()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let probe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lease_path)
            .unwrap();
        assert!(
            probe.try_lock().is_err(),
            "directory lease released while accepted writer work is held"
        );
        assert_eq!(
            client
                .local_db_raft_metrics()
                .unwrap()
                .membership_snapshot()
                .members,
            membership.members,
            "cleanup must not remove membership"
        );
        release.send(()).unwrap();
        drop(pending_tls);
        tokio::time::timeout(Duration::from_secs(15), drained)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            wal.snapshot().state,
            hiqlite_wal::WalRuntimeState::Stopped,
            "WAL must be terminal before lease release"
        );
        assert!(
            state.raft_db.sql_writer.is_disconnected(),
            "SQL writer must be terminal before lease release"
        );
        assert!(!client.has_retained_startup_resources());
        probe.try_lock().unwrap();
        let _raft = TcpListener::bind(raft).unwrap();
        let _api = TcpListener::bind(api).unwrap();
        drop(probe);
        drop(http);
        drop(client);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn k06_cancelled_wal_constructor_retains_real_writer_until_drained() {
        use crate::store::state_machine::sqlite::TypeConfigSqlite;
        let root = std::env::temp_dir().join(format!(
            "k06-wal-cancel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.to_string_lossy().into_owned();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let owned_path = path.clone();
        let requester = tokio::spawn(async move {
            let mut pending = super::PendingWal {
                task: Some(tokio::spawn(async move {
                    let store = hiqlite_wal::LogStore::<TypeConfigSqlite>::start_staged(
                        owned_path,
                        hiqlite_wal::LogSync::IntervalMillis(60_000),
                        1024 * 1024,
                    )
                    .await?;
                    ready_tx.send(store.status_handle()).unwrap();
                    // Deterministic constructor-result handoff boundary. The
                    // production owner must survive cancellation here.
                    release_rx.await.unwrap();
                    Ok(store)
                })),
            };
            let result = pending.task.as_mut().unwrap().await;
            pending.task.take();
            result
        });
        let status = ready_rx.await.expect("real writer started");
        requester.abort();
        assert!(requester.await.unwrap_err().is_cancelled());
        assert_ne!(
            status.snapshot().state,
            hiqlite_wal::WalRuntimeState::Stopped
        );
        release_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while status.snapshot().state != hiqlite_wal::WalRuntimeState::Stopped {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("owned cancellation cleanup drains real writer");
        let replacement = hiqlite_wal::LogStore::<TypeConfigSqlite>::start(
            path,
            hiqlite_wal::LogSync::Immediate,
            1024 * 1024,
        )
        .await
        .expect("OS lock released, not merely requester cancelled");
        replacement.shutdown_handle().shutdown().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn k06_staged_wal_timer_panic_drains_actual_writer_before_error() {
        use crate::store::state_machine::sqlite::TypeConfigSqlite;
        let root = std::env::temp_dir().join(format!(
            "k06-wal-panic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.to_string_lossy().into_owned();
        // A real zero interval panics after the real writer thread starts.
        // The staged blocking owner must finish that writer, not abort it.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::start_wal::<TypeConfigSqlite>(
                path.clone(),
                hiqlite_wal::LogSync::IntervalMillis(0),
                1024 * 1024,
                true,
            ),
        )
        .await
        .expect("bounded constructor refusal");
        assert!(result.is_err(), "timer panic must remain a refusal");
        // Reopening the SAME real directory proves the old writer released
        // its OS lock; success alone or a vanished requesting task does not.
        let replacement = hiqlite_wal::LogStore::<TypeConfigSqlite>::start(
            path,
            hiqlite_wal::LogSync::Immediate,
            1024 * 1024,
        )
        .await
        .expect("actual writer lock released after panic");
        replacement
            .shutdown_handle()
            .shutdown()
            .await
            .expect("replacement writer drained");
        std::fs::remove_dir_all(root).expect("remove exact owned test directory");
    }
}
