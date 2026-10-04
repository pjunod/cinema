//! B status and control for one actual retained receiver actor.
//!
//! The client speaks the ordinary `/api/v1/hls/{session}/status|control`
//! grammar against B's own session tuple. B validates that tuple, translates
//! the exchange onto the received Source tuple, owns the sent exchange
//! independently of the HTTP waiter, and rebinds the accepted answer to B.
//! No Source identity, URL or prose crosses back to the client.
use super::*;
use crate::{
    http::{
        hls::control_error,
        shared_source_playback::control::{SharedControlRefusal, SharedControlRefusalCode},
    },
    playback_control::{ControlAction, ControlRelayRequest, ControlRequestV1, ControlResponseV1},
    sharing_client::{PeerConnection, SharedVodStatus, SourcePeerLineage},
};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// B's own retained tuple for one started shared playback. The projected
/// Start bootstrap the client received must carry exactly this tuple.
#[derive(Clone)]
pub(super) struct ReceiverTuple {
    session: Uuid,
    incarnation: Uuid,
    owner_node_id: String,
    owner_epoch: u64,
    duration_ms: Option<i64>,
}

/// What B answers a client control exchange with, before HTTP encoding.
pub(super) enum SharedControlAnswer {
    Accepted(Box<ControlResponseV1>),
    Refused(SharedControlRefusal),
    /// The Source cannot honor this exchange's shape (a changed selection, an
    /// intent envelope or an acknowledgement) on the current rendition.
    Unsupported,
}

impl ReceiverStartActor {
    pub(super) fn receiver_tuple(&self) -> Result<ReceiverTuple, ReceiverStartError> {
        let owned = self.0.state.lock().expect("receiver owner");
        if owned.retirement_started {
            return Err(ReceiverStartError::Unresolved);
        }
        let Some(Ok(start)) = owned.start.as_ref() else {
            return Err(ReceiverStartError::Unresolved);
        };
        let owner = owned.owner.as_ref().ok_or(ReceiverStartError::Unresolved)?;
        let owner_epoch =
            u64::try_from(owner.owner_epoch).map_err(|_| ReceiverStartError::Unresolved)?;
        let control = start
            .control
            .as_ref()
            .ok_or(ReceiverStartError::Unresolved)?;
        if owner_epoch == 0
            || control.generation != owner.incarnation_id.to_string()
            || control.control_epoch != owner_epoch
            || start.session_id != owner.session_id.to_string()
        {
            return Err(ReceiverStartError::Unresolved);
        }
        Ok(ReceiverTuple {
            session: owner.session_id,
            incarnation: owner.incarnation_id,
            owner_node_id: owner.owner_node_id.clone(),
            owner_epoch,
            duration_ms: start.duration_ms,
        })
    }

    async fn verified_source_peer(
        &self,
        state: &AppState,
        received: &ReceivedSource,
        lifetime: Arc<dyn Send + Sync>,
    ) -> Result<PeerConnection, ReceiverStartError> {
        let expected = plurx_core::sharing::SharingIdentity {
            server_id: self.0.intent.scope.source_server_id,
            catalogue_epoch: self.0.intent.scope.catalogue_epoch,
            created_at_ms: 0,
        };
        let (peer, _) = PeerConnection::verified_with_lifetime(
            &state.sharing,
            std::slice::from_ref(&received.endpoint),
            &expected,
            lifetime,
        )
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?;
        Ok(peer)
    }

    /// Live Source VOD metrics. Reading status authorizes nothing and never
    /// renews B or Source activity.
    pub(super) async fn shared_vod_status(
        &self,
        state: &AppState,
    ) -> Result<SharedVodStatus, ReceiverStartError> {
        let lifetime: Arc<dyn Send + Sync> = self.0.bodies.reserve()?;
        let actor = self.clone();
        let state = state.clone();
        tokio::spawn(async move {
            let custody = lifetime.clone();
            let result = actor.shared_vod_status_owned(&state, lifetime).await;
            drop(custody);
            result
        })
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
    }

