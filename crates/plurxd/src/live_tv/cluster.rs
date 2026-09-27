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
    pub retiring: bool,
}
#[derive(Default)]
pub(crate) struct Placements {
    entries: BTreeMap<(i64, String), Placement>,
    retired: BTreeMap<(i64, String), (tokio::time::Instant, u64)>,
    retire_sequence: u64,
    generation: i64,
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
        if request.config_generation < self.generation {
            return Err(LiveTvError::Conflict(
                "the Live TV placement generation is stale".into(),
            ));
        }
        self.generation = request.config_generation;
        let now = tokio::time::Instant::now();
        self.retired
            .retain(|_, (at, _)| now.duration_since(*at) < RETIRED_TTL);
        if self
            .retired
            .contains_key(&(request.user_id, request.request_id.clone()))
        {
            return Err(LiveTvError::Conflict(
                "the Live TV request was retired before admission".into(),
            ));
        }
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
    pub fn begin_retire(
        &mut self,
        user: i64,
        request: &str,
    ) -> Result<Option<Placement>, LiveTvError> {
        let key = (user, request.to_owned());
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.retiring = true;
            return Ok(Some(entry.clone()));
        }
        let now = tokio::time::Instant::now();
        self.retired
            .retain(|_, (at, _)| now.duration_since(*at) < RETIRED_TTL);
        let sequence = self.retire_sequence;
        self.retire_sequence = self.retire_sequence.saturating_add(1);
        self.retired.insert(key, (now, sequence));
        while self
            .retired
            .keys()
            .filter(|(owner, _)| *owner == user)
            .count()
            > MAX_RETIRED_PER_USER
        {
            let oldest = self
                .retired
                .iter()
                .filter(|((owner, _), _)| *owner == user)
                .min_by_key(|(_, (at, sequence))| (*at, *sequence))
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.retired.remove(&oldest);
            }
        }
        Ok(None)
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

#[cfg(test)]
mod tests {
    use super::*;
    fn request(id: u32) -> LiveTvStartRequest {
        LiveTvStartRequest {
            expected_owner_node_id: "tuner".into(),
            source_node_id: "ingress".into(),
            user_id: 7,
            user_name: "viewer".into(),
            request_id: format!("{id:032x}"),
            channel_id: "7.1".into(),
            config_generation: 1,
            source_serving_generation: 1,
            playback: None,
        }
    }
    #[test]
    fn ambiguous_start_keeps_its_worker_and_retirement_fences_late_ingest() {
        let mut placements = Placements::default();
        let start = request(1);
        let first = placements.assign(&start, "worker-a".into()).expect("admit");
        let replay = placements
            .assign(&start, "worker-b".into())
            .expect("recover after a lost reply");
        assert_eq!(replay.worker, first.worker);
        assert_eq!(replay.nonce, first.nonce);
        assert!(placements.authorizes(&start, "worker-a", &first.nonce));
        assert!(!placements.authorizes(&start, "worker-b", &first.nonce));
        let mut other_user = start.clone();
        other_user.user_id += 1;
        assert!(!placements.authorizes(&other_user, "worker-a", &first.nonce));
        placements
            .begin_retire(start.user_id, &start.request_id)
            .expect("fence before remote exchange");
        assert!(!placements.authorizes(&start, "worker-a", &first.nonce));
        assert!(placements.assign(&start, "worker-b".into()).is_err());
        let late = request(2);
        placements
            .begin_retire(late.user_id, &late.request_id)
            .expect("retire an unknown start");
        assert!(
            placements.assign(&late, "worker-b".into()).is_err(),
            "retirement wins before the first placement"
        );
    }
    #[test]
    fn full_placement_history_refuses_new_work_without_evicting_ambiguous_owners() {
        let mut placements = Placements::default();
        for id in 0..1024 {
            placements
                .assign(&request(id), "worker-a".into())
                .expect("bounded history");
        }
        assert!(matches!(
            placements.assign(&request(1024), "worker-b".into()),
            Err(LiveTvError::Capacity(_))
        ));
        assert_eq!(
            placements
                .assign(&request(0), "worker-b".into())
                .expect("original still recoverable")
                .worker,
            "worker-a"
        );
        let held = placements
            .begin_retire(7, &request(0).request_id)
            .expect("full history still retires existing")
            .expect("owned");
        assert!(!placements.authorizes(&request(0), &held.worker, &held.nonce));
    }
    #[test]
    fn one_users_unknown_retirements_do_not_consume_another_users_placement_capacity() {
        let mut placements = Placements::default();
        for id in 0..2048 {
            placements
                .begin_retire(7, &request(id).request_id)
                .expect("per-user retirement");
        }
        assert_eq!(placements.retired.len(), MAX_RETIRED_PER_USER);
        let mut other = request(3000);
        other.user_id = 8;
        placements
            .assign(&other, "worker".into())
            .expect("another viewer can start");
        assert!(
            placements.assign(&request(2047), "worker".into()).is_err(),
            "the latest retirement still fences its viewer"
        );
    }
    #[test]
    fn delayed_old_generation_cannot_erase_a_newer_worker_assignment() {
        let mut placements = Placements::default();
        let old = request(1);
        placements
            .assign(&old, "old-worker".into())
            .expect("initial generation");
        let mut current = request(2);
        current.config_generation = 2;
        let held = placements
            .assign(&current, "current-worker".into())
            .expect("new generation");
        assert!(placements.assign(&old, "different-worker".into()).is_err());
        let replay = placements
            .assign(&current, "different-worker".into())
            .expect("current recovery");
        assert_eq!(replay.worker, held.worker);
        assert_eq!(replay.nonce, held.nonce);
        assert!(placements.authorizes(&current, &held.worker, &held.nonce));
    }
}
