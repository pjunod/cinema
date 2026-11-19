//! Private staged-startup ownership; never a membership-removal shortcut.
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub(crate) type Keepalive = Option<std::sync::Arc<dyn Send + Sync>>;

/// OpenRaft 0.9 joins its core without joining the state-machine worker or
/// its detached snapshot builders. Each actual SQL producer owns a sender;
/// receivers observe retirement only after the last producer has dropped.
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub(crate) struct SqliteStorageOwner(tokio::sync::watch::Sender<()>);

#[cfg(feature = "sqlite")]
impl Default for SqliteStorageOwner {
    fn default() -> Self {
        Self(tokio::sync::watch::channel(()).0)
    }
}

#[cfg(feature = "sqlite")]
impl SqliteStorageOwner {
    pub(crate) fn drain(&self) -> SqliteStorageDrain {
        SqliteStorageDrain(self.0.subscribe())
    }
}

#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub(crate) struct SqliteStorageDrain(tokio::sync::watch::Receiver<()>);

#[cfg(feature = "sqlite")]
impl SqliteStorageDrain {
    pub(crate) async fn wait(&self) {
        let mut retired = self.0.clone();
        while retired.changed().await.is_ok() {}
    }
}

/// An unconfirmed owner must never release the caller's directory lease.
#[derive(Default)]
pub(crate) struct CompletionClaim(Keepalive);

impl CompletionClaim {
    pub(crate) fn new(keepalive: &Keepalive) -> Self {
        Self(keepalive.clone())
    }
    pub(crate) fn confirmed(&mut self) {
        self.0.take();
    }
}

impl Drop for CompletionClaim {
    fn drop(&mut self) {
        if let Some(keepalive) = self.0.take() {
            tracing::error!("startup retirement is unconfirmed; retaining caller lease");
            std::mem::forget(keepalive);
        }
    }
}

struct StorageCleanup {
    work: Pin<Box<dyn Future<Output = Result<(), ()>> + Send>>,
    claim: CompletionClaim,
}

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
    cleanup: Vec<StorageCleanup>,
    keepalive: Keepalive,
}

impl StartupStorageOwner {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            cleanup: Vec::new(),
            keepalive: None,
        }
    }

    pub(crate) fn retain(&mut self, keepalive: Keepalive) {
        self.keepalive = keepalive;
    }

    pub(crate) fn handoff(&mut self) {
        for cleanup in &mut self.cleanup {
            cleanup.claim.confirmed();
        }
        self.cleanup.clear();
    }

    fn protect(&mut self, work: impl Future<Output = Result<(), ()>> + Send + 'static) {
        self.cleanup.push(StorageCleanup {
            work: Box::pin(work),
            claim: CompletionClaim::new(&self.keepalive),
        });
    }

    pub(crate) fn protect_wal(&mut self, wal: hiqlite_wal::ShutdownHandle) {
        if self.enabled {
            self.protect(async move {
                if let Err(error) = wal.shutdown().await {
                    tracing::error!(?error, "staged partial WAL cleanup failed");
                    return Err(());
                }
                Ok(())
            });
        }
    }

    #[cfg(feature = "sqlite")]
    pub(crate) fn protect_writer(
        &mut self,
        writer: flume::Sender<crate::store::state_machine::sqlite::writer::WriterRequest>,
    ) {
        self.protect_sqlite_writer(writer, None);
    }

    #[cfg(feature = "sqlite")]
    pub(crate) fn protect_sqlite_writer(
        &mut self,
        writer: flume::Sender<crate::store::state_machine::sqlite::writer::WriterRequest>,
        producers: Option<SqliteStorageDrain>,
    ) {
        if self.enabled {
            self.protect(async move {
                if let Some(producers) = producers {
                    producers.wait().await;
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
                    tracing::error!("staged partial SQLite writer cleanup failed");
                    return Err(());
                }
                Ok(())
            });
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
        let producers = state.storage_drain.clone();
        self.protect(async move {
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
                return Err(());
            }
            // RaftCore termination is not state-machine termination. Keep
            // storage live through every accepted apply and snapshot build.
            producers.wait().await;
            if let Err(error) = wal.shutdown().await {
                tracing::error!(
                    ?error,
                    "staged SQLite WAL cleanup failed; no clean termination claim"
                );
                return Err(());
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
                return Err(());
            }
            Ok(())
        });
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
        self.protect(async move {
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
                return Err(());
            }
            if let Some(wal) = wal
                && let Err(error) = wal.shutdown().await
            {
                tracing::error!(
                    ?error,
                    "staged cache WAL cleanup failed; no clean termination claim"
                );
                return Err(());
            }
            // No leave/remove proposal: durable fenced reduction is separate.
            Ok(())
        });
    }
}

