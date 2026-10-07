//! B's direct-play relay. A shared direct session is admitted only by the
//! authenticated Shared start; `{file_base}/direct` serves bytes only for the
//! exact B session bound to that file, login and delivery grant. Range and
//! HEAD only look sessions up; nothing here admits or starts a Source.
use super::*;
use crate::http::sharing_direct_wire::{
    DirectByteRequest, DirectRequestRefusal, SharedDirectStart, SourceDirectStart,
};
use axum::{
    extract::{FromRequestParts, Path, State},
    http::{request::Parts, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};

/// The viewer-facing direct reply: B's signed file alias plus the bounded
/// B session binding. The alias is re-issued from the retained recipe, so it
/// is the exact locator the viewer's details carried.
pub(super) async fn project_direct_start(
    state: &AppState,
    intent: &ReceiverSessionIntent,
    session: Uuid,
    direct: &SourceDirectStart,
) -> Result<SharedDirectStart, ReceiverStartError> {
    let key = super::super::shared_artwork::receiver_key(state)
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        .ok_or(ReceiverStartError::Unresolved)?;
    let recipe = &intent.recipe;
    let locator = key
        .issue(&plurx_core::sharing_file_locators::FileLocatorReference {
            item: recipe.reference.clone(),
            lifecycle_generation: recipe.lifecycle_generation,
            file_id: recipe.file_id.clone(),
            revision: recipe.file_revision.clone(),
        })
        .map_err(|_| ReceiverStartError::Unresolved)?;
    SharedDirectStart::new(&locator.file_base(), session, direct)
        .map_err(|_| ReceiverStartError::Unresolved)
}

impl ReceiverStartActor {
    /// The retained recipe names exactly this verified file alias.
    fn direct_binds(
        &self,
        reference: &plurx_core::sharing_file_locators::FileLocatorReference,
    ) -> bool {
        let recipe = &self.0.intent.recipe;
        self.0.direct
            && recipe.reference == reference.item
            && recipe.lifecycle_generation == reference.lifecycle_generation
            && recipe.file_id == reference.file_id
            && recipe.file_revision == reference.revision
    }
    /// One relayed direct byte request, in a bounded counted task: a cancelled
    /// HTTP waiter cannot abandon a sent Source request or its nested dial and
    /// body jobs, and the upstream driver retains the same custody.
    pub(super) async fn open_source_direct(
        &self,
        state: &AppState,
        demand: &DirectByteRequest,
    ) -> Result<crate::sharing_client::SourcePeerDirect, ReceiverStartError> {
        let lifetime: Arc<dyn Send + Sync> = self.0.bodies.reserve()?;
        let actor = self.clone();
        let state = state.clone();
        let demand = demand.clone();
        tokio::spawn(async move {
            let custody = lifetime.clone();
            let result = actor
                .open_source_direct_owned(&state, &demand, lifetime)
                .await;
            drop(custody);
            result
        })
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
    }
    async fn open_source_direct_owned(
        &self,
        state: &AppState,
        demand: &DirectByteRequest,
        lifetime: Arc<dyn Send + Sync>,
    ) -> Result<crate::sharing_client::SourcePeerDirect, ReceiverStartError> {
        let (_, _, received) = self.current_delivery_attachment(state).await?;
        let direct = received
            .direct()
            .cloned()
            .ok_or(ReceiverStartError::Unsupported)?;
        let expected = plurx_core::sharing::SharingIdentity {
            server_id: self.0.intent.scope.source_server_id,
            catalogue_epoch: self.0.intent.scope.catalogue_epoch,
            created_at_ms: 0,
        };
        let (peer, _) = crate::sharing_client::PeerConnection::verified_with_lifetime(
            &state.sharing,
            std::slice::from_ref(&received.endpoint),
            &expected,
            lifetime,
        )
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?;
        let known = received
            .lineage()
            .map_err(|_| ReceiverStartError::Unresolved)?;
        let opened = peer
            .file_direct(
                &received.credential,
                &received.viewer_hash,
                &self.0.peer_session,
                &known,
                demand,
                &direct,
            )
            .await
            .map_err(|_| ReceiverStartError::Unresolved)?;
        // Source IO can park. Reobserve the original B login, exact route,
        // current binding and delivery grant before returning any bytes.
        self.current_delivery_attachment(state).await?;
        Ok(opened)
    }
}

fn refusal(status: StatusCode, code: &'static str) -> Response {
    super::super::error::ApiError::typed(status, code, "Shared direct play is unavailable")
        .into_response()
}

/// The only accepted query is exactly one canonical `session=<B UUID>`.
fn session_query(query: Option<&str>) -> Option<Uuid> {
    let (key, value) = query?.split_once('=')?;
    if key != "session" || value.contains('&') {
        return None;
    }
    let session = Uuid::parse_str(value).ok()?;
    (session.get_version_num() == 4
        && session.get_variant() == uuid::Variant::RFC4122
        && session.to_string() == value)
        .then_some(session)
}

/// GET/HEAD `{file_base}/direct?session=<B UUID>`: relay one byte request to
/// the Source on the exact bound session, with Local's 200/206/416 shape.
pub(super) async fn receiver_direct(
    State(state): State<AppState>,
    Path((import, locator)): Path<(String, String)>,
    method: Method,
    request: axum::extract::Request,
) -> Response {
    let (mut parts, _) = request.into_parts();
    let Some(session) = session_query(parts.uri.query()) else {
        return refusal(StatusCode::BAD_REQUEST, "sharing_delivery_session_required");
    };
    let demand = match DirectByteRequest::from_request(&method, &parts.headers) {
        Ok(demand) => demand,
        Err(DirectRequestRefusal::Method) => {
            return refusal(StatusCode::METHOD_NOT_ALLOWED, "sharing_invalid_request")
        }
        Err(DirectRequestRefusal::RangeTooLarge) => {
            return refusal(
                StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
                "sharing_invalid_request",
            )
        }
    };
    let Ok(import_id) = Uuid::parse_str(&import) else {
        return refusal(StatusCode::BAD_REQUEST, "sharing_invalid_request");
    };
    if import_id.is_nil() || import_id.to_string() != import {
        return refusal(StatusCode::BAD_REQUEST, "sharing_invalid_request");
    }
    // Only an existing actor's exact session can authorize bytes. No lookup
    // creates or resumes a Source session.
    let actor = state.sharing.receiver_starts.by_session(session);
    let import = match state.store.sharing_import(import_id).await {
        Ok(Some(import)) if import.summary.state == "active" => import,
        Ok(_) => return refusal(StatusCode::NOT_FOUND, "sharing_delivery_unavailable"),
        Err(_) => {
            return refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_delivery_unavailable",
            )
        }
    };
    let key = match super::super::shared_artwork::receiver_key(&state).await {
        Ok(Some(key)) => key,
        _ => {
            return refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_delivery_unavailable",
            )
        }
    };
    let Ok(reference) = key.verify(&locator, import_id, import.summary.lifecycle_generation) else {
        return refusal(StatusCode::BAD_REQUEST, "sharing_invalid_request");
    };
    let Some(connection) = parts
        .extensions
        .get::<crate::SharingConnectionCancellation>()
        .cloned()
    else {
        return refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_delivery_unavailable",
        );
    };
    let Some(actor) = actor else {
        let route = match state.store.media_session_route(&session.to_string()).await {
            Ok(Some(route)) => route,
            _ => {
                return refusal(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "sharing_delivery_unavailable",
                )
            }
        };
        let proof = match state.store.receiver_relay_read_authority(&route).await {
            Ok(Some(proof)) if proof.binds_file(&reference) => proof,
            _ => return refusal(StatusCode::FORBIDDEN, "sharing_delivery_forbidden"),
        };
        if !authenticated_viewer_matches(&state, &mut parts, proof.viewer_id()).await {
            return refusal(StatusCode::FORBIDDEN, "sharing_delivery_forbidden");
        }
        return match forwarding::relay_operation(
            state,
            route,
            forwarding::ReceiverForwardOperation::Direct {
                demand,
                reference: forwarding::ReceiverDirectReference::from_reference(&reference),
                viewer_id: proof.viewer_id(),
            },
            Some(connection),
        )
        .await
        {
            Ok(response) => response,
            Err(error) => error.into_response(),
        };
    };
    if !authenticated_viewer_matches(&state, &mut parts, actor.0.intent.user_id).await {
        return refusal(StatusCode::FORBIDDEN, "sharing_delivery_forbidden");
    }
    let viewer_id = actor.0.intent.user_id;
    receiver_direct_actor(
        state,
        actor,
        &reference,
        demand,
        viewer_id,
        &connection,
        None,
    )
    .await
}

