#![allow(unused)]

use crate::app_state::{AppState, RaftType};
use crate::network::NetworkStreaming;
use crate::{CacheVariants, Error, NodeConfig, NodeId, RaftConfig, init};
use hiqlite_wal::LogSync;
use openraft::storage::RaftLogStorage;
use openraft::{Raft, StorageError};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cmp::PartialEq;
use std::fmt::Debug;
use std::mem;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::time;
use tracing::info;

#[cfg(feature = "cache")]
use crate::{
    app_state::StateRaftCache,
    store::state_machine::memory::{TypeConfigKV, state_machine::StateMachineMemory},
};
#[cfg(feature = "sqlite")]
use crate::{
    app_state::StateRaftDB,
    store::state_machine::sqlite::{
        TypeConfigSqlite,
        state_machine::{SqlitePool, StateMachineSqlite},
        writer::WriterRequest,
    },
};

pub mod logs;
pub mod state_machine;

pub type StorageResult<T> = Result<T, StorageError<NodeId>>;

#[cfg(feature = "sqlite")]
pub(crate) async fn start_raft_db(
    node_config: &NodeConfig,
    raft_config: Arc<RaftConfig>,
    do_reset_metadata: bool,
    snapshot_transport: crate::LocalSnapshotTransportStatus,
    staged_startup: bool,
) -> Result<StateRaftDB, Error> {
    // We always want to start stopped and set to `false` as soon as we found out,
    // that we are not pristine node and need cleanup.
    let is_raft_stopped = Arc::new(AtomicBool::new(true));

    let mut startup_owner = crate::startup_cleanup::StartupStorageOwner::new(staged_startup);
    let log_store = crate::startup_cleanup::start_wal::<TypeConfigSqlite>(
        logs::logs_dir_db(&node_config.data_dir),
        node_config.wal_sync.clone(),
        node_config.wal_size,
        staged_startup,
    )
    .await?;
    startup_owner.protect_wal(log_store.shutdown_handle());
    let wal_status = log_store.status_handle();
    let state_machine_store = if staged_startup {
        let data_dir = node_config.data_dir.clone();
        let filename = node_config.filename_db.clone();
        let node_id = node_config.node_id;
        let log_statements = node_config.log_statements;
        let statement_capacity = node_config.prepared_statement_cache_capacity;
        let read_pool_size = node_config.read_pool_size;
        let snapshot_deferral = node_config.snapshot_storage_deferral;
        #[cfg(feature = "s3")]
        let s3_config = node_config.s3_config.clone();
        #[cfg(feature = "backup")]
        let backup_keep_days = node_config.backup_keep_days_local;
        crate::startup_cleanup::construct_sqlite(async move {
            StateMachineSqlite::new(
                &data_dir,
                &filename,
                node_id,
                log_statements,
                statement_capacity,
                read_pool_size,
                snapshot_deferral,
                #[cfg(feature = "s3")]
                s3_config,
                do_reset_metadata,
                #[cfg(feature = "backup")]
                backup_keep_days,
                true,
            )
            .await
            .map_err(|error| Error::Error(format!("SQLite startup constructor: {error}").into()))
        })
        .await?
    } else {
        StateMachineSqlite::new(
            &node_config.data_dir,
            &node_config.filename_db,
            node_config.node_id,
            node_config.log_statements,
            node_config.prepared_statement_cache_capacity,
            node_config.read_pool_size,
            node_config.snapshot_storage_deferral,
            #[cfg(feature = "s3")]
            node_config.s3_config.clone(),
            do_reset_metadata,
            #[cfg(feature = "backup")]
            node_config.backup_keep_days_local,
            false,
        )
        .await
        .unwrap()
    };

    let is_startup_finished = Arc::new(AtomicBool::new(false));
    let sql_writer = state_machine_store.write_tx.clone();
    startup_owner.protect_writer(sql_writer.clone());
    let read_pool = state_machine_store.read_pool.clone();

    let network = NetworkStreaming {
        node_id: node_config.node_id,
        tls_config: node_config.tls_raft.as_ref().map(|tls| tls.client_config()),
        secret_raft: node_config.secret_raft.as_bytes().to_vec(),
        raft_type: RaftType::Sqlite,
        heartbeat_interval: node_config.raft_config.heartbeat_interval,
        is_raft_stopped: is_raft_stopped.clone(),
        is_startup_finished: is_startup_finished.clone(),
        snapshot_budgets: crate::network::raft_client::SnapshotRpcBudgets::from_node_config(
            node_config,
        ),
        snapshot_transport: snapshot_transport.clone(),
    };

    let shutdown_handle = log_store.shutdown_handle();

    let raft = openraft::Raft::new(
        node_config.node_id,
        raft_config.clone(),
        network,
        log_store,
        state_machine_store,
    )
    .await
    .expect("Raft create failed");
    snapshot_transport.bind_sqlite_membership(raft.metrics());
    let snapshot_executor = crate::network::snapshot_executor::start_snapshot_executor(
        raft.clone(),
        node_config.snapshot_chunk_timeout,
        Duration::from_millis(node_config.raft_config.install_snapshot_timeout),
        "sqlite",
        snapshot_transport,
    );

    let state = StateRaftDB {
        raft,
        snapshot_executor: Arc::new(snapshot_executor),
        shutdown_handle,
        wal_status,
        sql_writer,
        read_pool,
        log_statements: node_config.log_statements,
        is_raft_stopped,
        is_startup_finished,
    };
    startup_owner.handoff();
    startup_owner.protect_db(&state);
    init::init_pristine_node_1_db(
        &state.raft,
        node_config.node_id,
        &node_config.nodes,
        &node_config.secret_api,
        node_config.tls_api.is_some(),
        node_config
            .tls_api
            .as_ref()
            .map(|c| c.danger_tls_no_verify())
            .unwrap_or(false),
    )
    .await?;

    startup_owner.handoff();
    Ok(state)
}

