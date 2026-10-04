//! Source `/vod-status` and `/control` adapters for one actual retained
//! viewer actor. Both take the original whole operation envelope and answer
//! with its exact echo. Neither serializes `VodSessionInfo`,
//! `LocalControlResult` or any numeric Source file identity.
use super::*;
use crate::playback_control::{
    ControlRequestV1, ControlResponseV1, ControlStateError, DeliveryView, EffectiveSelection,
    LocalControlResult, PlaybackLeaseView,
};
use plurx_core::domain::MediaSessionRoute;

/// The closed refusal vocabulary a Source may return for an authenticated
/// current-rendition exchange. B maps each one onto the ordinary client
/// control answer under its own session identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SharedControlRefusalCode {
    StaleControl,
    OwnerChanged,
    RateLimited,
    SessionEnded,
    OwnerTransition,
    OwnerLost,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SharedControlRefusal {
    pub(crate) code: SharedControlRefusalCode,
    pub(crate) retry_after_ms: Option<u32>,
}

impl SharedControlRefusal {
    pub(crate) fn from_state(error: ControlStateError) -> Self {
        use SharedControlRefusalCode as C;
        let (code, retry_after_ms) = match error {
            ControlStateError::StaleGeneration
            | ControlStateError::StaleClient
            | ControlStateError::StaleSequence => (C::StaleControl, None),
            ControlStateError::OwnerChanged => (C::OwnerChanged, None),
            ControlStateError::RateLimited(after) => (C::RateLimited, Some(after.clamp(1, 60_000))),
            // Shared controls are VOD current-rendition only; a rolling or
            // paused terminal on the Source is still an ended session to B.
            ControlStateError::SessionEnded
            | ControlStateError::RollingEnded(_)
            | ControlStateError::PauseExpired => (C::SessionEnded, None),
            ControlStateError::OwnerTransition => (C::OwnerTransition, Some(500)),
            ControlStateError::OwnerLost => (C::OwnerLost, None),
            ControlStateError::Unavailable => (C::Unavailable, Some(500)),
        };
        Self {
            code,
            retry_after_ms,
        }
    }
    /// A well-formed refusal carries a retry hint exactly when its code is
    /// one a client retries on the server's cadence.
    pub(crate) fn is_valid(&self) -> bool {
        use SharedControlRefusalCode as C;
        match self.code {
            C::RateLimited => self
                .retry_after_ms
                .is_some_and(|after| (1..=60_000).contains(&after)),
            C::OwnerTransition | C::Unavailable => self.retry_after_ms == Some(500),
            C::StaleControl | C::OwnerChanged | C::SessionEnded | C::OwnerLost => {
                self.retry_after_ms.is_none()
            }
        }
    }
}

/// Pure projection of one accepted Source exchange onto the ordinary control
/// response. Every field comes from the actual fenced route, the actual Start
/// envelope and the engine's own result. Anything the shared wire cannot
/// carry (a terminal acknowledgement, a preparation transaction or a
/// successor URL) refuses instead of being dropped.
pub(crate) fn source_control_response(
    route: &MediaSessionRoute,
    start: &super::super::hls::StartResponse,
    request: &ControlRequestV1,
    result: &LocalControlResult,
    server_time_unix_ms: i64,
) -> Result<ControlResponseV1, SharedControlRefusal> {
    let unavailable = SharedControlRefusal::from_state(ControlStateError::Unavailable);
    if result.lease_state != "active"
        || result.preparation_directive.is_some()
        || result.terminal_commit.is_some()
        || result.terminal_handoff.is_some()
        || matches!(
            result.action,
            crate::playback_control::ControlAction::Prepare { .. }
        )
    {
        return Err(unavailable);
    }
    let session: crate::transcode::SessionRequest =
        serde_json::from_str(&route.recipe_json).map_err(|_| unavailable.clone())?;
    let owner_epoch = u64::try_from(route.owner_epoch).map_err(|_| unavailable.clone())?;
    let delivery = DeliveryView::from_status(
        &result.status,
        request,
        &route.owner_node_id,
        owner_epoch,
        route.media_origin_ms,
        None,
    );
    let response = ControlResponseV1 {
        protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
        generation: route.incarnation_id.clone(),
        control_epoch: owner_epoch,
        accepted_sequence: result.accepted_sequence,
        server_time_unix_ms,
        lease: PlaybackLeaseView {
            state: "active".to_owned(),
            renew_after_ms: crate::playback_control::NEXT_EXCHANGE_MS,
            expires_at_unix_ms: result.lease_expires_at_unix_ms,
        },
        delivery,
        effective_selection: EffectiveSelection::from_request(
            &session,
            start.height,
            start.delivered_dynamic_range.clone(),
        ),
        action: crate::playback_control::ControlAction::None,
    };
    let action =
        crate::playback_control::resolve_action(&result.action, &response.delivery, request);
    if matches!(
        action,
        crate::playback_control::ControlAction::Prepare { .. }
    ) {
        return Err(unavailable);
    }
    Ok(ControlResponseV1 { action, ..response })
}

