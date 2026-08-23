//! Production lease lifecycle for cluster-wide background jobs.
//!
//! This module deliberately depends only on `plurx-core` plus Tokio. The
//! separate-process cluster harness compiles this exact source so takeover
//! tests cannot pass against a test-only heartbeat implementation.

use std::time::Duration;

use plurx_core::cluster::coordination::{Lease, LeaseClaim, StoreCoordinator};
use plurx_core::error::StoreError;
use plurx_core::store::{PublicationFence, PublicationStore, Store};

const JOB_LEASE_TTL: Duration = Duration::from_secs(90);
const JOB_LEASE_HEARTBEAT: Duration = Duration::from_secs(30);

pub(crate) struct ActiveJobLease {
    coordinator: StoreCoordinator,
    fence: PublicationFence,
    cancel: tokio_util::sync::CancellationToken,
    lost: tokio_util::sync::CancellationToken,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
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
                    _ = heartbeat_cancel.cancelled() => break,
                    _ = &mut expiry => {
                        let _ = heartbeat_fence.invalidate(&current).await;
                        heartbeat_lost.cancel();
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
                        break;
                    }
                }
            }
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

    pub(crate) fn publication_fence(&self) -> PublicationFence {
        self.fence.clone()
    }

    pub(crate) async fn release(mut self) {
        self.cancel.cancel();
        if let Some(heartbeat) = self.heartbeat.take() {
            if let Err(error) = heartbeat.await {
                tracing::warn!(error = %error, "cluster job heartbeat task failed during release");
            }
        }
        let token = self.fence.snapshot().await;
        if let Some(token) = token {
            if let Err(error) = self.coordinator.release(&token).await {
                tracing::warn!(
                    resource = token.resource,
                    fence = token.fence,
                    error = %error,
                    "cluster job lease release failed; TTL will recover it"
                );
            }
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
        self.cancel.cancel();
        self.lost.cancel();
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
    }
}

pub(crate) async fn acquire_cluster_job(
    coordinator: &StoreCoordinator,
    resource: String,
) -> Result<Option<ActiveJobLease>, StoreError> {
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
