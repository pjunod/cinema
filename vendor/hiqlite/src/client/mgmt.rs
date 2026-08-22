use crate::app_state::AppState;
use crate::client::stream::ClientStreamReq;
use crate::helpers::deserialize;
use crate::network::HEADER_NAME_SECRET;
use crate::{Client, Error};
use openraft::ServerState;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time;
use tracing::{debug, info};

#[cfg(feature = "sqlite")]
pub(crate) const DB_QUORUM_WATERMARK_MARKER: &str =
    "/* hiqlite-internal:db-quorum-watermark:v1 */ THIS IS NOT SQL";

#[cfg(feature = "sqlite")]
pub(crate) const DB_QUORUM_WATERMARK_COMPAT_PROBE: &str =
    "SELECT 1 AS hiqlite_watermark_stream_compat_v1";

#[cfg(feature = "cache")]
use crate::network::management::{self, ClusterLeaveReq};
#[cfg(feature = "sqlite")]
use crate::store::state_machine::sqlite::writer::WriterRequest;
#[cfg(any(feature = "sqlite", feature = "cache"))]
use crate::{Node, NodeId};
#[cfg(any(feature = "sqlite", feature = "cache"))]
use openraft::RaftMetrics;
#[cfg(any(feature = "sqlite", feature = "cache"))]
use std::clone::Clone;
#[cfg(any(feature = "sqlite", feature = "cache"))]
use std::sync::atomic::Ordering;

/// Privacy-safe, local-only projection of the database Raft watch channel.
///
/// The projection deliberately omits node addresses and exposes no method
/// that can issue an HTTP request. Consumers can therefore sample local Raft
/// progress without accidentally falling back to the management API.
#[cfg(feature = "sqlite")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalDbRaftSnapshot {
    pub running: bool,
    pub node_id: u64,
    pub current_term: u64,
    pub current_leader: Option<u64>,
    pub last_applied_term: Option<u64>,
    pub last_applied_index: Option<u64>,
}

/// A leader-issued database commit watermark backed by a quorum heartbeat.
///
/// The term and leader identity describe the leadership proof, not the term
/// that originally appended the committed entry. Callers must bind this tuple
/// to a fresh local Raft observation before using it for a bounded read.
#[cfg(feature = "sqlite")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DbQuorumWatermark {
    pub term: u64,
    pub leader_id: u64,
    pub committed_index: u64,
}

/// Receiver for the in-process database Raft metrics watch channel.
///
/// This type can only be constructed for a [`Client`] backed by a local
/// Hiqlite node. A remote client returns an error instead of silently turning
/// a passive observation into network traffic.
#[cfg(feature = "sqlite")]
#[derive(Clone)]
pub struct LocalDbRaftMetrics {
    receiver: watch::Receiver<RaftMetrics<NodeId, Node>>,
}

#[cfg(feature = "sqlite")]
impl LocalDbRaftMetrics {
    /// Copy the latest in-process Raft observation without Store or network IO.
    #[must_use]
    pub fn snapshot(&self) -> LocalDbRaftSnapshot {
        let metrics = self.receiver.borrow();
        LocalDbRaftSnapshot {
            running: metrics.running_state.is_ok(),
            node_id: metrics.id,
            current_term: metrics.current_term,
            current_leader: metrics.current_leader,
            last_applied_term: metrics
                .last_applied
                .as_ref()
                .map(|log| log.leader_id.term),
            last_applied_index: metrics.last_applied.as_ref().map(|log| log.index),
        }
    }

    /// Wait for a new in-process observation. `false` means the Raft task
    /// closed its watch channel and no future sample can become fresh.
    pub async fn wait_for_change(&mut self) -> bool {
        self.receiver.changed().await.is_ok()
    }
}