pub(super) async fn receiver_direct_actor(
    state: AppState,
    actor: ReceiverStartActor,
    reference: &plurx_core::sharing_file_locators::FileLocatorReference,
    demand: DirectByteRequest,
    viewer_id: i64,
    connection: &crate::SharingConnectionCancellation,
    context: Option<&forwarding::ReceiverForwardContext>,
) -> Response {
    use futures_util::StreamExt;
    if viewer_id != actor.0.intent.user_id || !actor.direct_binds(reference) {
        return refusal(StatusCode::FORBIDDEN, "sharing_delivery_forbidden");
    }
    if let Some(context) = context {
        if Instant::now() >= context.deadline
            || validate_forward_ingress(&state, &context.tuple, &context.ingress)
                .await
                .is_err()
        {
            return refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_delivery_unavailable",
            );
        }
    }
    // Authorize before registering a writer, then again after Source IO.
    if actor.current_delivery_attachment(&state).await.is_err() {
        return refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_delivery_unavailable",
        );
    }
    let state = Arc::new(state);
    let guard = match actor
        .retain_delivery_connection(state.clone(), connection)
        .await
    {
        Ok(guard) => guard,
        Err(_) => {
            return refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_delivery_unavailable",
            )
        }
    };
    let opened = match actor.open_source_direct(&state, &demand).await {
        Ok(opened) => opened,
        Err(ReceiverStartError::Capacity) => {
            return refusal(StatusCode::TOO_MANY_REQUESTS, "sharing_delivery_capacity")
        }
        Err(_) => {
            actor.begin_retirement(
                state,
                plurx_core::sharing_receiver_retirement::ReceiverRetirementReason::Revoked,
            );
            return refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_delivery_unavailable",
            );
        }
    };
    let stream = opened.body.into_data_stream().map(move |frame| {
        let _writer_ownership = &guard;
        frame
    });
    let mut response = axum::body::Body::from_stream(stream).into_response();
    *response.status_mut() = opened.head.status;
    for (name, value) in opened.head.headers {
        let Ok(value) = axum::http::HeaderValue::from_str(&value) else {
            return refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_delivery_unavailable",
            );
        };
        response.headers_mut().insert(name, value);
    }
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