#[cfg(feature = "cache")]
pub(crate) async fn start_raft_cache<C>(
    node_config: &NodeConfig,
    raft_config: Arc<RaftConfig>,
    snapshot_transport: crate::LocalSnapshotTransportStatus,
    staged_startup: bool,
) -> Result<StateRaftCache, Error>
where
    C: Debug + CacheVariants,
{
    // TODO add a check here and only start the cache layer, if the given Idx Enum is NOT empty

    // We always want to start stopped and set to `false` as soon as we found out,
    // that we are not pristine node and need cleanup.
    let is_raft_stopped = Arc::new(AtomicBool::new(true));
    let is_startup_finished = Arc::new(AtomicBool::new(false));
    let mut startup_owner = crate::startup_cleanup::StartupStorageOwner::new(staged_startup);

    let state_machine_store = Arc::new(if staged_startup {
        let data_dir = node_config.data_dir.clone();
        let in_memory_only = !node_config.cache_storage_disk;
        crate::startup_cleanup::construct_cache(async move {
            StateMachineMemory::new::<C>(&data_dir, in_memory_only).await
        })
        .await?
    } else {
        StateMachineMemory::new::<C>(&node_config.data_dir, !node_config.cache_storage_disk).await?
    });
    let network = NetworkStreaming {
        node_id: node_config.node_id,
        tls_config: node_config.tls_raft.as_ref().map(|tls| tls.client_config()),
        secret_raft: node_config.secret_raft.as_bytes().to_vec(),
        raft_type: RaftType::Cache,
        heartbeat_interval: node_config.raft_config.heartbeat_interval,
        is_startup_finished: is_startup_finished.clone(),
        is_raft_stopped: is_raft_stopped.clone(),
        snapshot_budgets: crate::network::raft_client::SnapshotRpcBudgets::from_node_config(
            node_config,
        ),
        snapshot_transport: snapshot_transport.clone(),
    };

    let tx_caches = state_machine_store.tx_caches.clone();
    #[cfg(feature = "listen_notify")]
    let tx_notify = state_machine_store.tx_notify.clone();
    #[cfg(feature = "listen_notify_local")]
    let rx_notify = state_machine_store.rx_notify.clone();

    #[cfg(feature = "dlock")]
    let tx_dlock = state_machine_store.tx_dlock.clone();

    let (raft, shutdown_handle) = if node_config.cache_storage_disk {
        let log_store = crate::startup_cleanup::start_wal::<TypeConfigKV>(
            logs::logs_dir_cache(&node_config.data_dir),
            node_config.wal_sync.clone(),
            node_config.wal_size,
            staged_startup,
        )
        .await?;
        let shutdown_handle = log_store.shutdown_handle();
        startup_owner.protect_wal(shutdown_handle.clone());

        let raft = openraft::Raft::new(
            node_config.node_id,
            raft_config.clone(),
            network,
            log_store,
            state_machine_store,
        )
        .await
        .expect("Raft create failed");

        (raft, Some(shutdown_handle))
    } else {
        let raft = openraft::Raft::new(
            node_config.node_id,
            raft_config.clone(),
            network,
            logs::memory::LogStoreMemory::new(),
            state_machine_store,
        )
        .await
        .expect("Raft create failed");

        (raft, None)
    };
    let snapshot_executor = crate::network::snapshot_executor::start_snapshot_executor(
        raft.clone(),
        node_config.snapshot_chunk_timeout,
        Duration::from_millis(node_config.raft_config.install_snapshot_timeout),
        "cache",
        snapshot_transport,
    );

    let state = StateRaftCache {
        raft,
        snapshot_executor: Arc::new(snapshot_executor),
        tx_caches,
        #[cfg(feature = "listen_notify")]
        tx_notify,
        #[cfg(feature = "listen_notify_local")]
        rx_notify,
        #[cfg(feature = "dlock")]
        tx_dlock,
        is_raft_stopped,
        is_startup_finished,
        shutdown_handle,
        cache_storage_disk: node_config.cache_storage_disk,
    };
    startup_owner.handoff();
    startup_owner.protect_cache(&state);
    init::init_pristine_node_1_cache(
        &state.raft,
        node_config.cache_storage_disk,
        node_config.node_id,
        &node_config.nodes,
        &node_config.secret_api,
        node_config.tls_api.is_some(),
        node_config
            .tls_api
            .as_ref()
            .map(|c| c.danger_tls_no_verify())
            .unwrap_or(false),
    )
    .await?;

    startup_owner.handoff();
    Ok(state)
}