#[cfg(feature = "sqlite")]
pub(crate) async fn db_quorum_watermark_local(
    state: &Arc<AppState>,
) -> Result<DbQuorumWatermark, Error> {
    let before = state.raft_db.raft.metrics().borrow().clone();
    before.running_state?;

    let committed = tokio::time::timeout(
        Duration::from_secs(1),
        state.raft_db.raft.ensure_linearizable(),
    )
    .await
    .map_err(|_| Error::Timeout("database quorum watermark proof timed out".into()))??
    .ok_or_else(|| Error::LeaderChange("database leader has no read index".into()))?;
    let after = state.raft_db.raft.metrics().borrow().clone();
    after.running_state?;
    if after.state != ServerState::Leader
        || after.current_term != before.current_term
        || after.current_term != committed.leader_id.term
        || after.current_leader != Some(state.id)
        || committed.leader_id.node_id != state.id
    {
        return Err(Error::LeaderChange(
            "database leadership changed during quorum watermark proof".into(),
        ));
    }

    Ok(DbQuorumWatermark {
        term: after.current_term,
        leader_id: state.id,
        committed_index: committed.index,
    })
}

impl Client {
    /// Subscribe to database Raft metrics only when this client owns the local
    /// node. Remote clients return an error; this method never performs IO.
    #[cfg(feature = "sqlite")]
    #[must_use]
    pub fn local_db_raft_metrics(&self) -> Result<LocalDbRaftMetrics, Error> {
        let state = self.inner.state.as_ref().ok_or_else(|| {
            Error::Connect("local database Raft metrics require a local node client".to_owned())
        })?;
        Ok(LocalDbRaftMetrics {
            receiver: state.raft_db.raft.metrics(),
        })
    }

    /// Obtain a commit watermark after the database leader has confirmed its
    /// current term with a quorum and applied through the returned read index.
    ///
    /// A follower forwards this narrow request over Hiqlite's authenticated
    /// leader stream. The method performs no SQL or state-machine mutation.
    #[cfg(feature = "sqlite")]
    pub async fn db_quorum_watermark(&self) -> Result<DbQuorumWatermark, Error> {
        match self.db_quorum_watermark_req().await {
            Ok(watermark) => Ok(watermark),
            Err(error) => {
                if self
                    .was_leader_update_error(
                        &error,
                        &self.inner.leader_db,
                        &self.inner.tx_client_db,
                    )
                    .await
                {
                    self.db_quorum_watermark_req().await
                } else {
                    Err(error)
                }
            }
        }
    }

    #[cfg(feature = "sqlite")]
    async fn db_quorum_watermark_req(&self) -> Result<DbQuorumWatermark, Error> {
        if let Some(state) = self.is_leader_db_with_state().await {
            db_quorum_watermark_local(state).await
        } else {
            let mut rows = self
                .query_remote_req(
                    crate::store::state_machine::sqlite::state_machine::Query {
                        sql: DB_QUORUM_WATERMARK_MARKER.into(),
                        params: Vec::new(),
                    },
                    true,
                )
                .await?;
            if rows.len() != 1 {
                return Err(Error::Connect(
                    "database quorum watermark returned an invalid response".into(),
                ));
            }
            rows.swap_remove(0).into_db_quorum_watermark()
        }
    }

    /// Get cluster metrics for the database Raft.
    #[cfg(feature = "sqlite")]
    pub async fn metrics_db(&self) -> Result<RaftMetrics<NodeId, Node>, Error> {
        if let Some(state) = &self.inner.state {
            let metrics = state.raft_db.raft.metrics().borrow().clone();
            Ok(metrics)
        } else {
            let url = self
                .build_addr("/cluster/metrics/sqlite", &self.inner.leader_db)
                .await;
            self.get_metrics_remote(url).await
        }
    }

    /// Get cluster metrics for the cache Raft.
    #[cfg(feature = "cache")]
    pub async fn metrics_cache(&self) -> Result<RaftMetrics<NodeId, Node>, Error> {
        if let Some(state) = &self.inner.state {
            let metrics = state.raft_cache.raft.metrics().borrow().clone();
            Ok(metrics)
        } else {
            let url = self
                .build_addr("/cluster/metrics/cache", &self.inner.leader_cache)
                .await;
            self.get_metrics_remote(url).await
        }
    }