async fn authenticated_viewer_matches(state: &AppState, parts: &mut Parts, user_id: i64) -> bool {
    if !has_account_headers(&parts.headers) {
        return true;
    }
    match super::super::extract::AuthUser::from_request_parts(parts, state).await {
        Ok(super::super::extract::AuthUser(user)) => user.id == user_id,
        Err(_) => false,
    }
}
fn has_account_headers(headers: &HeaderMap) -> bool {
    headers.contains_key(axum::http::header::AUTHORIZATION) || headers.contains_key("x-api-key")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    #[test]
    fn sharing_receiver_direct_session_query_is_exact() {
        let id = Uuid::new_v4();
        assert_eq!(session_query(Some(&format!("session={id}"))), Some(id));
        for query in [
            None,
            Some(String::new()),
            Some(format!("session={}", id.to_string().to_uppercase())),
            Some(format!("session={id}&token=x")),
            Some(format!("token=x&session={id}")),
            Some(format!("sessions={id}")),
            Some(format!("session={}", Uuid::nil())),
            Some("session=".to_owned()),
        ] {
            assert_eq!(session_query(query.as_deref()), None, "{query:?}");
        }
    }

    /// No session, a foreign session and every method/Range/HEAD shape are
    /// refused before any import, locator, Store or Source lookup.
    #[tokio::test]
    async fn sharing_receiver_direct_requires_exact_session_binding() {
        let state = crate::http::source_actor_test_state();
        let app = super::direct_router().with_state(state);
        let base = format!(
            "/shared/imports/{}/files/{}/direct",
            Uuid::new_v4(),
            "x".repeat(236)
        );
        for (method, uri, range) in [
            ("GET", base.clone(), None::<&str>),
            ("GET", format!("{base}?token=abc"), None),
            ("GET", format!("{base}?session=not-a-uuid"), None),
        ] {
            let mut request = axum::http::Request::builder().method(method).uri(&uri);
            if let Some(range) = range {
                request = request.header("range", range);
            }
            let response = app
                .clone()
                .oneshot(request.body(axum::body::Body::empty()).expect("request"))
                .await
                .expect("router");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{method} {uri}");
        }
        // A well-formed but unknown session never reaches the Store: no actor
        // owns it, so there is nothing to admit.
        for (method, range) in [("GET", None::<&str>)] {
            let mut request = axum::http::Request::builder()
                .method(method)
                .uri(format!("{base}?session={}", Uuid::new_v4()));
            if let Some(range) = range {
                request = request.header("range", range);
            }
            let response = app
                .clone()
                .oneshot(request.body(axum::body::Body::empty()).expect("request"))
                .await
                .expect("router");
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{method} {range:?}"
            );
        }
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("{base}?session={}", Uuid::new_v4()))
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("router");
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    /// HEAD and Range carry the same exact-binding requirement as GET: they
    /// only look sessions up, never admit, and are refused before any Store
    /// or Source access when the binding is absent or unknown.
    #[tokio::test]
    async fn sharing_receiver_direct_head_range_cannot_bypass_binding() {
        let state = crate::http::source_actor_test_state();
        let app = super::direct_router().with_state(state);
        let base = format!(
            "/shared/imports/{}/files/{}/direct",
            Uuid::new_v4(),
            "x".repeat(236)
        );
        for (method, query, range, if_range, expected) in [
            ("HEAD", String::new(), None, false, StatusCode::BAD_REQUEST),
            (
                "HEAD",
                String::new(),
                Some("bytes=0-1"),
                false,
                StatusCode::BAD_REQUEST,
            ),
            (
                "GET",
                String::new(),
                Some("bytes=0-1"),
                true,
                StatusCode::BAD_REQUEST,
            ),
            (
                "GET",
                String::new(),
                Some("bytes=-1"),
                false,
                StatusCode::BAD_REQUEST,
            ),
            (
                "HEAD",
                format!("?session={}", Uuid::new_v4()),
                None,
                false,
                StatusCode::NOT_FOUND,
            ),
            (
                "HEAD",
                format!("?session={}", Uuid::new_v4()),
                Some("bytes=0-"),
                false,
                StatusCode::NOT_FOUND,
            ),
            (
                "GET",
                format!("?session={}", Uuid::new_v4()),
                Some("bytes=2-3"),
                true,
                StatusCode::NOT_FOUND,
            ),
        ] {
            let mut request = axum::http::Request::builder()
                .method(method)
                .uri(format!("{base}{query}"));
            if let Some(range) = range {
                request = request.header("range", range);
            }
            if if_range {
                request = request.header("if-range", "\"x\"");
            }
            let response = app
                .clone()
                .oneshot(request.body(axum::body::Body::empty()).expect("request"))
                .await
                .expect("router");
            assert_eq!(response.status(), expected, "{method} {query} {range:?}");
            assert!(!response.headers().contains_key("content-range"));
            assert!(!response.headers().contains_key("accept-ranges"));
        }
    }

    #[test]
    fn sharing_protocol_fixture_direct_session_query() {
        use crate::sharing_protocol_fixture::{accepted, fixture, rows};
        let fixture = fixture();
        let b = Uuid::parse_str(fixture["direct"]["b_session"].as_str().expect("B"))
            .expect("B session");
        for row in rows(&fixture["direct_session_query"], "direct session query") {
            let query = row["query"].as_str().expect("query");
            assert_eq!(
                session_query(Some(query)),
                accepted(row).then_some(b),
                "{query}"
            );
        }
    }
}
