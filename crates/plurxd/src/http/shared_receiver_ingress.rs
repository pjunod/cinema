//! Authenticated Shared Start on the public media router. The receiver actor
//! owns dispatch, persistence and physical cleanup.
use super::{
    error::ApiError,
    extract::{AuthUser, RawToken},
    hls::CreateSession,
    shared_receiver_playback::ReceiverStartError,
};
use crate::state::AppState;
use axum::{
    body::{to_bytes, Body},
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use plurx_core::{
    sharing_file_locators::FileLocatorReference,
    sharing_receiver_sessions::{ReceiverProducerKind, ReceiverSessionIntent, RemoteSourceRecipe},
};
use serde_json::Value;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

/// Installed on the public media group. The real pinned B-to-Source fixture
/// qualifies relay, status, control, delivery grant and confirmed End.
pub(crate) fn start_router() -> Router<AppState> {
    Router::new()
        .route(
            "/shared/imports/{import}/files/{locator}/hls/sessions",
            post(start),
        )
        .route(
            "/shared/imports/{import}/files/{locator}/playback",
            post(start),
        )
}
fn invalid() -> ApiError {
    ApiError::typed(
        StatusCode::BAD_REQUEST,
        "sharing_invalid_request",
        "Invalid shared start request",
    )
}
fn unavailable() -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "sharing_start_unavailable",
        "Shared playback authority is unavailable",
    )
}
fn actor_error(error: ReceiverStartError) -> ApiError {
    let (status, code) = match error {
        ReceiverStartError::Unsupported => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "sharing_start_unsupported",
        ),
        ReceiverStartError::Conflict => (StatusCode::CONFLICT, "sharing_start_conflict"),
        ReceiverStartError::Capacity => (StatusCode::TOO_MANY_REQUESTS, "sharing_start_capacity"),
        ReceiverStartError::Deadline => (StatusCode::SERVICE_UNAVAILABLE, "sharing_start_deadline"),
        ReceiverStartError::Unresolved => {
            (StatusCode::SERVICE_UNAVAILABLE, "sharing_start_unresolved")
        }
        ReceiverStartError::Unavailable => {
            (StatusCode::SERVICE_UNAVAILABLE, "sharing_start_unavailable")
        }
    };
    ApiError::typed(status, code, "Shared playback is unavailable")
}
struct RetainedRequest {
    value: Value,
    request_id: String,
    playback_id: String,
}
fn provided_fields(input: &Value, typed: &Value) -> bool {
    match (input, typed) {
        (Value::Object(a), Value::Object(b)) => a
            .iter()
            .all(|(k, v)| b.get(k).is_some_and(|t| provided_fields(v, t))),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| provided_fields(a, b))
        }
        (Value::Number(a), Value::Number(b)) if b.is_f64() => a.as_f64() == b.as_f64(),
        _ => input == typed,
    }
}
fn parse_request(bytes: &[u8]) -> Result<RetainedRequest, ApiError> {
    if bytes.is_empty() || bytes.len() > 128 * 1024 {
        return Err(invalid());
    }
    let mut json = serde_json::Deserializer::from_slice(bytes);
    let value =
        super::sharing_decision_decode::bounded_decision_value(&mut json).map_err(|_| invalid())?;
    json.end().map_err(|_| invalid())?;
    if !value.is_object() || serde_json::to_vec(&value).map_err(|_| invalid())?.len() > 24 * 1024 {
        return Err(invalid());
    }
    let typed: CreateSession = serde_json::from_value(value.clone()).map_err(|_| invalid())?;
    if !provided_fields(
        &value,
        &serde_json::to_value(&typed).map_err(|_| invalid())?,
    ) {
        return Err(invalid());
    }
    let request_id = typed.request_id.ok_or_else(invalid)?;
    let id = Uuid::parse_str(&request_id).map_err(|_| invalid())?;
    if id.get_version_num() != 4
        || id.get_variant() != uuid::Variant::RFC4122
        || id.to_string() != request_id
        || typed.playback_id.is_empty()
        || typed.playback_id.len() > 128
        || typed.playback_id.chars().any(char::is_control)
    {
        return Err(invalid());
    }
    let caps = typed.caps.ok_or_else(invalid)?;
    super::stream::validate_device_caps(&caps)?;
    if caps.v != plurx_core::playback::DeviceCaps::VERSION || caps.is_empty() {
        return Err(invalid());
    }
    Ok(RetainedRequest {
        value,
        request_id,
        playback_id: typed.playback_id,
    })
}
/// Initial play/resume only until the B-to-Source control translation is qualified.
/// Null is preserved as supplied; non-null controls are refused, never erased.
fn initial_request(request: &RetainedRequest) -> Result<(), ApiError> {
    if [
        "previous_session_id",
        "control_sequence",
        "reopen_reason",
        "intent",
    ]
    .iter()
    .any(|key| request.value.get(key).is_some_and(|value| !value.is_null()))
    {
        return Err(invalid());
    }
    Ok(())
}
fn prepare(
    scope: plurx_core::store::sharing_catalogue::ReceiverCatalogueScope,
    reference: FileLocatorReference,
    user: i64,
    login_hash: String,
    request: &RetainedRequest,
) -> Result<(ReceiverSessionIntent, String), ApiError> {
    initial_request(request)?;
    if user <= 0
        || reference.item.import_id != scope.import_id
        || reference.item.server_id != scope.source_server_id
        || reference.item.catalogue_epoch != scope.catalogue_epoch
        || reference.lifecycle_generation != scope.lifecycle_generation
        || scope.libraries.as_slice() != [reference.item.library_id.clone()]
    {
        return Err(invalid());
    }
    let source_request_id = Uuid::new_v4();
    let recipe = RemoteSourceRecipe {
        kind: ReceiverProducerKind::RemoteSource,
        version: 1,
        reference: reference.item.clone(),
        lifecycle_generation: reference.lifecycle_generation,
        file_id: reference.file_id.clone(),
        file_revision: reference.revision.clone(),
        source_request_id,
        parent_login_hash: login_hash.clone(),
        request_json: request.value.to_string(),
    };
    let intent = ReceiverSessionIntent {
        scope,
        user_id: user,
        login_hash,
        recipe,
        source_position_ms: 0,
    };
    let wrapper = crate::sharing::receiver_source_wrapper(&intent.recipe).map_err(|_| invalid())?;
    crate::sharing::receiver_source_request(&intent, &wrapper).map_err(|_| invalid())?;
    Ok((intent, wrapper))
}
async fn start(
    AuthUser(user): AuthUser,
    RawToken(token): RawToken,
    State(state): State<AppState>,
    Path((import, locator)): Path<(String, String)>,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    body: Body,
) -> Result<Response, ApiError> {
    let import_id = Uuid::parse_str(&import).map_err(|_| invalid())?;
    if import_id.is_nil() || import_id.to_string() != import {
        return Err(invalid());
    }
    let import = state
        .store
        .sharing_import(import_id)
        .await?
        .ok_or_else(unavailable)?;
    if import.summary.state != "active" || !crate::sharing::enabled(state.store.as_ref()).await? {
        return Err(unavailable());
    }
    let key = super::shared_artwork::receiver_key(&state)
        .await?
        .ok_or_else(unavailable)?;
    let reference = key
        .verify(&locator, import_id, import.summary.lifecycle_generation)
        .map_err(|_| invalid())?;
    let bytes = tokio::time::timeout(Duration::from_secs(15), to_bytes(body, 128 * 1024))
        .await
        .map_err(|_| invalid())?
        .map_err(|_| invalid())?;
    let request = parse_request(&bytes)?;
    let summary = &import.summary;
    let scope = plurx_core::store::sharing_catalogue::ReceiverCatalogueScope {
        import_id: summary.id,
        source_server_id: summary.source_server_id,
        catalogue_epoch: summary.catalogue_epoch,
        lifecycle_generation: summary.lifecycle_generation,
        assignment_generation: summary.assignment_generation,
        endpoint_generation: summary.endpoint_generation,
        claim_id: summary.claim_id,
        remote_grant_id: summary.remote_grant_id.ok_or_else(unavailable)?,
        libraries: vec![reference.item.library_id.clone()],
    };
    let (intent, wrapper) = prepare(
        scope,
        reference,
        user.id,
        plurx_core::auth::hash_token(&token),
        &request,
    )?;
    // AuthUser can be revoked while the body is being read. This fresh opaque
    // proof is B metadata authority only; Source checks revision before work.
    let _authority = state
        .store
        .prepare_receiver_session_authority(intent.clone())
        .await?
        .ok_or_else(unavailable)?;
    let actor = state
        .sharing
        .receiver_starts
        .begin(
            Arc::new(state.clone()),
            intent,
            request.request_id,
            request.playback_id,
            wrapper,
        )
        .map_err(actor_error)?;
    let response = actor
        .wait_ready(Instant::now() + Duration::from_secs(305))
        .await
        .map_err(actor_error)?;
    let connection = connection.ok_or_else(unavailable)?;
    actor
        .protect_start_response(
            Arc::new(state),
            &connection.0,
            ([(header::CACHE_CONTROL, "no-store")], Json(response)).into_response(),
        )
        .await
        .map_err(actor_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::{
        sharing::SourceId, sharing_catalogue::SharedReference,
        sharing_catalogue_details::FileRevision, store::sharing_catalogue::ReceiverCatalogueScope,
    };
    use tower::ServiceExt;
    fn request() -> Value {
        serde_json::json!({"playback_id":"actual-client-instance","request_id":Uuid::new_v4(),"start":17,"copy":true,"height":1080,"quality_auto":false,"presentation":"vod","transport":"native","subtitle":null,"audio":1,"audio_offset_ms":250,"block_budget_secs":12,"preserve_dolby_vision":false,"hdr10":false,"native_subtitles":true,"previous_session_id":Uuid::new_v4(),"reopen_reason":"stall","control_sequence":9,"caps":{"v":2,"video":[{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"]}})
    }
    fn context() -> (ReceiverCatalogueScope, FileLocatorReference) {
        let source = Uuid::new_v4();
        let epoch = Uuid::new_v4();
        let import = Uuid::new_v4();
        let library = SourceId::parse("0").expect("zero");
        let reference = FileLocatorReference {
            item: SharedReference {
                import_id: import,
                server_id: source,
                catalogue_epoch: epoch,
                library_id: library.clone(),
                item_id: SourceId::parse("9007199254740993").expect("lossless"),
            },
            lifecycle_generation: 3,
            file_id: SourceId::parse("9223372036854775807").expect("lossless"),
            revision: FileRevision::parse(&"a".repeat(64)).expect("revision"),
        };
        let scope = ReceiverCatalogueScope {
            import_id: import,
            source_server_id: source,
            catalogue_epoch: epoch,
            lifecycle_generation: 3,
            assignment_generation: 2,
            endpoint_generation: 1,
            claim_id: Uuid::new_v4(),
            remote_grant_id: Uuid::new_v4(),
            libraries: vec![library],
        };
        (scope, reference)
    }
    #[test]
    fn sharing_receiver_ingress_preserves_complete_original_request_and_lossless_source_context() {
        let mut value = request();
        for key in [
            "previous_session_id",
            "control_sequence",
            "reopen_reason",
            "intent",
        ] {
            value[key] = Value::Null;
        }
        let request =
            parse_request(&serde_json::to_vec(&value).expect("encode")).expect("whole DTO");
        let (scope, reference) = context();
        let (intent, wrapper) = prepare(
            scope.clone(),
            reference.clone(),
            7,
            "b".repeat(64),
            &request,
        )
        .expect("captured context");
        assert_eq!(intent.recipe.request_json, value.to_string());
        assert_eq!(intent.source_position_ms, 0);
        assert!(request.value["start"].is_i64());
        assert!(request.value.get("subtitle").is_some_and(Value::is_null));
        assert!(request.value.get("aac").is_none());
        let source: Value = serde_json::from_str(&wrapper).expect("wrapper");
        let mut expected = value.clone();
        expected["request_id"] = intent.recipe.source_request_id.to_string().into();
        assert_eq!(source["session"], expected);
        assert_eq!(source["reference"]["library_id"], "0");
        assert_eq!(source["reference"]["file_id"], "9223372036854775807");
        assert_ne!(
            intent.recipe.source_request_id.to_string(),
            request.request_id
        );
        let (retry, _) = prepare(
            scope.clone(),
            reference.clone(),
            7,
            "b".repeat(64),
            &request,
        )
        .expect("retry context");
        assert_ne!(
            retry.recipe.source_request_id,
            intent.recipe.source_request_id
        );
        assert_eq!(
            intent.recipe.request_fingerprint().expect("identity"),
            retry.recipe.request_fingerprint().expect("identity")
        );
        let mut wrong = reference.clone();
        wrong.item.server_id = Uuid::new_v4();
        assert!(prepare(scope.clone(), wrong, 7, "b".repeat(64), &request).is_err());
        let mut wrong = reference.clone();
        wrong.lifecycle_generation += 1;
        assert!(prepare(scope, wrong, 7, "b".repeat(64), &request).is_err());
    }
    #[test]
    fn receiver_source_wrapper_matches_ingress_prepare() {
        let mut value = request();
        for key in [
            "previous_session_id",
            "control_sequence",
            "reopen_reason",
            "intent",
        ] {
            value[key] = Value::Null;
        }
        let request =
            parse_request(&serde_json::to_vec(&value).expect("encode")).expect("whole DTO");
        let (scope, reference) = context();
        let (intent, wrapper) = prepare(scope, reference.clone(), 7, "b".repeat(64), &request)
            .expect("captured context");
        // Independent construction from the live client request, as ingress
        // built it before the wrapper was shared with crash recovery.
        let target = super::super::hls::SourcePlaybackTarget {
            server_id: reference.item.server_id,
            catalogue_epoch: reference.item.catalogue_epoch,
            library_id: reference.item.library_id.clone(),
            item_id: reference.item.item_id.clone(),
            file_id: reference.file_id.clone(),
            revision: reference.revision.clone(),
        };
        let mut session = request.value.clone();
        session["request_id"] = intent.recipe.source_request_id.to_string().into();
        let original = serde_json::json!({"reference":target,"session":session}).to_string();
        assert_eq!(wrapper, original);
        // Recovery has only the durable recipe; it rebuilds the same bytes.
        let durable: RemoteSourceRecipe =
            serde_json::from_str(&serde_json::to_string(&intent.recipe).expect("persist"))
                .expect("durable recipe");
        assert_eq!(
            crate::sharing::receiver_source_wrapper(&durable).expect("recovered wrapper"),
            original
        );
        let target = crate::sharing::receiver_source_request(&intent, &original)
            .expect("exact private Source request");
        assert!(crate::sharing_client::SourcePeerSession::new(target, original.as_bytes()).is_ok());
    }
    #[test]
    fn sharing_receiver_ingress_refuses_recovery_controls_without_erasing_intent() {
        let value = request();
        let retained = parse_request(&serde_json::to_vec(&value).expect("encode"))
            .expect("complete ordinary DTO remains parseable");
        assert_eq!(retained.value, value);
        let (scope, reference) = context();
        assert!(prepare(scope, reference, 7, "b".repeat(64), &retained).is_err());
        for (key, unsupported) in [
            ("previous_session_id", Value::String(String::new())),
            ("control_sequence", Value::from(0)),
            ("reopen_reason", Value::String("stall".into())),
            (
                "intent",
                serde_json::to_value(plurx_core::playback::MediaIntentEnvelope {
                    lifetime_id: "actual-player-lifetime".into(),
                    recipe_revision: 1,
                    destination_revision: 1,
                    transport_revision: 1,
                    selection: plurx_core::playback::DesiredSelection {
                        quality: plurx_core::playback::DesiredQuality::Auto {
                            height: None,
                            candidate_id: None,
                        },
                        codec: plurx_core::playback::DesiredCodec::Auto,
                        dynamic_range: plurx_core::playback::DesiredDynamicRange::Auto,
                        audio_track: None,
                        audio_offset_ms: 0,
                        subtitles: plurx_core::playback::DesiredSubtitles::Off,
                    },
                })
                .expect("whole intent"),
            ),
        ] {
            let mut initial = value.clone();
            for control in [
                "previous_session_id",
                "control_sequence",
                "reopen_reason",
                "intent",
            ] {
                initial.as_object_mut().expect("object").remove(control);
            }
            initial[key] = unsupported;
            let retained = parse_request(&serde_json::to_vec(&initial).expect("encode"))
                .expect("well-typed control");
            assert!(initial_request(&retained).is_err(), "{key}");
            assert_eq!(retained.value, initial, "refusal must not rewrite intent");
            initial[key] = Value::Null;
            let retained = parse_request(&serde_json::to_vec(&initial).expect("encode"))
                .expect("null control");
            assert!(initial_request(&retained).is_ok());
            assert_eq!(retained.value["start"], 17);
            assert_eq!(retained.value["height"], 1080);
        }
    }
    #[test]
    fn sharing_receiver_ingress_refuses_duplicate_unknown_and_oversized_start_shapes() {
        let value = request();
        for field in ["source_url", "file_id", "queue", "viewer"] {
            let mut wrong = value.clone();
            wrong[field] = true.into();
            assert!(
                parse_request(&serde_json::to_vec(&wrong).expect("encode")).is_err(),
                "{field}"
            );
        }
        let mut wrong = value.clone();
        wrong["caps"]["video"][0]["unknown"] = true.into();
        assert!(parse_request(&serde_json::to_vec(&wrong).expect("encode")).is_err());
        for bad in [
            Value::Null,
            Value::String("00000000-0000-0000-0000-000000000000".into()),
            Value::String(Uuid::new_v4().to_string().to_uppercase()),
        ] {
            let mut wrong = value.clone();
            wrong["request_id"] = bad;
            assert!(parse_request(&serde_json::to_vec(&wrong).expect("encode")).is_err());
        }
        let mut wrong = value.clone();
        wrong.as_object_mut().expect("object").remove("caps");
        assert!(parse_request(&serde_json::to_vec(&wrong).expect("encode")).is_err());
        let mut wrong = value.clone();
        wrong["playback_id"] = "x".repeat(25 * 1024).into();
        assert!(parse_request(&serde_json::to_vec(&wrong).expect("encode")).is_err());
        let encoded = value.to_string();
        let duplicate = format!("{{\"request_id\":\"{}\",{}", Uuid::new_v4(), &encoded[1..]);
        assert!(parse_request(duplicate.as_bytes()).is_err());
        let trailing = format!("{encoded} {{}}");
        assert!(parse_request(trailing.as_bytes()).is_err());
        assert!(parse_request(&vec![b' '; 128 * 1024 + 1]).is_err());
    }
    #[tokio::test]
    async fn sharing_receiver_ingress_aliases_require_account_auth_on_the_public_router() {
        let state = super::super::source_actor_test_state();
        let user = state
            .store
            .create_user("ingress-account", "fixture-hash", false)
            .await
            .expect("real B account");
        let token = "ingress-original-login";
        state
            .store
            .create_token(&plurx_core::auth::hash_token(token), user.id, None)
            .await
            .expect("actual login");
        let app = start_router().with_state(state.clone());
        let prefix = format!("/shared/imports/{}/files/unsigned", Uuid::new_v4());
        for suffix in ["hls/sessions", "playback"] {
            let request = axum::http::Request::builder()
                .method("POST")
                .uri(format!("{prefix}/{suffix}?session={}", Uuid::new_v4()))
                .body(Body::empty())
                .expect("request");
            assert_eq!(
                app.clone()
                    .oneshot(request)
                    .await
                    .expect("response")
                    .status(),
                StatusCode::UNAUTHORIZED
            );
            let request = axum::http::Request::builder()
                .method("POST")
                .uri(format!("{prefix}/{suffix}"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request");
            assert_eq!(
                app.clone()
                    .oneshot(request)
                    .await
                    .expect("response")
                    .status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "authenticated missing import has no Source work"
            );
            let request = axum::http::Request::builder()
                .method("POST")
                .uri(format!("/api/v1{prefix}/{suffix}"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request");
            assert_eq!(
                super::super::router(state.clone())
                    .oneshot(request)
                    .await
                    .expect("public response")
                    .status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "the public route is installed and finds no import"
            );
            let request = axum::http::Request::builder()
                .method("POST")
                .uri(format!("/api/v1{prefix}/{suffix}"))
                .body(Body::empty())
                .expect("request");
            assert_eq!(
                super::super::router(state.clone())
                    .oneshot(request)
                    .await
                    .expect("public response")
                    .status(),
                StatusCode::UNAUTHORIZED
            );
        }
    }
}