    // This is separated from the `self.send_with_retry_db()` to avoid recursion on leader unreachable
    async fn get_metrics_remote(&self, url: String) -> Result<RaftMetrics<NodeId, Node>, Error> {
        // This should never be called if we have a local client with its own replicated data
        debug_assert!(
            self.inner.state.is_none(),
            "get_metrics_remote should never be called with local state"
        );
        debug_assert!(
            self.inner.api_secret.is_some(),
            "api_secret should always exist for remote clients"
        );

        let res = self
            .inner
            .client
            .as_ref()
            .unwrap()
            .get(url)
            .header(HEADER_NAME_SECRET, self.inner.api_secret.as_ref().unwrap())
            .send()
            .await?;

        if res.status().is_success() {
            let bytes = res.bytes().await?;
            let resp = deserialize(bytes.as_ref())?;
            Ok(resp)
        } else {
            let err = res.json::<Error>().await?;
            Err(err)
        }
    }

    /// Check the cluster health state for the database Raft.
    #[cfg(feature = "sqlite")]
    pub async fn is_healthy_db(&self) -> Result<(), Error> {
        let metrics = self.metrics_db().await?;
        metrics.running_state?;
        if metrics.current_leader.is_some() {
            if metrics.state == ServerState::Learner
                || metrics.state == ServerState::Follower
                || metrics.state == ServerState::Leader
            {
                Ok(())
            } else {
                Err(Error::Connect(format!(
                    "The DB leader voting process has not finished yet - server state: {:?}",
                    metrics.state
                )))
            }
        } else {
            // tracing::error!("Unhealthy DB");
            Err(Error::LeaderChange(
                "The DB leader voting process has not finished yet".into(),
            ))
        }
    }

    /// Check the cluster health state for the cache Raft.
    #[cfg(feature = "cache")]
    pub async fn is_healthy_cache(&self) -> Result<(), Error> {
        let metrics = self.metrics_cache().await?;
        metrics.running_state?;
        if metrics.current_leader.is_some() {
            if metrics.state == ServerState::Learner
                || metrics.state == ServerState::Follower
                || metrics.state == ServerState::Leader
            {
                Ok(())
            } else {
                Err(Error::Connect(format!(
                    "The cache leader voting process has not finished yet - server state: {:?}",
                    metrics.state
                )))
            }
        } else {
            // tracing::error!("Unhealthy cache");
            Err(Error::LeaderChange(
                "The cache leader voting process has not finished yet".into(),
            ))
        }
    }