struct SourceControlInput {
    operation: SourceOperationInput,
    control: ControlRequestV1,
}

fn parse_control_request(
    bytes: &[u8],
    item: &str,
    file: &str,
    request: &str,
) -> Result<SourceControlInput, ApiError> {
    if bytes.is_empty() || bytes.len() > 128 * 1024 {
        return Err(invalid());
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let mut value = super::super::sharing_decision_decode::bounded_decision_value(&mut decoder)
        .map_err(|_| invalid())?;
    decoder.end().map_err(|_| invalid())?;
    let object = value.as_object_mut().ok_or_else(invalid)?;
    let control = object.remove("control").ok_or_else(invalid)?;
    let control: ControlRequestV1 = serde_json::from_value(control).map_err(|_| invalid())?;
    let operation = parse_operation_request(
        &serde_json::to_vec(&value).map_err(|_| invalid())?,
        item,
        file,
        request,
    )?;
    if operation.known.is_none() {
        return Err(invalid());
    }
    Ok(SourceControlInput { operation, control })
}

fn known_status_input(
    bytes: &[u8],
    item: &str,
    file: &str,
    request: &str,
) -> Result<SourceOperationInput, ApiError> {
    let input = parse_operation_request(bytes, item, file, request)?;
    if input.known.is_none() {
        return Err(invalid());
    }
    Ok(input)
}

fn echo(input: &SourceOperationInput) -> Result<Value, ApiError> {
    let known = input.known.as_ref().ok_or_else(invalid)?;
    Ok(serde_json::json!({
        "reference": input.start.reference,
        "request_id": input.start.request_id,
        "incarnation_id": known.incarnation_id,
        "session_id": known.session_id,
        "control_epoch": known.control_epoch,
    }))
}

/// Live VOD metrics for the exact retained session. A revoked grant gets no
/// observation; status alone never renews viewer activity.
pub(super) async fn vod_status(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    headers: HeaderMap,
    axum::extract::Path((item, file, request)): axum::extract::Path<(String, String, String)>,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    body: axum::body::Body,
) -> Result<axum::response::Response, ApiError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(9);
    let bytes = axum::body::to_bytes(body, 128 * 1024)
        .await
        .map_err(|_| invalid())?;
    let input = known_status_input(&bytes, &item, &file, &request)?;
    let viewer = viewer_hash(&headers)?;
    let (hash, grant) = current_reference(&state, &headers, &input.start.reference).await?;
    let entry = state
        .transcode
        .source_http_starts
        .current_entry(grant, &hash, &viewer, &input.start)
        .map_err(SourceStartFailure::response)?;
    entry
        .validate_known(input.known.as_ref())
        .map_err(SourceStartFailure::response)?;
    let owned = entry
        .wait(deadline)
        .await
        .map_err(SourceStartFailure::response)?;
    let opened = owned
        .actor
        .open_status(deadline)
        .await
        .map_err(|e| SourceStartFailure::from(e).response())?;
    let (_, current_grant) = current_reference(&state, &headers, &input.start.reference).await?;
    if current_grant != grant {
        return Err(unavailable());
    }
    let (status, guard) = opened.into_parts();
    let mut value = echo(&input)?;
    value["status"] = serde_json::to_value(&status).map_err(|_| unavailable())?;
    let response =
        super::super::shared_library::source_file_json(grant, &input.start.reference, value)?;
    Ok(super::super::shared_library::guard_source_response(
        state,
        connection.map(|c| c.0),
        hold_start_body(response, guard),
    )
    .await)
}

