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
        // Publication is removed only for final destruction. The lifecycle
        // still owns the original lease/client if that observation is cancelled.
        if self
            .shared
            .ordinary_collector
            .lock()
            .expect("ordinary collector publication")
            .is_none()
        {
            return root
                .lock()
                .await
                .shutdown(deadline)
                .await
                .map_err(|error| format!("resuming ordinary finalizer close: {error:?}"));
        }
        self.shared.ordinary_collector_draining.store(true, Release);
        let runtime = self
            .shared
            .resource_runtime
            .as_ref()
            .ok_or("ordinary finalizer has no runtime")?;
        runtime
            .begin_finite_shutdown_exit()
            .map_err(|error| error.to_string())?;
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
        // Terminal replay records still own their original finite client
        // slots after reader detach. At process exit their actual payloads
        // must be destroyed before S waits for its allocation partition.
        // Join each original cleanup first; cancellation leaves that same
        // session and cleanup discoverable for the next shutdown observer.
        loop {
            let next = self
                .shared
                .sessions
                .lock()
                .await
                .iter()
                .next()
                .map(|(id, session)| {
                    (
                        id.clone(),
                        session.terminal_cleanup.clone(),
                        session.finite_session_slot.clone(),
                    )
                });
            let Some((id, cleanup, session_slot)) = next else {
                break;
            };
            let cleanup = cleanup.ok_or("terminal shutdown session has no original cleanup")?;
            tokio::time::timeout_at(deadline.into(), cleanup.wait())
                .await
                .map_err(|_| "original terminal session cleanup deadline elapsed")?;
            let (removed, empty_backing) = {
                let mut sessions = self.shared.sessions.lock().await;
                let removed = if sessions.get(&id).is_some_and(|session| {
                    session.tombstone.is_some()
                        && session
                            .terminal_cleanup
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, &cleanup))
                }) {
                    sessions.remove_entry(&id)
                } else {
                    return Err("original terminal shutdown session changed".into());
                };
                let backing = sessions.is_empty().then(|| std::mem::take(&mut *sessions));
                (removed, backing)
            };
            drop(removed);
            drop(empty_backing);
            drop(cleanup);
            drop(id);
            // Keep the original session allowance until the temporary key
            // and cleanup aliases above have also been fully destroyed.
            drop(session_slot);
        }
        // Install original opaque S selectors in the paid fixed pool BEFORE
        // any serving-map removal. Cancellation leaves retry custody there.
        let client = self
            .shared
            .ordinary_collector
            .lock()
            .expect("ordinary collector publication")
            .clone()
            .ok_or("ordinary finalizer is unavailable during drain")?;
        client
            .capture_retained_shutdown()
            .map_err(|error| error.to_string())?;
        loop {
            let next = self
                .shared
                .renditions
                .lock()
                .await
                .keys()
                .find(|key| client.retained_shutdown_key(key))
                .cloned();
            let Some(key) = next else {
                break;
            };
            self.shared.detach_retained_shutdown(&key, deadline).await?;
        }
        client.request_retained_shutdown();
        client
            .wait_retained_shutdown(deadline)
            .await
            .map_err(|error| error.to_string())?;
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
        // Recheck actual runtime custody after ordinary R close progressed.
        // Capacity exhaustion or an uncertain adoption keeps support held.
        client
            .capture_retained_shutdown()
            .map_err(|error| error.to_string())?;
        client.request_retained_shutdown();
        client
            .wait_retained_shutdown(deadline)
            .await
            .map_err(|error| error.to_string())?;
        drop(client);
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
    /// Only process-exit detach: the paid collector already owns the exact S
    /// selector. Preserve its durable assets, census, and original charge.
    async fn detach_retained_shutdown(
        self: &Arc<Self>,
        key: &str,
        deadline: Instant,
    ) -> Result<(), String> {
        let gate = self
            .rendition_build_gate(key)
            .map_err(|error| error.to_string())?;
        let _guard = tokio::time::timeout_at(deadline.into(), gate.lock_owned())
            .await
            .map_err(|_| "original retained shutdown gate is still held")?;
        let rendition = self.renditions.lock().await.get(key).cloned();
        let Some(rendition) = rendition else {
            return Ok(());
        };
        if let Some(producer) = rendition.dir.finite_producer() {
            producer
                .join_installed_driver()
                .await
                .map_err(|error| error.to_string())?;
        }
        let removed = {
            let mut renditions = self.renditions.lock().await;
            if renditions
                .get(key)
                .is_some_and(|current| Arc::ptr_eq(current, &rendition))
            {
                renditions.remove(key)
            } else {
                None
            }
        };
        rendition.closed.store(true, Release);
        rendition.kick();
        self.pool.close(key);
        drop(removed);
        drop(rendition);
        Ok(())
    }

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