    /// Wait until the database Raft is healthy.
    #[cfg(feature = "sqlite")]
    pub async fn wait_until_healthy_db(&self) {
        loop {
            match self.is_healthy_db().await {
                Ok(_) => {
                    return;
                }
                Err(err) => {
                    debug!("Waiting for healthy Raft DB: {:?}", err);
                    // tracing::warn!("Waiting for healthy Raft DB");
                    info!("Waiting for healthy Raft DB");
                    time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }

    /// Wait until the cache Raft is healthy.
    #[cfg(feature = "cache")]
    pub async fn wait_until_healthy_cache(&self) {
        loop {
            match self.is_healthy_cache().await {
                Ok(_) => {
                    return;
                }
                Err(err) => {
                    debug!("Waiting for healthy Raft cache: {:?}", err);
                    info!("Waiting for healthy Raft cache");
                    time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }

    /// Perform a graceful shutdown for this Raft node.
    /// Works on local clients only and can't shut down remote nodes.
    ///
    /// The shutdown adds a 10 delay on purpose for smoothing out Kubernetes rolling releases and
    /// make the whole process more graceful, because a whole new leader election might be necessary.
    ///
    /// In future versions, there will be the possibility to trigger a graceful leader election
    /// upfront, but this has not been stabilized in this version.
    pub async fn shutdown(&self) -> Result<(), Error> {
        if let Some(state) = &self.inner.state {
            if tokio::time::timeout(
                Duration::from_secs(15),
                Self::shutdown_execute(
                    state,
                    #[cfg(feature = "cache")]
                    self.inner.tls_config.is_some(),
                    #[cfg(feature = "cache")]
                    self.inner.tls_no_verify,
                    #[cfg(feature = "cache")]
                    &self.inner.tx_client_cache,
                    #[cfg(feature = "sqlite")]
                    &self.inner.tx_client_db,
                    &self.inner.tx_shutdown,
                ),
            )
            .await
            .is_err()
            {
                Err(Error::Error(
                    "Timeout reached while shutting down Raft".into(),
                ))
            } else {
                Ok(())
            }
        } else {
            Err(Error::Error(
                "Shutdown for remote Raft clients is not yet implemented".into(),
            ))
        }
    }

    #[allow(unused_assignments)]
    #[allow(unused_variables)]
    pub(crate) async fn shutdown_execute(
        state: &Arc<AppState>,
        #[cfg(feature = "cache")] with_tls: bool,
        #[cfg(feature = "cache")] tls_no_verify: bool,
        #[cfg(feature = "cache")] tx_client_cache: &flume::Sender<ClientStreamReq>,
        #[cfg(feature = "sqlite")] tx_client_db: &flume::Sender<ClientStreamReq>,
        tx_shutdown: &Option<watch::Sender<bool>>,
    ) -> Result<(), Error> {
        info!("Starting Node shutdown");

        #[allow(unused_mut)]
        let mut is_single_instance: bool;
        #[cfg(feature = "cache")]
        {
            let node_count = state
                .raft_cache
                .raft
                .metrics()
                .borrow()
                .membership_config
                .nodes()
                .count();
            is_single_instance = node_count == 1;
        }
        #[cfg(feature = "sqlite")]
        {
            let node_count = state
                .raft_db
                .raft
                .metrics()
                .borrow()
                .membership_config
                .nodes()
                .count();
            is_single_instance = node_count == 1;
        }

        state.is_shutting_down.store(true, Ordering::Relaxed);

        // This pre-shutdown delay is not strictly necessary, but it makes rolling releases
        // smoother, especially with ephemeral storage. It also allows to set a ready check
        // interval of 3 seconds while it will still catch it before it actually starts the
        // shutdown, so services can stop sending requests to this node.
        if !is_single_instance {
            time::sleep(Duration::from_millis(9500)).await;
        }

        #[cfg(feature = "cache")]
        {
            let mut metrics = state.raft_cache.raft.metrics().borrow().clone();

            for _ in 0..5 {
                if metrics.current_leader.is_some() {
                    break;
                }
                info!("Delaying cache cluster leave because of no existing leader");
                time::sleep(Duration::from_secs(1)).await;
                metrics = state.raft_cache.raft.metrics().borrow().clone();
            }

            if !state.raft_cache.cache_storage_disk {
                // If we run an entirely in-memory cache and therefore lose the Raft state
                // and membership between restarts, we should always leave the cluster cleanly
                // before doing a shutdown.
                info!("Leaving in-memory-only cache cluster");

                let client = crate::http_client::build_http_client(tls_no_verify);
                let scheme = if with_tls { "https" } else { "http" };

                if metrics.current_leader == Some(state.id) {
                    if let Err(err) = management::leave_cluster_exec(
                        state,
                        &crate::app_state::RaftType::Cache,
                        ClusterLeaveReq {
                            node_id: state.id,
                            stay_as_learner: false,
                        },
                    )
                    .await
                    {
                        tracing::error!("Error leaving the Cache cluster: {:?}", err);
                    }
                } else if let Err(err) = crate::init::leave_remote_cluster(
                    state,
                    &crate::app_state::RaftType::Cache,
                    &client,
                    scheme,
                    state.id,
                    &state.nodes,
                    0,
                    false,
                )
                .await
                {
                    tracing::error!("Error leaving the Cache cluster: {:?}", err);
                }

                info!("Left in-memory-only cache cluster successfully");
            }

            info!("Shutting down raft cache layer");

            // TODO as soon openraft-0.10 is out, we will be able to trigger a pre-emptive
            //  leader switch, if this node is the leader. This will smoth out things even more.

            state
                .raft_cache
                .is_raft_stopped
                .store(true, Ordering::Relaxed);
            state.raft_cache.raft.shutdown().await?;
            if let Some(handle) = &state.raft_cache.shutdown_handle {
                handle.shutdown().await?;
            }
            let _ = tx_client_cache.send_async(ClientStreamReq::Shutdown).await;
        };

        #[cfg(feature = "sqlite")]
        {
            info!("Shutting down raft sqlite layer");

            for _ in 0..5 {
                if state
                    .raft_db
                    .raft
                    .metrics()
                    .borrow()
                    .current_leader
                    .is_some()
                {
                    break;
                }
                info!("Delaying sqlite raft shutdown because of no existing leader");
                time::sleep(Duration::from_secs(1)).await;
            }

            // TODO as soon openraft-0.10 is out, we will be able to trigger a pre-emptive
            //  leader switch, if this node is the leader. This will smoth out things even more.

            state.raft_db.is_raft_stopped.store(true, Ordering::Relaxed);

            state.raft_db.raft.shutdown().await?;
            info!("Shutting down sqlite logs writer");
            state.raft_db.shutdown_handle.shutdown().await?;

            info!("Shutting down sqlite writer");
            let (tx_sm, rx_sm) = tokio::sync::oneshot::channel();
            state
                .raft_db
                .sql_writer
                .send_async(WriterRequest::Shutdown(tx_sm))
                .await
                .expect("The state machine writer to always be listening");
            rx_sm
                .await
                .expect("To always get an answer from SQL writer");

            let _ = tx_client_db.send_async(ClientStreamReq::Shutdown).await;
        }

        if let Some(tx) = tx_shutdown {
            tx.send(true)
                .expect("The global Hiqlite shutdown handler to always listen");
        }

        info!("Shutdown complete");
        Ok(())
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    #[test]
    fn local_watch_accessor_has_no_remote_fallback() {
        let source = include_str!("mgmt.rs");
        let accessor = source
            .split_once("pub fn local_db_raft_metrics")
            .expect("local watch accessor")
            .1
            .split_once("/// Obtain a commit watermark")
            .expect("end of local watch accessor")
            .0;
        assert!(accessor.contains("self.inner.state.as_ref().ok_or_else"));
        assert!(!accessor.contains("build_addr"));
        assert!(!accessor.contains("get_metrics_remote"));
        assert!(!accessor.contains(".await"));
    }

    #[test]
    fn quorum_watermark_reuses_the_existing_consistent_query_wire_variant() {
        let source = include_str!("mgmt.rs");
        let accessor = source
            .split_once("async fn db_quorum_watermark_req")
            .expect("quorum watermark request")
            .1
            .split_once("/// Get cluster metrics for the database Raft.")
            .expect("end of quorum watermark request")
            .0;
        assert!(accessor.contains("query_remote_req"));
        assert!(accessor.contains("DB_QUORUM_WATERMARK_MARKER"));
        assert!(!accessor.contains("ApiStreamRequestPayload"));

        let stream = include_str!("../network/api.rs");
        assert!(stream.contains("ApiStreamRequestPayload::QueryConsistent"));
        assert!(!stream.contains("ApiStreamRequestPayload::QuorumWatermark"));
    }

    #[test]
    fn quorum_watermark_row_round_trips_full_u64_values() {
        let expected = super::DbQuorumWatermark {
            term: u64::MAX,
            leader_id: u64::MAX - 1,
            committed_index: u64::MAX - 2,
        };
        let actual = crate::query::rows::RowOwned::from_db_quorum_watermark(expected)
            .into_db_quorum_watermark()
            .expect("watermark row");
        assert_eq!(actual, expected);
    }
}