    async fn shared_vod_status_owned(
        &self,
        state: &AppState,
        lifetime: Arc<dyn Send + Sync>,
    ) -> Result<SharedVodStatus, ReceiverStartError> {
        let (_, _, received) = self.current_delivery_attachment(state).await?;
        let mut peer = self
            .verified_source_peer(state, &received, lifetime)
            .await?;
        let known = SourcePeerLineage::from_start(received.incarnation, &received.response)
            .map_err(|_| ReceiverStartError::Unresolved)?;
        let receipt = peer
            .file_vod_status(
                &received.credential,
                &received.viewer_hash,
                &self.0.peer_session,
                &known,
            )
            .await
            .map_err(|_| ReceiverStartError::Unresolved)?;
        // Source IO can park; the original login and exact binding must still
        // hold before any observation reaches the client.
        self.current_delivery_attachment(state).await?;
        Ok(receipt.into_status())
    }

    /// Translate one current-rendition exchange onto the Source tuple. Once
    /// sent, the exchange belongs to an owned task: a cancelled B waiter can
    /// only lose the answer, which the same-sequence replay recovers.
    pub(super) async fn shared_control(
        &self,
        state: &AppState,
        tuple: &ReceiverTuple,
        request: &ControlRequestV1,
    ) -> Result<SharedControlAnswer, ReceiverStartError> {
        let lifetime: Arc<dyn Send + Sync> = self.0.bodies.reserve()?;
        let actor = self.clone();
        let state = state.clone();
        let tuple = tuple.clone();
        let request = request.clone();
        tokio::spawn(async move {
            let custody = lifetime.clone();
            let result = actor
                .shared_control_owned(&state, &tuple, &request, lifetime)
                .await;
            drop(custody);
            result
        })
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
    }

    async fn shared_control_owned(
        &self,
        state: &AppState,
        tuple: &ReceiverTuple,
        request: &ControlRequestV1,
        lifetime: Arc<dyn Send + Sync>,
    ) -> Result<SharedControlAnswer, ReceiverStartError> {
        let (_, _, received) = self.current_delivery_attachment(state).await?;
        let known = SourcePeerLineage::from_start(received.incarnation, &received.response)
            .map_err(|_| ReceiverStartError::Unresolved)?;
        let forwarded = translate_to_source(request, &received)?;
        let mut peer = self
            .verified_source_peer(state, &received, lifetime)
            .await?;
        let receipt = peer
            .file_control(
                &received.credential,
                &received.viewer_hash,
                &self.0.peer_session,
                &known,
                &forwarded,
            )
            .await;
        self.current_delivery_attachment(state).await?;
        let receipt = match receipt {
            Ok(receipt) => receipt,
            Err(crate::sharing_client::PeerError::Rejected(status))
                if status == StatusCode::UNPROCESSABLE_ENTITY =>
            {
                return Ok(SharedControlAnswer::Unsupported);
            }
            Err(crate::sharing_client::PeerError::Rejected(status))
                if status == StatusCode::CONFLICT =>
            {
                return Ok(SharedControlAnswer::Refused(SharedControlRefusal {
                    code: SharedControlRefusalCode::StaleControl,
                    retry_after_ms: None,
                }));
            }
            Err(_) => {
                return Ok(SharedControlAnswer::Refused(SharedControlRefusal {
                    code: SharedControlRefusalCode::Unavailable,
                    retry_after_ms: Some(500),
                }));
            }
        };
        Ok(match receipt.into_outcome() {
            Ok(response) => SharedControlAnswer::Accepted(Box::new(
                rebind_to_receiver(response, tuple, request)
                    .ok_or(ReceiverStartError::Unresolved)?,
            )),
            Err(refusal) => SharedControlAnswer::Refused(refusal),
        })
    }
}

/// B's request on B's tuple becomes the same request on the received Source
/// tuple. Sequence, client identity, capabilities and the frozen desired
/// selection are kept exactly; B never offers the Source a preparation it
/// cannot carry back.
fn translate_to_source(
    request: &ControlRequestV1,
    received: &ReceivedSource,
) -> Result<ControlRequestV1, ReceiverStartError> {
    let bootstrap = received
        .response
        .control
        .as_ref()
        .ok_or(ReceiverStartError::Unresolved)?;
    let mut forwarded = request.clone();
    forwarded.generation = bootstrap.generation.clone();
    forwarded.control_epoch = bootstrap.control_epoch;
    if let Some(actions) = forwarded.supported_actions.as_mut() {
        // Offer the Source only the advisory actions B can rebind; a
        // preparation transaction names a Source session and URL.
        actions.retain(|action| crate::playback_control::is_advisory_action_name(action));
    }
    Ok(forwarded)
}