/// The blocking WAL constructor cannot be interrupted safely. Its owned task
/// must deliver the result to either startup or cancellation cleanup, never
/// to a dropped JoinHandle while the interval syncer retains the writer.
struct PendingWal<C: openraft::RaftTypeConfig> {
    claim: CompletionClaim,
    task: Option<
        tokio::task::JoinHandle<Result<hiqlite_wal::LogStore<C>, hiqlite_wal::error::Error>>,
    >,
}

impl<C: openraft::RaftTypeConfig> Drop for PendingWal<C> {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            let mut claim = std::mem::take(&mut self.claim);
            tokio::spawn(async move {
                match task.await {
                    Ok(Ok(store)) => {
                        let stopped = store.shutdown_handle().shutdown().await;
                        drop(store);
                        match stopped {
                            Ok(()) => claim.confirmed(),
                            Err(error) => {
                                tracing::error!(?error, "cancelled WAL constructor cleanup failed")
                            }
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
    keepalive: Keepalive,
) -> Result<hiqlite_wal::LogStore<C>, crate::Error> {
    if !staged {
        return Ok(hiqlite_wal::LogStore::<C>::start(path, sync, size).await?);
    }
    let claim = CompletionClaim::new(&keepalive);
    let mut pending = PendingWal {
        claim,
        task: Some(tokio::spawn(hiqlite_wal::LogStore::<C>::start_staged(
            path, sync, size,
        ))),
    };
    let result = pending.task.as_mut().expect("owned constructor task").await;
    pending.task.take();
    let store = result??;
    pending.claim.confirmed();
    Ok(store)
}

#[cfg(feature = "sqlite")]
type SqliteState = crate::store::state_machine::sqlite::state_machine::StateMachineSqlite;

#[cfg(feature = "sqlite")]
struct PendingSqlite {
    claim: CompletionClaim,
    task: Option<tokio::task::JoinHandle<Result<SqliteState, crate::Error>>>,
}

#[cfg(feature = "sqlite")]
impl Drop for PendingSqlite {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            let mut claim = std::mem::take(&mut self.claim);
            tokio::spawn(async move {
                match task.await {
                    Ok(Ok(state)) => {
                        // Constructor-owned snapshot recovery is complete
                        // before its writer can be drained; never abort it.
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        if state.write_tx.send_async(crate::store::state_machine::sqlite::writer::WriterRequest::Shutdown(tx)).await.is_err()
                            || rx.await.is_err() {
                            tracing::error!("cancelled SQLite constructor writer cleanup failed");
                        } else {
                            drop(state);
                            claim.confirmed();
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
pub(crate) async fn construct_sqlite<F>(
    constructor: F,
    keepalive: Keepalive,
) -> Result<SqliteState, crate::Error>
where
    F: Future<Output = Result<SqliteState, crate::Error>> + Send + 'static,
{
    let claim = CompletionClaim::new(&keepalive);
    let mut pending = PendingSqlite {
        claim,
        task: Some(tokio::spawn(constructor)),
    };
    let result = pending
        .task
        .as_mut()
        .expect("owned SQLite constructor task")
        .await;
    pending.task.take();
    let state = result??;
    pending.claim.confirmed();
    Ok(state)
}

impl Drop for StartupStorageOwner {
    fn drop(&mut self) {
        for cleanup in self.cleanup.drain(..) {
            // This owner survives cancellation of the requesting startup
            // future. Shutdown ordering never depends on that future polling.
            tokio::spawn(async move {
                let StorageCleanup { work, mut claim } = cleanup;
                if work.await.is_ok() {
                    claim.confirmed();
                }
            });
        }
    }
}

#[cfg(feature = "cache")]
type CacheState = crate::store::state_machine::memory::state_machine::StateMachineMemory;

#[cfg(feature = "cache")]
struct PendingCache {
    claim: CompletionClaim,
    task: Option<tokio::task::JoinHandle<Result<CacheState, crate::Error>>>,
}

#[cfg(feature = "cache")]
impl Drop for PendingCache {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            let claim = std::mem::take(&mut self.claim);
            tokio::spawn(async move {
                let _retained_claim = claim;
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
pub(crate) async fn construct_cache<F>(
    constructor: F,
    keepalive: Keepalive,
) -> Result<CacheState, crate::Error>
where
    F: Future<Output = Result<CacheState, crate::Error>> + Send + 'static,
{
    let claim = CompletionClaim::new(&keepalive);
    let mut pending = PendingCache {
        claim,
        task: Some(tokio::spawn(constructor)),
    };
    let result = pending
        .task
        .as_mut()
        .expect("owned cache constructor task")
        .await;
    pending.task.take();
    let state = result??;
    pending.claim.confirmed();
    Ok(state)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    #[tokio::test]
    async fn sqlite_retirement_waits_for_detached_snapshot_builder_before_writer_and_root_release()
    {
        use crate::store::state_machine::sqlite::{
            state_machine::StateMachineSqlite, writer::WriterRequest,
        };
        use openraft::{RaftSnapshotBuilder, RaftStateMachine};
        use std::future::Future as _;

        let root =
            std::env::temp_dir().join(format!("sqlite-producer-drain-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let lock = root.join("root.lock");
        let lease = std::fs::File::create(&lock).unwrap();
        lease.lock().unwrap();
        let probe = std::fs::File::open(&lock).unwrap();
        let data = root.join("data").to_string_lossy().into_owned();
        let mut state = StateMachineSqlite::new(
            &data,
            "state.db",
            1,
            false,
            16,
            1,
            std::time::Duration::from_secs(600),
            #[cfg(feature = "s3")]
            None,
            false,
            #[cfg(feature = "backup")]
            30,
            false,
            crate::applied_mark::AppliedMark::new::<&str>(&[]),
        )
        .await
        .unwrap();
        let writer = state.write_tx.clone();
        let mut owner = super::StartupStorageOwner::new(true);
        owner.retain(Some(std::sync::Arc::new(lease)));
        owner.protect_sqlite_writer(writer.clone(), Some(state.storage_owner.drain()));
        let builder = state.get_snapshot_builder().await;
        let mut detached = builder.clone();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let build = tokio::spawn(async move {
            entered.send(()).unwrap();
            released.await.unwrap();
            // This is the actual writer-backed builder, after its parent
            // state machine retired, as in OpenRaft's detached build task.
            detached.build_snapshot().await.unwrap()
        });
        started.await.unwrap();
        drop(state);
        let super::StorageCleanup {
            mut work,
            mut claim,
        } = owner.cleanup.pop().unwrap();
        drop(owner);
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(work.as_mut().poll(cx).is_pending()))
                .await,
            "a live snapshot builder still owns storage after the worker retires"
        );
        drop(builder);
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(work.as_mut().poll(cx).is_pending()))
                .await,
            "the detached builder clone retains its own producer lifetime"
        );
        assert!(probe.try_lock().is_err(), "actual root lease remains held");
        let (metadata, read) = tokio::sync::oneshot::channel();
        writer
            .send_async(WriterRequest::MetadataRead(metadata))
            .await
            .unwrap();
        read.await
            .expect("the real SQL writer remains live for accepted work");
        release.send(()).unwrap();
        drop(build.await.unwrap());
        assert!(
            probe.try_lock().is_err(),
            "producer retirement is not a writer ACK"
        );
        assert!(work.await.is_ok(), "the real writer confirms its shutdown");
        claim.confirmed();
        assert!(writer.is_disconnected());
        assert!(!root.join("data/state_machine/lock").exists());
        probe
            .try_lock()
            .expect("only confirmed drainage releases the root");
        drop(probe);
        std::fs::remove_dir_all(root).unwrap();
    }

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
        let requester = tokio::spawn(cleanup);
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
        std::fs::create_dir_all(&root).unwrap();
        let lock_path = root.join("root.lock");
        let lease = std::fs::File::create(&lock_path).unwrap();
        lease.lock().unwrap();
        let probe = std::fs::File::open(&lock_path).unwrap();
        let keepalive: super::Keepalive = Some(std::sync::Arc::new(lease));
        let claim = super::CompletionClaim::new(&keepalive);
        drop(keepalive);
        let path = root.join("wal").to_string_lossy().into_owned();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let owned_path = path.clone();
        let requester = tokio::spawn(async move {
            let mut pending = super::PendingWal {
                claim,
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
        assert!(
            probe.try_lock().is_err(),
            "constructor cancellation must retain the actual root lease"
        );
        let released = tokio::task::spawn_blocking(move || {
            probe.lock().unwrap();
            probe
        });
        release_tx.send(()).unwrap();
        let probe = tokio::time::timeout(std::time::Duration::from_secs(5), released)
            .await
            .expect("owned cancellation cleanup drains real writer")
            .expect("root lease waiter joined");
        assert_eq!(
            status.snapshot().state,
            hiqlite_wal::WalRuntimeState::Stopped
        );
        let replacement = hiqlite_wal::LogStore::<TypeConfigSqlite>::start(
            path,
            hiqlite_wal::LogSync::Immediate,
            1024 * 1024,
        )
        .await
        .expect("OS lock released, not merely requester cancelled");
        replacement.shutdown_handle().shutdown().await.unwrap();
        drop(probe);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn startup_missing_writer_ack_retains_the_root_lease() {
        let root =
            std::env::temp_dir().join(format!("startup-missing-ack-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("root.lock");
        let lease = std::fs::File::create(&path).unwrap();
        lease.lock().unwrap();
        let probe = std::fs::File::open(&path).unwrap();
        let mut owner = super::StartupStorageOwner::new(true);
        owner.retain(Some(std::sync::Arc::new(lease)));
        let (writer, requests) = flume::unbounded();
        let producer = super::SqliteStorageOwner::default();
        owner.protect_sqlite_writer(writer, Some(producer.drain()));
        drop(producer);
        let super::StorageCleanup { work, claim } = owner.cleanup.pop().unwrap();
        drop(owner);
        let receiver = tokio::spawn(async move {
            match requests.recv_async().await.unwrap() {
                crate::store::state_machine::sqlite::writer::WriterRequest::Shutdown(ack) => {
                    drop(ack)
                }
                _ => panic!("expected shutdown request"),
            }
        });
        assert!(
            work.await.is_err(),
            "missing shutdown ACK is not a terminal receipt"
        );
        receiver.await.unwrap();
        drop(claim);
        assert!(
            probe.try_lock().is_err(),
            "unknown retirement must retain the actual root lease"
        );
        // Do not unlink a guarded path: custody intentionally lasts for this
        // isolated test process, whose exit closes the retained descriptor.
        drop(probe);
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
                None,
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
