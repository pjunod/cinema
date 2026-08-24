//! Production lease lifecycle for cluster-wide background jobs.
//!
//! This module deliberately depends only on `plurx-core` plus Tokio. The
//! separate-process cluster harness compiles this exact source so takeover
//! tests cannot pass against a test-only heartbeat implementation.

use std::time::Duration;

use plurx_core::cluster::coordination::{
    ClusterJobAuthority, Lease, LeaseClaim, StoreCoordinator,
};
use plurx_core::error::StoreError;
use plurx_core::store::{PublicationFence, PublicationStore, Store};

/// The job authority for a process that has no Raft membership handle.
///
/// The maintenance commands attach to a running voter as a remote client, so
/// they cannot read committed membership and must fall back on this data
/// directory's durable admission record. That record can only be trusted to
/// *refuse*: it says what this host was admitted as, which is enough to keep
/// `plurxd refresh-metadata` on a learner from taking the cluster-wide artwork
/// lease away from the voters, and is never used to grant anything a live
/// check would deny.
pub(crate) struct AdmittedRoleJobAuthority(plurx_core::cluster::membership::ClusterRole);

impl AdmittedRoleJobAuthority {
    pub(crate) fn new(role: plurx_core::cluster::membership::ClusterRole) -> Self {
        Self(role)
    }
}

#[async_trait::async_trait]
impl ClusterJobAuthority for AdmittedRoleJobAuthority {
    async fn may_run_cluster_jobs(&self) -> bool {
        !self.0.is_learner()
    }
}

const JOB_LEASE_TTL: Duration = Duration::from_secs(90);
const JOB_LEASE_HEARTBEAT: Duration = Duration::from_secs(30);
const RELEASE_ATTEMPTS: usize = 5;
const RELEASE_RETRY_DELAY: Duration = Duration::from_millis(100);

pub(crate) struct ActiveJobLease {
    coordinator: StoreCoordinator,
    fence: PublicationFence,
    cancel: tokio_util::sync::CancellationToken,
    lost: tokio_util::sync::CancellationToken,
    heartbeat: Option<tokio::task::JoinHandle<Result<(), StoreError>>>,
}

impl ActiveJobLease {
    fn start(coordinator: StoreCoordinator, lease: Lease) -> Result<Self, StoreError> {
        Self::start_with_policy(coordinator, lease, JOB_LEASE_TTL, JOB_LEASE_HEARTBEAT)
    }

