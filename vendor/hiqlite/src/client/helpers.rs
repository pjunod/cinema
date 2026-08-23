use crate::app_state::AppState;
use crate::client::stream::ClientStreamReq;
use crate::{Client, Error, Node, NodeId};
use openraft::RaftMetrics;
use std::clone::Clone;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time;
use tracing::{debug, error, warn};

const LEADER_RETRY_RECOVERY_TIMEOUT: Duration = Duration::from_secs(2);

impl Client {
    #[inline(always)]
    pub(crate) async fn build_addr(
        &self,
        path: &str,
        leader: &Arc<RwLock<(NodeId, String)>>,
    ) -> String {
        let scheme = if self.inner.tls_config.is_some() {
            "https"
        } else {
            "http"
        };
        let url = {
            let lock = leader.read().await;
            format!("{}://{}{}", scheme, lock.1, path)
        };
        debug!("request url: {}", url);
        url
    }

    pub(crate) async fn find_set_active_leader(&self) {
        if self.inner.proxy_mode {
            return;
        }
        if let Some(state) = &self.inner.state {
            // we never need to do any remote lookups for metrics -> get can never fail
            #[cfg(feature = "sqlite")]
            {
                let metrics = state.raft_db.raft.metrics().borrow().clone();
                let mut find_leader = Self::find_set_leader(metrics, &self.inner.leader_db).await;

                while let Err(err) = find_leader {
                    warn!("Find DB leader error: {}", err);
                    time::sleep(Duration::from_millis(500)).await;
                    let metrics = state.raft_db.raft.metrics().borrow().clone();
                    find_leader = Self::find_set_leader(metrics, &self.inner.leader_db).await;
                }
            }

            #[cfg(feature = "cache")]
            {
                let metrics = state.raft_cache.raft.metrics().borrow().clone();
                let mut find_leader =
                    Self::find_set_leader(metrics, &self.inner.leader_cache).await;

                while let Err(err) = find_leader {
                    warn!("Find cache leader error: {}", err);
                    time::sleep(Duration::from_millis(500)).await;
                    let metrics = state.raft_cache.raft.metrics().borrow().clone();
                    find_leader = Self::find_set_leader(metrics, &self.inner.leader_cache).await;
                }
            }
        } else {
            // in this case, we have a remote client
            #[cfg(feature = "sqlite")]
            {
                let mut metrics = self.remote_metrics_loop_db().await;
                loop {
                    match Self::find_set_leader(metrics, &self.inner.leader_db).await {
                        Ok(_) => {
                            break;
                        }
                        Err(_) => {
                            metrics = self.remote_metrics_loop_db().await;
                        }
                    }
                }
            }

            #[cfg(feature = "cache")]
            {
                let mut metrics = self.remote_metrics_loop_cache().await;
                loop {
                    match Self::find_set_leader(metrics, &self.inner.leader_cache).await {
                        Ok(_) => {
                            break;
                        }
                        Err(_) => {
                            metrics = self.remote_metrics_loop_cache().await;
                        }
                    }
                }
            }
        }
    }

    #[cfg(feature = "cache")]
    async fn remote_metrics_loop_cache(&self) -> RaftMetrics<NodeId, Node> {
        loop {
            for addr in &self.inner.nodes {
                {
                    let mut lock = self.inner.leader_cache.write().await;
                    *lock = (lock.0, addr.clone());
                }

                match self.metrics_cache().await {
                    Ok(metrics) => {
                        return metrics;
                    }
                    Err(err) => {
                        error!("Error looking up Cache metrics: {}", err);
                    }
                }
            }
            time::sleep(Duration::from_millis(500)).await;
        }
    }

