//! Bounded raw transport consumers for remote Live TV processing.
//!
//! The configured tuner owner remains the only node that opens the device.
//! A remote processor takes an ordinary viewer seat on that same transport.

use super::*;

pub(crate) const INGEST_PATH: &str = "/_internal/v1/live-tv/ingest";

/// Owns exactly one fan-out queue. Dropping the HTTP body detaches that queue
/// synchronously; the transport's existing last-consumer retirement joins its
/// reader. Neither a disconnected nor a slow peer can retain another viewer.
struct IngestFeed {
    transport: Arc<dvr::DvrTransport>,
    consumer: Arc<dvr::ViewerConsumer>,
    feed: dvr::ViewerFeed,
    cancel: CancellationToken,
    serving: crate::serving_fence::ServingAuthority,
    generation: u64,
    manager: Weak<LiveTvManager>,
    request: LiveTvStartRequest,
}
impl Drop for IngestFeed {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.transport.detach_viewer(&self.consumer.capability);
    }
}

impl LiveTvManager {
    pub(crate) async fn shared_ingest(
        self: &Arc<Self>,
        request: &LiveTvStartRequest,
    ) -> Result<Body, LiveTvError> {
        validate_start_request(request)?;
        let config = self.config().await?;
        validate_start_config(&config, request, &self.node_id)?;
        let generation = self.serving.admit().ok_or_else(|| {
            LiveTvError::OwnerUnavailable(crate::serving_fence::SERVING_FENCED_MESSAGE.into())
        })?;
        let snapshot = self.local_snapshot(&config, true, false).await?;
        if snapshot.freshness != SnapshotFreshness::Fresh {
            return Err(LiveTvError::DeviceUnavailable(
                "a fresh tuner lineup is required".into(),
            ));
        }
        let channel = snapshot
            .channels
            .into_iter()
            .find(|c| c.id == request.channel_id)
            .ok_or_else(|| {
                LiveTvError::ChannelNotFound("the channel left the tuner lineup".into())
            })?;
        if channel.drm || channel.support == LiveTvChannelSupport::DrmUnsupported {
            return Err(LiveTvError::DrmUnsupported(
                "the channel is protected".into(),
            ));
        }
        let address = config
            .device_ipv4
            .ok_or_else(|| LiveTvError::InvalidConfig("the tuner address is missing".into()))?;
        let client = self
            .client
            .as_ref()
            .map_err(|_| LiveTvError::DeviceUnavailable("the tuner client is unavailable".into()))?
            .clone();
        let seat = TransportSeat {
            channel_id: &channel.id,
            device_id: &snapshot.device.device_id,
            address,
            serving_generation: generation,
        };
        let (transport, opened) = {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if registry.closing || request.config_generation < registry.min_generation {
                return Err(LiveTvError::Conflict(
                    "the ingest was fenced by a drain".into(),
                ));
            }
            match registry.viewer_admission(
                &seat,
                request.user_id,
                tokio::time::Instant::now(),
                config.max_sessions,
            ) {
                ViewerAdmission::Join(transport) if transport.reserve_seat() => (transport, false),
                ViewerAdmission::Open => {
                    let transport = dvr::DvrTransport::new(dvr::TransportInit {
                        channel: channel.clone(),
                        generation: request.config_generation,
                        owner_serving_generation: generation,
                        device_id: snapshot.device.device_id,
                        address,
                        origin: dvr::TransportOrigin::Viewer,
                        scratch: self.transport_scratch(),
                        metrics: Arc::clone(&self.metrics),
                        seats: 1,
                    });
                    registry
                        .transports
                        .insert(channel.id, Arc::clone(&transport));
                    (transport, true)
                }
                ViewerAdmission::Refuse(error) => return Err(error),
                _ => {
                    return Err(LiveTvError::Capacity(
                        "the prior channel transport is still closing".into(),
                    ))
                }
            }
        };
        let cancel = CancellationToken::new();
        let (consumer, feed) =
            dvr::ViewerConsumer::new(format!("peer-{}", uuid::Uuid::new_v4()), cancel.clone());
        let attached = transport.attach_viewer(Arc::clone(&consumer));
        transport.release_seat();
        if !attached {
            if opened {
                self.abandon_unstarted_transport(&transport);
            }
            return Err(LiveTvError::StreamFailed(
                "the shared transport closed during admission".into(),
            ));
        }
        if opened {
            self.spawn_transport_worker(&transport, client);
        }
        let held = IngestFeed {
            transport,
            consumer,
            feed,
            cancel,
            serving: self.serving.clone(),
            generation,
            manager: Arc::downgrade(self),
            request: request.clone(),
        };
        Ok(Body::from_stream(futures_util::stream::unfold(
            held,
            |mut held| async move {
                let bytes = tokio::select! {
                    biased;
                    _ = held.cancel.cancelled() => return None,
                    next = tokio::time::timeout(TUNER_READ_TIMEOUT, held.feed.rx.recv()) => next.ok().flatten()?,
                };
                if !held.serving.is_current(held.generation) {
                    return None;
                }
                let manager = held.manager.upgrade()?;
                let observation = manager.fence.validated().ok()?;
                if validate_start_config(&observation.config, &held.request, &manager.node_id)
                    .is_err()
                {
                    return None;
                }
                held.feed
                    .queued_bytes
                    .fetch_sub(bytes.len() as u64, Ordering::AcqRel);
                Some((Ok::<_, io::Error>(bytes), held))
            },
        )))
    }
}