/// Rebind a Source-accepted answer to B's own tuple. The result must satisfy
/// the same relay contract against B's tuple and the client's own request.
fn rebind_to_receiver(
    mut response: ControlResponseV1,
    tuple: &ReceiverTuple,
    original: &ControlRequestV1,
) -> Option<ControlResponseV1> {
    response.generation = tuple.incarnation.to_string();
    response.control_epoch = tuple.owner_epoch;
    response.delivery.owner_node_hash = crate::playback_control::node_hash(&tuple.owner_node_id);
    response.delivery.owner_epoch = tuple.owner_epoch;
    match &mut response.action {
        ControlAction::Terminal { code, message } => {
            *message = crate::playback_control::terminal_message(*code);
        }
        ControlAction::Prepare { .. } => return None,
        ControlAction::None | ControlAction::Hold { .. } | ControlAction::RetryResource { .. } => {}
    }
    let relay = ControlRelayRequest {
        session_id: tuple.session.to_string(),
        generation: tuple.incarnation.to_string(),
        expected_owner_node_id: tuple.owner_node_id.clone(),
        expected_owner_epoch: i64::try_from(tuple.owner_epoch).ok()?,
        deadline_unix_ms: 0,
        control: original.clone(),
    };
    response.is_valid_for(&relay).then_some(response)
}

fn refusal_response(refusal: &SharedControlRefusal, tuple: &ReceiverTuple) -> Response {
    use SharedControlRefusalCode as C;
    let generation = Some(tuple.incarnation.to_string());
    let epoch = Some(tuple.owner_epoch);
    let (status, code, message) = match refusal.code {
        C::StaleControl => (
            StatusCode::CONFLICT,
            "stale_control",
            "the generation, client instance, or sequence fence is stale",
        ),
        C::OwnerChanged => (
            StatusCode::CONFLICT,
            "owner_changed",
            "the media session owner epoch changed",
        ),
        C::RateLimited => (
            StatusCode::TOO_MANY_REQUESTS,
            "control_rate_limited",
            "the control sequence advanced faster than the per-session budget",
        ),
        C::SessionEnded => (
            StatusCode::GONE,
            "session_ended",
            "the shared media session ended before control could renew it",
        ),
        C::OwnerTransition => (
            StatusCode::TOO_EARLY,
            "owner_transition",
            "the shared media owner is not currently authorizing control",
        ),
        C::OwnerLost => (
            StatusCode::GONE,
            "owner_lost",
            "this shared media session can no longer be recovered; reopen playback",
        ),
        C::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "control_unavailable",
            "the shared media owner could not revalidate control authority",
        ),
    };
    control_error(
        status,
        code,
        message,
        generation,
        epoch,
        refusal.retry_after_ms,
        None,
    )
}

fn unavailable_without_tuple() -> Response {
    control_error(
        StatusCode::TOO_EARLY,
        "owner_transition",
        "the shared media session is not yet available for control",
        None,
        None,
        Some(500),
        None,
    )
}

fn with_writer(response: Response, guard: Arc<dyn Send + Sync>) -> Response {
    use futures_util::StreamExt;
    let (parts, body) = response.into_parts();
    let stream = body.into_data_stream().map(move |frame| {
        let _accepted_writer = &guard;
        frame
    });
    Response::from_parts(parts, axum::body::Body::from_stream(stream))
}