/// One current-rendition control exchange. The owned task finishes an
/// exchange the actor has begun even if this HTTP waiter disappears, so a
/// lost response can only ever be answered by the same-sequence replay.
pub(super) async fn control(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    headers: HeaderMap,
    axum::extract::Path((item, file, request)): axum::extract::Path<(String, String, String)>,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    body: axum::body::Body,
) -> Result<axum::response::Response, ApiError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let bytes = axum::body::to_bytes(body, 128 * 1024)
        .await
        .map_err(|_| invalid())?;
    let input = parse_control_request(&bytes, &item, &file, &request)?;
    let viewer = viewer_hash(&headers)?;
    let (hash, grant) =
        current_reference(&state, &headers, &input.operation.start.reference).await?;
    let entry = state
        .transcode
        .source_http_starts
        .current_entry(grant, &hash, &viewer, &input.operation.start)
        .map_err(SourceStartFailure::response)?;
    entry
        .validate_known(input.operation.known.as_ref())
        .map_err(SourceStartFailure::response)?;
    let owned = entry
        .wait(deadline)
        .await
        .map_err(SourceStartFailure::response)?;
    let actor = owned.actor.clone();
    let control = input.control.clone();
    let opened = tokio::spawn(async move { actor.control(control, deadline).await })
        .await
        .map_err(|_| unavailable())?
        .map_err(|e| SourceStartFailure::from(e).response())?;
    let (_, current_grant) =
        current_reference(&state, &headers, &input.operation.start.reference).await?;
    if current_grant != grant {
        return Err(unavailable());
    }
    let parts = opened.into_wire_parts();
    let projected = parts
        .result
        .map_err(SharedControlRefusal::from_state)
        .and_then(|result| {
            source_control_response(
                &parts.route,
                &parts.start,
                &input.control,
                &result,
                crate::media_sessions::unix_ms(),
            )
        });
    let mut value = echo(&input.operation)?;
    match projected {
        Ok(response) => {
            value["response"] = serde_json::to_value(&response).map_err(|_| unavailable())?
        }
        Err(refusal) => {
            value["refusal"] = serde_json::to_value(&refusal).map_err(|_| unavailable())?
        }
    }
    let response = super::super::shared_library::source_file_json(
        grant,
        &input.operation.start.reference,
        value,
    )?;
    Ok(super::super::shared_library::guard_source_response(
        state,
        connection.map(|c| c.0),
        hold_start_body(response, parts.guard),
    )
    .await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharing_source_control_refusals_are_closed_and_carry_exact_retry_hints() {
        use ControlStateError as E;
        for (error, code, retry) in [
            (
                E::StaleGeneration,
                SharedControlRefusalCode::StaleControl,
                None,
            ),
            (E::StaleClient, SharedControlRefusalCode::StaleControl, None),
            (
                E::StaleSequence,
                SharedControlRefusalCode::StaleControl,
                None,
            ),
            (
                E::OwnerChanged,
                SharedControlRefusalCode::OwnerChanged,
                None,
            ),
            (
                E::RateLimited(0),
                SharedControlRefusalCode::RateLimited,
                Some(1),
            ),
            (
                E::RateLimited(u32::MAX),
                SharedControlRefusalCode::RateLimited,
                Some(60_000),
            ),
            (
                E::SessionEnded,
                SharedControlRefusalCode::SessionEnded,
                None,
            ),
            (
                E::PauseExpired,
                SharedControlRefusalCode::SessionEnded,
                None,
            ),
            (
                E::OwnerTransition,
                SharedControlRefusalCode::OwnerTransition,
                Some(500),
            ),
            (E::OwnerLost, SharedControlRefusalCode::OwnerLost, None),
            (
                E::Unavailable,
                SharedControlRefusalCode::Unavailable,
                Some(500),
            ),
        ] {
            let refusal = SharedControlRefusal::from_state(error);
            assert_eq!(refusal.code, code, "{error:?}");
            assert_eq!(refusal.retry_after_ms, retry, "{error:?}");
            assert!(refusal.is_valid(), "{error:?}");
        }
        for (code, retry) in [
            (SharedControlRefusalCode::StaleControl, Some(500)),
            (SharedControlRefusalCode::RateLimited, None),
            (SharedControlRefusalCode::RateLimited, Some(60_001)),
            (SharedControlRefusalCode::Unavailable, None),
            (SharedControlRefusalCode::OwnerTransition, Some(1)),
        ] {
            assert!(!SharedControlRefusal {
                code,
                retry_after_ms: retry
            }
            .is_valid());
        }
        let wire: serde_json::Value =
            serde_json::to_value(SharedControlRefusal::from_state(E::RateLimited(250)))
                .expect("refusal wire");
        assert_eq!(
            wire,
            serde_json::json!({"code":"rate_limited","retry_after_ms":250})
        );
        assert!(serde_json::from_value::<SharedControlRefusal>(
            serde_json::json!({"code":"rate_limited","retry_after_ms":250,"message":"raw"})
        )
        .is_err());
    }

    async fn post(url: &str, headers: &HeaderMap, body: &Value) -> (StatusCode, Vec<u8>) {
        let response = reqwest::Client::builder()
            .http1_only()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("H1 client")
            .post(url)
            .headers(headers.clone())
            .header("connection", "close")
            .body(serde_json::to_vec(body).expect("request"))
            .send()
            .await
            .expect("actual H1 response");
        let status = response.status();
        (status, response.bytes().await.expect("bytes").to_vec())
    }

    /// The Copy fixture asks `copy:true, height:72, quality_auto:false`, so
    /// its frozen desired selection is Original with no audio or subtitles.
    fn original_selection() -> crate::playback_control::ClientSelection {
        use crate::playback_control as pc;
        pc::ClientSelection {
            quality: pc::QualitySelection::Original,
            audio_track: None,
            subtitle: pc::SubtitleSelection {
                mode: pc::SubtitleMode::Off,
                track: None,
            },
            audio_offset_ms: 0,
            codec: pc::CodecPolicy::Auto,
            dynamic_range: pc::DynamicRangePolicy::Auto,
        }
    }

    fn control_request(generation: &str, epoch: u64, sequence: u64) -> ControlRequestV1 {
        use crate::playback_control as pc;
        ControlRequestV1 {
            intent: None,
            protocol: pc::PROTOCOL_V1.to_owned(),
            generation: generation.to_owned(),
            control_epoch: epoch,
            client_instance_id: "6f1c2d1e-7f9a-4b8e-9d3c-2a1b0c9d8e7f".to_owned(),
            sequence,
            demand: pc::PlaybackDemand::Active,
            position_ms: 0,
            buffered_from_ms: Some(0),
            buffered_through_ms: 1_000,
            playback_rate: 1.0,
            render_state: pc::RenderState::Rendering,
            seek_target_ms: None,
            observed_download_bps: None,
            selection: original_selection(),
            capabilities: None,
            observation: None,
            acknowledgement: None,
            supported_actions: Some(vec!["hold".to_owned(), "retry_resource".to_owned()]),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sharing_source_http_vod_status_and_control_echo_exact_lineage_on_actual_actor() {
        Box::pin(actual_source_status_and_control()).await;
    }
    async fn actual_source_status_and_control() {
        use std::time::{Duration, Instant};
        let fixture = real_source_start_fixture_with(SourceFixtureMode::Copy, None).await;
        let response = start(
            axum::extract::State((*fixture.state).clone()),
            fixture.headers.clone(),
            axum::extract::Path((
                fixture.reference.item_id.as_str().to_owned(),
                fixture.reference.file_id.as_str().to_owned(),
            )),
            axum::body::Body::from(fixture.request.clone()),
        )
        .await
        .expect("actual Start");
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("full DTO");
        let decoded = super::super::super::decode_source_start_response(&bytes, &fixture.reference)
            .expect("strict actual Start");
        let incarnation = decoded.incarnation_id();
        let source_response = decoded.response().clone();
        let epoch = source_response
            .control
            .as_ref()
            .expect("control bootstrap")
            .control_epoch;
        let mut recipe: Value = serde_json::from_slice(&fixture.request).expect("recipe");
        let request = recipe["session"]["request_id"]
            .as_str()
            .expect("request")
            .to_owned();
        recipe["incarnation_id"] = serde_json::json!(incarnation);
        recipe["session_id"] = serde_json::json!(source_response.session_id);
        recipe["control_epoch"] = serde_json::json!(epoch);
        // The B client's own strict parsers judge every reply below.
        let session = crate::sharing_client::SourcePeerSession::new(
            fixture.reference.clone(),
            &fixture.request,
        )
        .expect("retained B session");
        let known =
            crate::sharing_client::SourcePeerLineage::from_start(incarnation, &source_response)
                .expect("known lineage");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(crate::serve_http(
            listener,
            super::super::super::sharing::peer_router((*fixture.state).clone()),
            async move {
                let _ = stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        let base = format!(
            "http://{address}/sharing/v1/items/{}/files/{}/sessions/{request}",
            fixture.reference.item_id.as_str(),
            fixture.reference.file_id.as_str()
        );
        let touched = || async {
            fixture
                .state
                .transcode
                .vod_for_test()
                .source_control_observation_for_test(&source_response.session_id)
                .await
                .expect("actual VOD session")
        };

        // Status: exact echo, closed metrics, no activity mutation.
        let before = touched().await;
        let (status, bytes) = post(&format!("{base}/vod-status"), &fixture.headers, &recipe).await;
        assert_eq!(status, StatusCode::OK);
        let value: Value = serde_json::from_slice(&bytes).expect("status JSON");
        for forbidden in ["id", "file_id", "producer_failed"] {
            assert!(value["status"].get(forbidden).is_none(), "{forbidden}");
        }
        crate::sharing_client::SourceVodStatusReceipt::parse(&bytes, &session, &known)
            .expect("B accepts the actual Source observation");
        assert_eq!(
            touched().await,
            before,
            "status alone never renews activity"
        );
        let mut foreign = recipe.clone();
        foreign["session_id"] = serde_json::json!(Uuid::new_v4());
        assert_eq!(
            post(&format!("{base}/vod-status"), &fixture.headers, &foreign)
                .await
                .0,
            StatusCode::CONFLICT
        );
        let mut unknown = recipe.clone();
        unknown
            .as_object_mut()
            .expect("object")
            .remove("session_id");
        unknown
            .as_object_mut()
            .expect("object")
            .remove("incarnation_id");
        unknown
            .as_object_mut()
            .expect("object")
            .remove("control_epoch");
        assert_eq!(
            post(&format!("{base}/vod-status"), &fixture.headers, &unknown)
                .await
                .0,
            StatusCode::BAD_REQUEST,
            "observation requires the exact known lineage"
        );

        // Control: accepted on the actual actor, then exact replay.
        let control = control_request(&incarnation.to_string(), epoch, 1);
        let mut body = recipe.clone();
        body["control"] = serde_json::to_value(&control).expect("control");
        let (status, bytes) = post(&format!("{base}/control"), &fixture.headers, &body).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let accepted =
            crate::sharing_client::SourceControlReceipt::parse(&bytes, &session, &known, &control)
                .expect("B accepts the actual Source answer")
                .into_outcome()
                .expect("accepted");
        assert_eq!(accepted.accepted_sequence, 1);
        assert_eq!(accepted.generation, incarnation.to_string());
        let value: Value = serde_json::from_slice(&bytes).expect("control JSON");
        assert!(value.get("refusal").is_none());
        let after_accept = touched().await;
        assert!(after_accept.1 >= before.1);
        let (status, replay) = post(&format!("{base}/control"), &fixture.headers, &body).await;
        assert_eq!(status, StatusCode::OK);
        let replayed =
            crate::sharing_client::SourceControlReceipt::parse(&replay, &session, &known, &control)
                .expect("replay parses")
                .into_outcome()
                .expect("replay accepted");
        assert_eq!(replayed.accepted_sequence, 1);

        // A later sequence, then the earlier one is stale.
        let later = control_request(&incarnation.to_string(), epoch, 2);
        body["control"] = serde_json::to_value(&later).expect("control");
        assert_eq!(
            post(&format!("{base}/control"), &fixture.headers, &body)
                .await
                .0,
            StatusCode::OK
        );
        let stale = control_request(&incarnation.to_string(), epoch, 1);
        let mut other_client = stale.clone();
        other_client.client_instance_id = "0b8c1f52-3c55-4f0d-8a1e-5e9f6d7c8b9a".to_owned();
        body["control"] = serde_json::to_value(&other_client).expect("control");
        let (status, bytes) = post(&format!("{base}/control"), &fixture.headers, &body).await;
        assert_eq!(status, StatusCode::OK);
        let refusal = crate::sharing_client::SourceControlReceipt::parse(
            &bytes,
            &session,
            &known,
            &other_client,
        )
        .expect("closed refusal")
        .into_outcome()
        .expect_err("stale");
        assert_eq!(refusal.code, SharedControlRefusalCode::StaleControl);

        // A changed selection is not a current-rendition control.
        let mut manual = control_request(&incarnation.to_string(), epoch, 3);
        manual.selection.quality = crate::playback_control::QualitySelection::Manual { height: 72 };
        body["control"] = serde_json::to_value(&manual).expect("control");
        assert_eq!(
            post(&format!("{base}/control"), &fixture.headers, &body)
                .await
                .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        // A foreign Source generation conflicts before any authority extension.
        let foreign_generation = control_request(&Uuid::new_v4().to_string(), epoch, 3);
        body["control"] = serde_json::to_value(&foreign_generation).expect("control");
        assert_eq!(
            post(&format!("{base}/control"), &fixture.headers, &body)
                .await
                .0,
            StatusCode::CONFLICT
        );

        let entry = fixture
            .state
            .transcode
            .source_http_starts
            .entries
            .lock()
            .expect("entry")
            .first()
            .cloned()
            .expect("entry");
        let owned = entry
            .wait(Instant::now() + Duration::from_secs(1))
            .await
            .expect("owner");
        tokio::time::timeout(Duration::from_secs(15), owned.actor.retire())
            .await
            .expect("actual bodies/writers settle")
            .expect("retirement");
        assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
        let _ = stop.send(());
        server.await.expect("server").expect("shutdown");
        drop(owned);
        fixture.shutdown().await;
    }

    #[test]
    fn sharing_source_control_request_requires_known_lineage_and_closed_control() {
        let parsed = parse_control_request(b"{}", "1", "2", &Uuid::new_v4().to_string());
        assert!(parsed.is_err());
        let foreign = serde_json::json!({"control":{"protocol":"x","unexpected":1}});
        assert!(parse_control_request(
            &serde_json::to_vec(&foreign).expect("wire"),
            "1",
            "2",
            &Uuid::new_v4().to_string()
        )
        .is_err());
    }
}
