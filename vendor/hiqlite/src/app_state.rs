use crate::NodeId;
use chrono::Utc;
use serde::Deserialize;
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::Mutex;

#[cfg(feature = "dashboard")]
use crate::client::stream::ClientStreamControl;
#[cfg(any(feature = "backup", feature = "dashboard"))]
use crate::client::stream::ClientStreamReq;
#[cfg(feature = "dashboard")]
use crate::dashboard::DashboardState;
#[cfg(feature = "s3")]
use crate::s3::S3Config;
#[cfg(feature = "dlock")]
use crate::store::state_machine::memory::dlock_handler::LockRequest;
#[cfg(feature = "listen_notify")]
use crate::store::state_machine::memory::notify_handler::NotifyRequest;
#[cfg(feature = "cache")]
use crate::store::state_machine::memory::{TypeConfigKV, kv_handler::CacheRequestHandler};
#[cfg(feature = "sqlite")]
use crate::store::state_machine::sqlite::{
    TypeConfigSqlite, state_machine::SqlitePool, writer::WriterRequest,
};
#[cfg(any(feature = "backup", feature = "dashboard"))]
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RaftType {
    #[cfg(feature = "sqlite")]
    Sqlite,
    #[cfg(feature = "cache")]
    Cache,
    Unknown,
}

impl RaftType {
    pub fn as_str(&self) -> &str {
        match self {
            #[cfg(feature = "sqlite")]
            RaftType::Sqlite => "sqlite",
            #[cfg(feature = "cache")]
            RaftType::Cache => "cache",
            RaftType::Unknown => "unknown",
        }
    }
}

// Representation of an application state. This struct can be shared around to share
// instances of raft, store and more.
pub(crate) struct AppState {
    pub app_start: chrono::DateTime<Utc>,
    pub is_shutting_down: AtomicBool,
    #[cfg(feature = "backup")]
    pub backups_dir: String,
    pub id: NodeId,
    #[cfg(feature = "cache")]
    pub nodes: Vec<crate::Node>,
    pub addr_api: String,
    pub snapshot_transport: crate::LocalSnapshotTransportStatus,
    #[cfg(feature = "sqlite")]
    pub raft_db: StateRaftDB,
    #[cfg(feature = "cache")]
    pub raft_cache: StateRaftCache,
    pub raft_lock: Arc<Mutex<()>>,
    pub membership_admission: Option<Arc<dyn crate::membership_admission::MembershipAdmission>>,
    #[cfg(feature = "backup")]
    pub startup_backup: std::sync::Mutex<Option<crate::backup::BackupConfig>>,
    #[cfg(feature = "s3")]
    pub s3_config: Option<Arc<S3Config>>,
    pub secret_raft: String,
    pub secret_api: String,
    #[cfg(feature = "dashboard")]
    pub dashboard: DashboardState,
    #[cfg(any(feature = "backup", feature = "dashboard"))]
    pub client_request_id: AtomicUsize,
    #[cfg(any(feature = "backup", feature = "dashboard"))]
    pub tx_client_stream: flume::Sender<ClientStreamReq>,
    #[cfg(feature = "dashboard")]
    pub tx_client_control: flume::Sender<ClientStreamControl>,
    pub health_check_delay_secs: u32,
    pub learner_only: bool,
}

#[cfg(any(feature = "backup", feature = "dashboard"))]
impl AppState {
    #[inline(always)]
    pub fn new_request_id(&self) -> usize {
        self.client_request_id.fetch_add(1, Ordering::Relaxed)
    }
}

#[cfg(feature = "sqlite")]
pub struct StateRaftDB {
    pub raft: openraft::Raft<TypeConfigSqlite>,
    pub(crate) snapshot_executor:
        Arc<crate::network::snapshot_executor::SnapshotExecutor<TypeConfigSqlite>>,
    pub shutdown_handle: hiqlite_wal::ShutdownHandle,
    pub wal_status: hiqlite_wal::WalStatusHandle,
    pub sql_writer: flume::Sender<WriterRequest>,
    pub read_pool: SqlitePool,
    /// Plurx patch 27: shared with the writer and the state machine.
    pub(crate) applied_mark: Arc<crate::applied_mark::AppliedMark>,
    pub log_statements: bool,
    pub is_raft_stopped: Arc<AtomicBool>,
    pub is_startup_finished: Arc<AtomicBool>,
    /// Plurx patch 30: the one gate every proposal on this Raft passes.
    pub(crate) proposals: crate::proposal_gate::ProposalGate,
}

/// Plurx patch 30: the only places this node proposes to its database Raft.
/// Each takes a shared proposal permit for the whole call, so a
/// [`crate::ProposalHold`] stops this node's log from growing.
#[cfg(feature = "sqlite")]
impl StateRaftDB {
    pub(crate) async fn client_write(
        &self,
        request: crate::store::state_machine::sqlite::state_machine::QueryWrite,
    ) -> Result<
        openraft::raft::ClientWriteResponse<TypeConfigSqlite>,
        openraft::error::RaftError<NodeId, openraft::error::ClientWriteError<NodeId, crate::Node>>,
    > {
        let _permit = self.proposals.permit().await;
        self.raft.client_write(request).await
    }

    pub(crate) async fn change_membership(
        &self,
        members: impl Into<openraft::ChangeMembers<NodeId, crate::Node>>,
        retain: bool,
    ) -> Result<
        openraft::raft::ClientWriteResponse<TypeConfigSqlite>,
        openraft::error::RaftError<NodeId, openraft::error::ClientWriteError<NodeId, crate::Node>>,
    > {
        let _permit = self.proposals.permit().await;
        self.raft.change_membership(members, retain).await
    }

    pub(crate) async fn add_learner(
        &self,
        id: NodeId,
        node: crate::Node,
        blocking: bool,
    ) -> Result<
        openraft::raft::ClientWriteResponse<TypeConfigSqlite>,
        openraft::error::RaftError<NodeId, openraft::error::ClientWriteError<NodeId, crate::Node>>,
    > {
        let _permit = self.proposals.permit().await;
        self.raft.add_learner(id, node, blocking).await
    }
}

#[cfg(feature = "cache")]
pub struct StateRaftCache {
    pub raft: openraft::Raft<TypeConfigKV>,
    pub(crate) snapshot_executor:
        Arc<crate::network::snapshot_executor::SnapshotExecutor<TypeConfigKV>>,
    pub tx_caches: Vec<flume::Sender<CacheRequestHandler>>,
    #[cfg(feature = "listen_notify")]
    pub tx_notify: flume::Sender<NotifyRequest>,
    #[cfg(feature = "listen_notify_local")]
    pub rx_notify: flume::Receiver<(i64, Vec<u8>)>,
    #[cfg(feature = "dlock")]
    pub tx_dlock: flume::Sender<LockRequest>,
    pub is_raft_stopped: Arc<AtomicBool>,
    pub is_startup_finished: Arc<AtomicBool>,
    pub shutdown_handle: Option<hiqlite_wal::ShutdownHandle>,
    #[cfg(feature = "cache")]
    pub cache_storage_disk: bool,
}