pub(crate) const PLACEMENT_PATH: &str = "/_internal/v1/live-tv/placement";
pub(crate) const PROCESS_PATH: &str = "/_internal/v1/live-tv/process";

/// A new wire shape preserves the legacy start protocol's exact signature.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlacedStart {
    pub request: LiveTvStartRequest,
    pub playback: Option<LivePlaybackRequest>,
}
impl PlacedStart {
    pub fn from_request(request: &LiveTvStartRequest) -> Self {
        Self {
            request: request.clone(),
            playback: request.playback.clone(),
        }
    }
    pub fn into_request(mut self) -> LiveTvStartRequest {
        self.request.playback = self.playback;
        self.request
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProcessingStart {
    pub start: PlacedStart,
    pub worker: String,
    pub nonce: String,
}
#[derive(Clone)]
pub(crate) struct Placement {
    pub request: LiveTvStartRequest,
    pub worker: String,
    pub nonce: String,
    created: tokio::time::Instant,
    checked: tokio::time::Instant,
    finished: Option<tokio::time::Instant>,
    retiring: bool,
}
#[derive(Default)]
pub(crate) struct Placements {
    entries: BTreeMap<(i64, String), Placement>,
}
impl Placements {
    pub fn get(&self, user: i64, request: &str) -> Option<Placement> {
        self.entries.get(&(user, request.to_owned())).cloned()
    }
    pub fn assign(
        &mut self,
        request: &LiveTvStartRequest,
        worker: String,
    ) -> Result<Placement, LiveTvError> {
        let now = tokio::time::Instant::now();
        self.entries.retain(|_, entry| {
            entry.request.config_generation == request.config_generation
                && entry
                    .finished
                    .is_none_or(|at| now.duration_since(at) < Duration::from_secs(60))
        });
        if let Some(held) = self.get(request.user_id, &request.request_id) {
            if held.request != *request || held.finished.is_some() || held.retiring {
                return Err(LiveTvError::Conflict(
                    "the Live TV request was changed or retired".into(),
                ));
            }
            return Ok(held);
        }
        if self.entries.len() >= 1024 {
            return Err(LiveTvError::Capacity(
                "Live TV placement recovery history is full".into(),
            ));
        }
        let entry = Placement {
            request: request.clone(),
            worker,
            nonce: uuid::Uuid::new_v4().to_string(),
            created: now,
            checked: now,
            finished: None,
            retiring: false,
        };
        self.entries
            .insert((request.user_id, request.request_id.clone()), entry.clone());
        Ok(entry)
    }
    pub fn begin_retire(&mut self, user: i64, request: &str) {
        if let Some(entry) = self.entries.get_mut(&(user, request.to_owned())) {
            entry.retiring = true;
        }
    }
    pub fn finish(&mut self, user: i64, request: &str) {
        if let Some(entry) = self.entries.get_mut(&(user, request.to_owned())) {
            entry.finished.get_or_insert_with(tokio::time::Instant::now);
        }
    }
    /// Probe at most eight old assignments per admission. Unknown or terminal
    /// workers are explicitly retired before forgetting ownership; silence is
    /// never a reason to scatter the same start to another processor.
    pub fn due(&mut self) -> Vec<Placement> {
        let now = tokio::time::Instant::now();
        let mut entries = self
            .entries
            .values_mut()
            .filter(|e| {
                e.finished.is_none()
                    && now.duration_since(e.created) >= Duration::from_secs(60)
                    && now.duration_since(e.checked) >= Duration::from_secs(30)
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|e| e.checked);
        entries
            .into_iter()
            .take(8)
            .map(|e| {
                e.checked = now;
                e.clone()
            })
            .collect()
    }
    pub fn authorizes(&self, request: &LiveTvStartRequest, worker: &str, nonce: &str) -> bool {
        self.get(request.user_id, &request.request_id)
            .is_some_and(|e| {
                e.request == *request
                    && e.worker == worker
                    && e.nonce == nonce
                    && e.finished.is_none()
                    && !e.retiring
            })
    }
}
impl LiveTvManager {
    pub(crate) fn placements(&self) -> std::sync::MutexGuard<'_, Placements> {
        self.placements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl LiveTvManager {
    pub(crate) async fn recover_processing(
        &self,
        request: &LiveTvStartRequest,
    ) -> Result<Option<LiveTvProvisional>, LiveTvError> {
        validate_start_request(request)?;
        let config = self.config().await?;
        validate_start_config(&config, request, &request.expected_owner_node_id)?;
        let existing = self.session_for_request(&LiveTvRequestKey::from(request), request)?;
        match existing {
            Some(session) => {
                if !self.serving.is_current(session.owner_serving_generation) {
                    return Err(LiveTvError::OwnerUnavailable(
                        "the processor lost serving authority".into(),
                    ));
                }
                wait_for_startup(session, true).await.map(Some)
            }
            None => Ok(None),
        }
    }
}
