//! Ordinary finite cleanup runs in the independently charged permanent service
//! frames. Native close authority and original ticket custody remain in core.
use super::*;
use plurx_core::adaptive_resources::runtime::ordinary_finite_collector::{
    CoreServicePort, OrdinarySlot, DAEMON_CLOSING_ENVELOPE_BYTES,
};

impl VodServe {
    pub(crate) async fn initialize_ordinary_finalizer(
        &self,
        deadline: Instant,
    ) -> Result<(), String> {
        let Some(runtime) = self.shared.resource_runtime.as_ref() else {
            return Ok(());
        };
        // Publication failure never restores the uncollected ordinary path.
        self.shared.ordinary_collector_required.store(true, Release);
        let root = self
            .ordinary_finalizer
            .as_ref()
            .ok_or("ordinary finalizer has no external lifecycle root")?;
        let mut lifecycle = root.lock().await;
        let client = lifecycle
            .start(runtime, Arc::clone(&self.shared), slot_service, deadline)
            .await
            .map_err(|error| format!("starting ordinary finalizer: {error:?}"))?;
        *self
            .shared
            .ordinary_collector
            .lock()
            .expect("ordinary collector publication") = Some(client);
        Ok(())
    }

    pub(crate) async fn shutdown_ordinary_finalizer(
        &self,
        deadline: Instant,
    ) -> Result<(), String> {
        if !self.shared.ordinary_collector_required.load(Acquire) {
            return Ok(());
        }
        let Some(root) = self.ordinary_finalizer.as_ref() else {
            return Ok(());
        };
        self.shared.ordinary_collector_draining.store(true, Release);
        {
            let lifecycle = root.lock().await;
            lifecycle.begin_drain();
        }
        // End one registered session at a time, without retaining a growing
        // shutdown snapshot through the final support refund.
        loop {
            let next = self
                .shared
                .sessions
                .lock()
                .await
                .iter()
                .find(|(_, session)| session.tombstone.is_none())
                .map(|(id, _)| id.clone());
            let Some(id) = next else {
                break;
            };
            tokio::time::timeout_at(deadline.into(), self.end(&id, Terminal::AdminStop))
                .await
                .map_err(|_| "ordinary session shutdown deadline elapsed")?;
        }
        loop {
            let next = self.shared.renditions.lock().await.keys().next().cloned();
            let Some(key) = next else {
                break;
            };
            self.shared.purge_if_dormant(&key, Duration::ZERO).await;
            if self.shared.renditions.lock().await.contains_key(&key) {
                return Err("a rendition still needs its original shutdown gate".into());
            }
            if Instant::now() >= deadline {
                return Err("ordinary rendition shutdown deadline elapsed".into());
            }
        }
        loop {
            let next = self
                .shared
                .finite_closing
                .lock()
                .await
                .iter()
                .next()
                .map(|(key, owner)| (key.clone(), owner.clone()));
            let Some((key, owner)) = next else {
                break;
            };
            self.shared.retry_finite_closing(key, owner).await;
            if Instant::now() >= deadline {
                return Err("ordinary close shutdown deadline elapsed".into());
            }
        }
        let empty_backing = {
            let mut map = self.shared.finite_closing.lock().await;
            if !map.is_empty() {
                return Err("original closing registry is not empty".into());
            }
            std::mem::take(&mut *map)
        };
        drop(empty_backing);
        // No generic legacy cleanup is hidden by the global directory seal.
        self.shared
            .ordinary_collector
            .lock()
            .expect("ordinary collector publication")
            .take();
        root.lock()
            .await
            .shutdown(deadline)
            .await
            .map_err(|error| format!("closing ordinary finalizer: {error:?}"))
    }
}

impl Shared {
    pub(super) fn reserve_collector_slot(&self, key: &str) -> Result<Option<OrdinarySlot>, String> {
        let refusal =
            |message: String| crate::transcode::vod_refusal_error("vod_family_capacity", message);
        if self.ordinary_collector_draining.load(Acquire) {
            return Err(refusal("ordinary finalizer is draining".into()));
        }
        let client = self
            .ordinary_collector
            .lock()
            .expect("ordinary collector publication")
            .clone();
        let Some(client) = client else {
            return if self.ordinary_collector_required.load(Acquire) {
                Err(refusal("ordinary finalizer is unavailable".into()))
            } else {
                Ok(None)
            };
        };
        let bytes = finite_metadata::closing_owner_heap_bytes(key.len())
            .ok_or_else(|| refusal("ordinary closing envelope overflowed".into()))?;
        if bytes > DAEMON_CLOSING_ENVELOPE_BYTES {
            return Err(refusal(
                "ordinary closing envelope exceeds its independently charged allowance".into(),
            ));
        }
        client
            .reserve(key)
            .map(Some)
            .map_err(|error| refusal(format!("ordinary finalizer slot unavailable: {error:?}")))
    }
}

/// Exactly this concrete F is sized by the core before any of its N boxes are
/// allocated. Shared is an independently rooted graph; no Arc envelope is a
/// claim about the complete memory/CPU peak of that graph.
async fn slot_service(port: CoreServicePort, shared: Arc<Shared>) {
    while let Some(work) = port.wait_work().await {
        loop {
            let result = shared.drive_collected_closing(&port, &work).await;
            match result {
                Ok(()) => break,
                Err(error) => {
                    tracing::debug!(target: "plurxd::vodserve", rendition = %work.key(),
                        %error, "original finite close remains held in its permanent service");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}