    #[cfg(feature = "sqlite")]
    async fn remote_metrics_loop_db(&self) -> RaftMetrics<NodeId, Node> {
        loop {
            for addr in &self.inner.nodes {
                {
                    let mut lock = self.inner.leader_db.write().await;
                    *lock = (lock.0, addr.clone());
                }

                match self.metrics_db().await {
                    Ok(metrics) => {
                        return metrics;
                    }
                    Err(err) => {
                        error!("Error looking up DB metrics: {}", err);
                    }
                }
            }
            time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn find_set_leader(
        metrics: RaftMetrics<NodeId, Node>,
        leader: &Arc<RwLock<(NodeId, String)>>,
    ) -> Result<(), Error> {
        let (leader_id, node) = Self::leader_from_metrics(metrics)?;

        let mut lock = leader.write().await;
        *lock = (leader_id, node.addr_api);

        Ok(())
    }

    fn leader_from_metrics(metrics: RaftMetrics<NodeId, Node>) -> Result<(NodeId, Node), Error> {
        let leader_id = metrics
            .current_leader
            .ok_or_else(|| Error::Connect("Leader vote is in progress".to_string()))?;
        let mut leaders = metrics
            .membership_config
            .nodes()
            .filter(|(id, _)| **id == leader_id);
        let (_, node) = leaders.next().ok_or_else(|| {
            Error::Config("Raft leader is absent from the authenticated membership".into())
        })?;
        if leaders.next().is_some() {
            return Err(Error::Config(
                "Raft leader is duplicated in the authenticated membership".into(),
            ));
        }
        Ok((leader_id, node.clone()))
    }

    async fn discover_active_leader(
        &self,
        leader: &Arc<RwLock<(NodeId, String)>>,
    ) -> Result<(NodeId, Node), Error> {
        loop {
            if let Some(state) = &self.inner.state {
                #[cfg(feature = "sqlite")]
                if Arc::ptr_eq(leader, &self.inner.leader_db) {
                    let metrics = state.raft_db.raft.metrics().borrow().clone();
                    match Self::leader_from_metrics(metrics) {
                        Ok(found) => return Ok(found),
                        Err(err) => warn!("Find DB leader error: {}", err),
                    }
                }

                #[cfg(feature = "cache")]
                if Arc::ptr_eq(leader, &self.inner.leader_cache) {
                    let metrics = state.raft_cache.raft.metrics().borrow().clone();
                    match Self::leader_from_metrics(metrics) {
                        Ok(found) => return Ok(found),
                        Err(err) => warn!("Find cache leader error: {}", err),
                    }
                }
            } else {
                for addr in &self.inner.nodes {
                    let scheme = if self.inner.tls_config.is_some() {
                        "https"
                    } else {
                        "http"
                    };

                    #[cfg(feature = "sqlite")]
                    if Arc::ptr_eq(leader, &self.inner.leader_db) {
                        let url = format!("{scheme}://{addr}/cluster/metrics/sqlite");
                        match self.get_metrics_remote(url).await {
                            Ok(metrics) => match Self::leader_from_metrics(metrics) {
                                Ok(found) => return Ok(found),
                                Err(err) => warn!("Find DB leader error: {}", err),
                            },
                            Err(err) => error!("Error looking up DB metrics: {}", err),
                        }
                    }

                    #[cfg(feature = "cache")]
                    if Arc::ptr_eq(leader, &self.inner.leader_cache) {
                        let url = format!("{scheme}://{addr}/cluster/metrics/cache");
                        match self.get_metrics_remote(url).await {
                            Ok(metrics) => match Self::leader_from_metrics(metrics) {
                                Ok(found) => return Ok(found),
                                Err(err) => warn!("Find cache leader error: {}", err),
                            },
                            Err(err) => error!("Error looking up Cache metrics: {}", err),
                        }
                    }
                }
            }
            time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Check if this instance is the current Raft cluster leader for the database.
    #[cfg(feature = "sqlite")]
    pub async fn is_leader_db(&self) -> bool {
        if let Some(state) = &self.inner.state
            && state.id == self.inner.leader_db.read().await.0
        {
            return true;
        }
        false
    }

    /// Check if this instance is the current Raft cluster leader for the cache.
    #[cfg(feature = "cache")]
    pub async fn is_leader_cache(&self) -> bool {
        if let Some(state) = &self.inner.state
            && state.id == self.inner.leader_cache.read().await.0
        {
            return true;
        }
        false
    }

    #[cfg(feature = "sqlite")]
    #[inline(always)]
    pub(crate) async fn is_leader_db_with_state(&self) -> Option<&Arc<AppState>> {
        if let Some(state) = &self.inner.state
            && state.id == self.inner.leader_db.read().await.0
        {
            return Some(state);
        }
        None
    }

    #[cfg(feature = "cache")]
    #[inline(always)]
    pub(crate) async fn is_leader_cache_with_state(&self) -> Option<&Arc<AppState>> {
        if let Some(state) = &self.inner.state
            && state.id == self.inner.leader_cache.read().await.0
        {
            return Some(state);
        }
        None
    }

    #[cfg(not(feature = "dashboard"))]
    #[inline(always)]
    pub(crate) fn new_request_id(&self) -> usize {
        self.inner.request_id.fetch_add(1, Ordering::Relaxed)
    }

    #[cfg(feature = "dashboard")]
    #[inline(always)]
    pub(crate) fn new_request_id(&self) -> usize {
        if let Some(st) = &self.inner.state {
            st.new_request_id()
        } else {
            self.inner.request_id.fetch_add(1, Ordering::Relaxed)
        }
    }

    #[inline]
    pub(crate) async fn was_leader_update_error(
        &self,
        err: &Error,
        lock: &Arc<RwLock<(NodeId, String)>>,
        tx: &flume::Sender<ClientStreamReq>,
    ) -> bool {
        let Some((leader_id, node)) = err.is_forward_to_leader() else {
            return false;
        };

        if self.inner.proxy_mode {
            // Reconnect through the same proxy endpoint. The proxy owns leader
            // discovery; accepting an advertised node (or trying to discover
            // a leader when the responder has none) would silently escape the
            // caller's network and trust boundary.
            tx.send_async(ClientStreamReq::LeaderChange((None, None)))
                .await
                .expect("the Client API WebSocket Manager to always be running");
            return true;
        }

        if let (Some(leader_id), Some(node)) = (leader_id, node.clone()) {
            let api_addr = node.addr_api.clone();
            {
                let mut lock = lock.write().await;
                // we check additionally to prevent race conditions and multiple
                // re-connect triggers
                if lock.0 != leader_id {
                    *lock = (leader_id, api_addr.clone());
                }
            }
            tx.send_async(ClientStreamReq::LeaderChange((Some(leader_id), Some(node))))
                .await
                .expect("the Client API WebSocket Manager to always be running");
        } else {
            // A resumed follower can reject a write before it has learned the
            // new leader, yielding ForwardToLeader(None, None). That response
            // is definitive evidence the write was not accepted. Recover in a
            // detached, explicitly bounded task so raw Client callers cannot
            // hang forever and cancellation by a higher-level timeout cannot
            // interrupt recovery. Discovery is side-effect-free; only the
            // stream manager atomically applies the authenticated result.
            let client = self.clone();
            let leader = Arc::clone(lock);
            let tx = tx.clone();
            let recovered = tokio::spawn(async move {
                time::timeout(LEADER_RETRY_RECOVERY_TIMEOUT, async move {
                    let (leader_id, node) = client.discover_active_leader(&leader).await?;
                    tx.send_async(ClientStreamReq::LeaderChange((Some(leader_id), Some(node))))
                        .await
                        .expect("the Client API WebSocket Manager to always be running");
                    Ok::<(), Error>(())
                })
                .await
                .is_ok_and(|result| result.is_ok())
            })
            .await
            .unwrap_or(false);
            if !recovered {
                return false;
            }
        }

        true
    }
}