    pub(crate) fn start_with_policy(
        coordinator: StoreCoordinator,
        lease: Lease,
        ttl: Duration,
        heartbeat_every: Duration,
    ) -> Result<Self, StoreError> {
        if heartbeat_every.is_zero() || heartbeat_every >= ttl {
            return Err(StoreError::Task(format!(
                "cluster job heartbeat {heartbeat_every:?} must be positive and shorter than TTL {ttl:?}"
            )));
        }
        let fence = PublicationFence::new(lease);
        let heartbeat_fence = fence.clone();
        let heartbeat_coordinator = coordinator.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let heartbeat_cancel = cancel.clone();
        let lost = tokio_util::sync::CancellationToken::new();
        let heartbeat_lost = lost.clone();
        let heartbeat = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(heartbeat_every);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await;
            loop {
                tokio::select! {
                    _ = heartbeat_cancel.cancelled() => break,
                    _ = ticker.tick() => {}
                }
                let Some(current) = heartbeat_fence.snapshot().await else {
                    break;
                };
                let renewal = heartbeat_fence.renew(&heartbeat_coordinator, ttl);
                tokio::pin!(renewal);
                let expiry = tokio::time::sleep(lease_time_remaining(current.expires_at_unix_ms));
                tokio::pin!(expiry);
                let renewed = tokio::select! {
                    _ = heartbeat_cancel.cancelled() => {
                        // Graceful release must join an already-dispatched
                        // renewal before it samples and retires the final
                        // token; dropping the future can detach a Raft write.
                        renewal.await?;
                        break;
                    }
                    _ = &mut expiry => {
                        // Revocation and loss notification are synchronous,
                        // but the already-dispatched renewal must still be
                        // drained. TimedClient bounds that request; awaiting
                        // it prevents a late Raft write after release returns.
                        heartbeat_fence.revoke();
                        heartbeat_lost.cancel();
                        renewal.await?;
                        let _ = heartbeat_fence.invalidate(&current).await;
                        tracing::warn!(
                            resource = current.resource,
                            fence = current.fence,
                            "cluster job renewal exceeded its lease deadline and self-fenced"
                        );
                        break;
                    }
                    result = &mut renewal => result,
                };
                match renewed {
                    Ok(true) => {}
                    Ok(false) => {
                        heartbeat_lost.cancel();
                        tracing::warn!(
                            resource = current.resource,
                            fence = current.fence,
                            "cluster job lost its lease and self-fenced"
                        );
                        break;
                    }
                    Err(error) => {
                        heartbeat_lost.cancel();
                        tracing::warn!(
                            resource = current.resource,
                            fence = current.fence,
                            error = %error,
                            "cluster job lease renewal failed and self-fenced"
                        );
                        return Err(error);
                    }
                }
            }
            Ok::<(), StoreError>(())
        });
        Ok(Self {
            coordinator,
            fence,
            cancel,
            lost,
            heartbeat: Some(heartbeat),
        })
    }

    pub(crate) fn publisher<'a>(&self, store: &'a dyn Store) -> PublicationStore<'a> {
        PublicationStore::fenced(store, self.fence.clone())
    }

    pub(crate) fn loss_token(&self) -> tokio_util::sync::CancellationToken {
        self.lost.clone()
    }

    pub(crate) async fn release(mut self) -> Result<(), StoreError> {
        self.fence.revoke();
        self.cancel.cancel();
        let mut cleanup_errors = Vec::new();
        if let Some(heartbeat) = self.heartbeat.take() {
            match heartbeat.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(error = %error, "cluster job renewal cleanup was ambiguous");
                    cleanup_errors.push(error.to_string());
                }
                Err(error) => {
                    tracing::warn!(error = %error, "cluster job heartbeat task failed during release");
                    cleanup_errors.push(error.to_string());
                }
            }
        }
        let token = self.fence.snapshot().await;
        if let Some(token) = token {
            let mut attempt = 1;
            let released = loop {
                match self.coordinator.release(&token).await {
                    Ok(value) => break Ok(value),
                    Err(error) if attempt < RELEASE_ATTEMPTS => {
                        tracing::warn!(
                            resource = token.resource,
                            fence = token.fence,
                            attempt,
                            error = %error,
                            "cluster job lease release was transient; retrying"
                        );
                        tokio::time::sleep(RELEASE_RETRY_DELAY).await;
                        attempt += 1;
                    }
                    Err(error) => break Err(error),
                }
            };
            if let Err(error) = released {
                tracing::warn!(
                    resource = token.resource,
                    fence = token.fence,
                    error = %error,
                    "cluster job lease release failed; TTL will recover it"
                );
                cleanup_errors.push(error.to_string());
            }
        }
        if cleanup_errors.is_empty() {
            Ok(())
        } else {
            Err(StoreError::Task(format!(
                "cluster job cleanup was ambiguous: {}",
                cleanup_errors.join("; ")
            )))
        }
    }
}

fn lease_time_remaining(expires_at_unix_ms: i64) -> Duration {
    let now_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(i64::MAX);
    Duration::from_millis(expires_at_unix_ms.saturating_sub(now_unix_ms).max(0) as u64)
}

impl Drop for ActiveJobLease {
    fn drop(&mut self) {
        self.fence.revoke();
        self.cancel.cancel();
        self.lost.cancel();
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
    }
}

/// Acquire one cluster-wide singleton job, if this node is allowed to run it.
///
/// The eligibility question comes first, and it is asked *now* rather than
/// remembered from startup. A learner holds the same shared cluster credential
/// as every voter, so nothing inside the lease itself would stop it winning
/// one — and a learner that won `provider:artwork` would not merely duplicate
/// work, it would take the lease away from the voters that should own it.
///
/// This is also the only gate the whole singleton-job surface passes through:
/// `provider:artwork`, `provider:genres`, `scan:library:{id}`, `repair:probe`,
/// `candidate:pretranscode`, and any future resource all arrive here.
pub(crate) async fn acquire_cluster_job(
    coordinator: &StoreCoordinator,
    authority: &dyn ClusterJobAuthority,
    resource: String,
) -> Result<Option<ActiveJobLease>, StoreError> {
    if !authority.may_run_cluster_jobs().await {
        tracing::debug!(
            resource,
            "declining a cluster job: this node is not a committed voter"
        );
        return Ok(None);
    }
    match coordinator.acquire(&resource, JOB_LEASE_TTL).await? {
        LeaseClaim::Acquired(lease) => ActiveJobLease::start(coordinator.clone(), lease).map(Some),
        LeaseClaim::Held {
            owner_node_id,
            fence,
            expires_at_unix_ms,
        } => {
            tracing::debug!(
                resource,
                owner = owner_node_id,
                fence,
                expires_at_unix_ms,
                "cluster job lease is held; skipping local duplicate"
            );
            Ok(None)
        }
    }
}
