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
                held.feed
                    .queued_bytes
                    .fetch_sub(bytes.len() as u64, Ordering::AcqRel);
                Some((Ok::<_, io::Error>(bytes), held))
            },
        )))
    }
}