/// `GET /api/v1/hls/{B}/status` for a shared session: the agreed Shared
/// grammar, bound to B's tuple and the complete shared file reference.
pub(super) async fn receiver_status(
    actor: ReceiverStartActor,
    state: Arc<AppState>,
    connection: &crate::SharingConnectionCancellation,
) -> Response {
    let Ok(tuple) = actor.receiver_tuple() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let status = match actor.shared_vod_status(&state).await {
        Ok(status) => status,
        Err(ReceiverStartError::Capacity) => return StatusCode::TOO_MANY_REQUESTS.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    // The tuple cannot have moved under a parked observation.
    match actor.receiver_tuple() {
        Ok(current)
            if current.session == tuple.session
                && current.incarnation == tuple.incarnation
                && current.owner_epoch == tuple.owner_epoch => {}
        _ => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let recipe = &actor.0.intent.recipe;
    let body = serde_json::json!({
        "subject": "shared",
        "reference": {
            "item": recipe.reference,
            "file_id": recipe.file_id,
            "revision": recipe.file_revision,
            "lifecycle_generation": recipe.lifecycle_generation,
        },
        "session_id": tuple.session,
        "incarnation_id": tuple.incarnation,
        "control_epoch": tuple.owner_epoch,
        "status": status,
    });
    let guard = match actor.retain_accepted_connection(state, connection) {
        Ok(guard) => guard,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    with_writer(
        (
            StatusCode::OK,
            [(axum::http::header::CACHE_CONTROL, "no-store")],
            axum::Json(body),
        )
            .into_response(),
        guard,
    )
}

/// `POST /api/v1/hls/{B}/control` for a shared session.
pub(super) async fn receiver_control(
    actor: ReceiverStartActor,
    state: Arc<AppState>,
    connection: &crate::SharingConnectionCancellation,
    body: axum::body::Body,
) -> Response {
    let Ok(bytes) = axum::body::to_bytes(body, 64 * 1024).await else {
        return control_error(
            StatusCode::BAD_REQUEST,
            "invalid_control",
            "the control body is not a bounded v1 request",
            None,
            None,
            None,
            None,
        );
    };
    let Ok(request) = serde_json::from_slice::<ControlRequestV1>(&bytes) else {
        return control_error(
            StatusCode::BAD_REQUEST,
            "invalid_control",
            "the control body is not a bounded v1 request",
            None,
            None,
            None,
            None,
        );
    };
    let Ok(tuple) = actor.receiver_tuple() else {
        return unavailable_without_tuple();
    };
    let generation = Some(tuple.incarnation.to_string());
    let epoch = Some(tuple.owner_epoch);
    // The largest target-duration clamp: B refuses only what the Source
    // would certainly refuse, and the Source re-validates exactly.
    if let Err(field) = request.validate(tuple.duration_ms, 30_000) {
        return control_error(
            StatusCode::BAD_REQUEST,
            "invalid_control",
            "a control field is outside the bounded v1 contract",
            generation,
            epoch,
            None,
            Some(field),
        );
    }
    if request.generation != tuple.incarnation.to_string() {
        return control_error(
            StatusCode::CONFLICT,
            "stale_control",
            "the control generation is no longer current",
            generation,
            epoch,
            None,
            None,
        );
    }
    if request.control_epoch != tuple.owner_epoch {
        return control_error(
            StatusCode::CONFLICT,
            "owner_changed",
            "the media session owner epoch changed",
            generation,
            epoch,
            None,
            None,
        );
    }
    if request.demand == crate::playback_control::PlaybackDemand::End {
        // A terminal exchange ends B's own session through the one retirement
        // owner: it answers only after the actual confirmed Source End.
        actor.begin_retirement(
            state,
            plurx_core::sharing_receiver_retirement::ReceiverRetirementReason::Deleted,
        );
        return match actor.wait_confirmed_end(tuple.session).await {
            Ok(()) => control_error(
                StatusCode::GONE,
                "session_ended",
                "the shared media session ended",
                generation,
                epoch,
                None,
                None,
            ),
            Err(_) => control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the shared media session end is not yet confirmed",
                generation,
                epoch,
                Some(500),
                None,
            ),
        };
    }
    let guard = match actor.retain_accepted_connection(state.clone(), connection) {
        Ok(guard) => guard,
        Err(_) => {
            return refusal_response(
                &SharedControlRefusal {
                    code: SharedControlRefusalCode::Unavailable,
                    retry_after_ms: Some(500),
                },
                &tuple,
            )
        }
    };
    let answer = match actor.shared_control(&state, &tuple, &request).await {
        Ok(answer) => answer,
        Err(ReceiverStartError::Capacity) => SharedControlAnswer::Refused(SharedControlRefusal {
            code: SharedControlRefusalCode::RateLimited,
            retry_after_ms: Some(500),
        }),
        Err(_) => SharedControlAnswer::Refused(SharedControlRefusal {
            code: SharedControlRefusalCode::Unavailable,
            retry_after_ms: Some(500),
        }),
    };
    let response = match answer {
        SharedControlAnswer::Accepted(response) => (
            StatusCode::OK,
            [(axum::http::header::CACHE_CONTROL, "no-store")],
            axum::Json(*response),
        )
            .into_response(),
        SharedControlAnswer::Refused(refusal) => {
            if matches!(
                refusal.code,
                SharedControlRefusalCode::SessionEnded | SharedControlRefusalCode::OwnerLost
            ) {
                // The Source session is gone; B's relay retires through its
                // one owner and confirmed End.
                actor.begin_retirement(
                    state,
                    plurx_core::sharing_receiver_retirement::ReceiverRetirementReason::Revoked,
                );
            }
            refusal_response(&refusal, &tuple)
        }
        SharedControlAnswer::Unsupported => control_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "shared_control_unsupported",
            "this shared session accepts only current-rendition controls with its original selection",
            generation,
            epoch,
            None,
            None,
        ),
    };
    with_writer(response, guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback_control::{ControlBootstrap, PROTOCOL_V1};

    fn tuple() -> ReceiverTuple {
        ReceiverTuple {
            session: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
            owner_node_id: "receiver-node".to_owned(),
            owner_epoch: 3,
            duration_ms: Some(600_000),
        }
    }

    fn request(tuple: &ReceiverTuple) -> ControlRequestV1 {
        use crate::playback_control as pc;
        ControlRequestV1 {
            intent: None,
            protocol: PROTOCOL_V1.to_owned(),
            generation: tuple.incarnation.to_string(),
            control_epoch: tuple.owner_epoch,
            client_instance_id: Uuid::new_v4().to_string(),
            sequence: 4,
            demand: pc::PlaybackDemand::Active,
            position_ms: 10_000,
            buffered_from_ms: Some(9_000),
            buffered_through_ms: 30_000,
            playback_rate: 1.0,
            render_state: pc::RenderState::Rendering,
            seek_target_ms: None,
            observed_download_bps: None,
            selection: pc::ClientSelection {
                quality: pc::QualitySelection::Auto {
                    height: None,
                    candidate_id: None,
                },
                audio_track: None,
                subtitle: pc::SubtitleSelection {
                    mode: pc::SubtitleMode::Off,
                    track: None,
                },
                audio_offset_ms: 0,
                codec: pc::CodecPolicy::Auto,
                dynamic_range: pc::DynamicRangePolicy::Auto,
            },
            capabilities: None,
            observation: None,
            acknowledgement: None,
            supported_actions: Some(vec![
                "hold".to_owned(),
                "terminal".to_owned(),
                "retry_resource".to_owned(),
                pc::PREPARE_REPLACEMENT_ACTION.to_owned(),
            ]),
        }
    }

    fn received(source_epoch: u64) -> (ReceivedSource, Uuid, Uuid) {
        let source_session = Uuid::new_v4();
        let source_incarnation = Uuid::new_v4();
        let mut response: StartResponse = serde_json::from_value(serde_json::json!({
            "session_id": source_session,
            "playlist_url": format!("/api/v1/hls/{source_session}/index.m3u8"),
            "duration_ms": 600_000, "start_seconds": 0.0, "media_origin_ms": 0,
            "height": 720, "encoder": "copy", "vod": true, "ladder": [], "plan_notes": []
        }))
        .expect("actual Start DTO");
        response.control = ControlBootstrap::new(
            &source_session.to_string(),
            &source_incarnation.to_string(),
            i64::try_from(source_epoch).expect("bounded fixture epoch"),
            crate::playback_control::VOD_LEASE_TIMEOUT_MS,
        );
        (
            ReceivedSource {
                credential: plurx_core::secrets::Secret::from_cleartext("c".repeat(43)),
                viewer_hash: "a".repeat(64),
                endpoint: plurx_core::sharing::Endpoint {
                    ipv4: std::net::Ipv4Addr::new(100, 64, 0, 1),
                    ipv6: None,
                    ts_fqdn: "source.example.ts.net".to_owned(),
                    port: 443,
                    spki_sha256: "b".repeat(64),
                },
                incarnation: source_incarnation,
                response,
            },
            source_session,
            source_incarnation,
        )
    }

    #[test]
    fn sharing_receiver_control_translates_only_the_tuple_and_never_offers_prepare() {
        let tuple = tuple();
        let original = request(&tuple);
        let (received, _, source_incarnation) = received(11);
        let forwarded = translate_to_source(&original, &received).expect("translated");
        assert_eq!(forwarded.generation, source_incarnation.to_string());
        assert_eq!(forwarded.control_epoch, 11);
        assert_eq!(
            forwarded.supported_actions.as_deref(),
            Some(
                &[
                    "hold".to_owned(),
                    "terminal".to_owned(),
                    "retry_resource".to_owned()
                ][..]
            )
        );
        let mut restored = forwarded.clone();
        restored.generation = original.generation.clone();
        restored.control_epoch = original.control_epoch;
        restored.supported_actions = original.supported_actions.clone();
        assert_eq!(restored, original, "only the tuple and offer may change");
    }

    fn accepted(source: &ControlRequestV1, source_epoch: u64) -> ControlResponseV1 {
        use crate::playback_control as pc;
        let server_time_unix_ms = 1_800_000_000_000_i64;
        ControlResponseV1 {
            protocol: PROTOCOL_V1.to_owned(),
            generation: source.generation.clone(),
            control_epoch: source_epoch,
            accepted_sequence: source.sequence,
            server_time_unix_ms,
            lease: pc::PlaybackLeaseView {
                state: "active".to_owned(),
                renew_after_ms: pc::NEXT_EXCHANGE_MS,
                expires_at_unix_ms: server_time_unix_ms + 30_000,
            },
            delivery: pc::DeliveryView {
                presentation: "vod".to_owned(),
                producer_state: "vod".to_owned(),
                produced_through_ms: Some(60_000),
                fetched_through_ms: 30_000,
                delivered_bps: Some(1_000_000),
                delivered_idle_ms: Some(10),
                recent_producer_speed: None,
                client_runway_ms: 20_000,
                admitted: Some(true),
                hold_reason: None,
                producer_decision: None,
                subtitle_readiness: None,
                preparation: None,
                owner_node_hash: pc::node_hash("source-node"),
                owner_epoch: source_epoch,
            },
            effective_selection: pc::EffectiveSelection {
                candidate_id: None,
                quality_auto: true,
                height: 720,
                audio_track: None,
                subtitle_burn: None,
                audio_offset_ms: 0,
                codec: "source".to_owned(),
                dynamic_range: None,
            },
            action: ControlAction::None,
        }
    }

    #[test]
    fn sharing_receiver_control_rebinds_accepted_answer_to_receiver_tuple() {
        let tuple = tuple();
        let original = request(&tuple);
        let (received, _, _) = received(11);
        let forwarded = translate_to_source(&original, &received).expect("translated");
        let source = accepted(&forwarded, 11);
        let rebound = rebind_to_receiver(source.clone(), &tuple, &original).expect("rebound");
        assert_eq!(rebound.generation, tuple.incarnation.to_string());
        assert_eq!(rebound.control_epoch, tuple.owner_epoch);
        assert_eq!(rebound.delivery.owner_epoch, tuple.owner_epoch);
        assert_eq!(
            rebound.delivery.owner_node_hash,
            crate::playback_control::node_hash("receiver-node")
        );
        assert_ne!(
            rebound.delivery.owner_node_hash,
            source.delivery.owner_node_hash
        );
        assert_eq!(rebound.accepted_sequence, original.sequence);
        assert_eq!(rebound.effective_selection, source.effective_selection);
        // An answer to a different B sequence cannot be rebound.
        let mut later = original.clone();
        later.sequence = 3;
        assert!(rebind_to_receiver(source, &tuple, &later).is_none());
    }

    #[test]
    fn sharing_receiver_control_refusals_answer_under_receiver_identity() {
        let tuple = tuple();
        for (code, retry, status) in [
            (
                SharedControlRefusalCode::StaleControl,
                None,
                StatusCode::CONFLICT,
            ),
            (
                SharedControlRefusalCode::OwnerChanged,
                None,
                StatusCode::CONFLICT,
            ),
            (
                SharedControlRefusalCode::RateLimited,
                Some(250),
                StatusCode::TOO_MANY_REQUESTS,
            ),
            (
                SharedControlRefusalCode::SessionEnded,
                None,
                StatusCode::GONE,
            ),
            (
                SharedControlRefusalCode::OwnerTransition,
                Some(500),
                StatusCode::TOO_EARLY,
            ),
            (SharedControlRefusalCode::OwnerLost, None, StatusCode::GONE),
            (
                SharedControlRefusalCode::Unavailable,
                Some(500),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ] {
            let response = refusal_response(
                &SharedControlRefusal {
                    code,
                    retry_after_ms: retry,
                },
                &tuple,
            );
            assert_eq!(response.status(), status, "{code:?}");
        }
    }
}
