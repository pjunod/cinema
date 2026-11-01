use super::*;

use std::cell::RefCell;

use crate::http::error::ApiErrorCode;

/// Every route template under `/hls/{session}`, by method, and whether a
/// request for it is a read of a session that already exists.
///
/// The list is the classification, not a pattern: a route added under
/// `/hls/{session}` that is missing here fails
/// `every_hls_session_route_is_classified`, so it has to be placed
/// explicitly. Templates are matched on axum's `MatchedPath`, never on a
/// `/hls/` substring, so Live TV and every other family whose outage budget
/// is not `TranscodeManager`'s is never mistaken for one of these.
///
/// `true` marks a read of an existing session's playlist, object or status.
/// `false` marks a request that must keep failing fast while the node is
/// fenced: a control handler must never run twice, and a delete is not a
/// playback read.
pub(crate) const HLS_SESSION_ROUTES: &[(&str, &str, bool)] = &[
    ("GET", "/hls/{session}/master.m3u8", true),
    ("GET", "/hls/{session}/index.m3u8", true),
    ("GET", "/hls/{session}/video.m3u8", true),
    ("GET", "/hls/{session}/subs/{index}/index.m3u8", true),
    ("GET", "/hls/{session}/subs/{index}/{segment}", true),
    ("GET", "/hls/{session}/status", true),
    ("GET", "/hls/{session}/quality-family", true),
    ("GET", "/hls/{session}/{segment}", true),
    (
        "GET",
        "/hls/{session}/{role}/{rendition}/{kind}/{object}",
        true,
    ),
    ("GET", "/hls/{session}/{role}/{rendition}/index.m3u8", true),
    ("POST", "/hls/{session}/control", false),
    ("POST", "/hls/{session}/quality-control", false),
    ("POST", "/hls/{session}/quality-schedule", false),
    ("DELETE", "/hls/{session}", false),
];

/// One request on an `/hls/{session}` route, as the serving gate sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HlsSessionRoute {
    pub(crate) template: &'static str,
    /// A read of an existing session that may wait out a short outage.
    pub(crate) existing_session_read: bool,
}

/// Classify one matched route. `None` for everything that is not an
/// `/hls/{session}` route, including every Live TV and session-create path.
pub(crate) fn hls_session_route(method: &str, matched_path: &str) -> Option<HlsSessionRoute> {
    let template = matched_path.strip_prefix("/api/v1").unwrap_or(matched_path);
    // axum answers HEAD on a GET route; it is the same read.
    let method = if method == "HEAD" { "GET" } else { method };
    HLS_SESSION_ROUTES
        .iter()
        .find(|(route_method, route, _)| *route_method == method && *route == template)
        .map(|(_, route, held)| HlsSessionRoute {
            template: route,
            existing_session_read: *held,
        })
}

/// The session capability in an `/hls/{session}/...` path, for the log only.
pub(crate) fn hls_session_id(path: &str) -> Option<&str> {
    let (_, rest) = path.split_once("/hls/")?;
    rest.split('/').next().filter(|session| !session.is_empty())
}

/// What the request's handler learned about who owns the bytes it tried to
/// publish. Recorded while the request runs, read once it has an answer.
#[derive(Default)]
struct ResponseContext {
    owner_kind: Option<&'static str>,
    producer_attempt: Option<u64>,
    relayed_to: Option<String>,
}

tokio::task_local! {
    static RESPONSE_CONTEXT: RefCell<ResponseContext>;
}

/// Record the exact owner a response is being authorized against. A no-op
/// outside an observed request (relay workers, tests that call handlers
/// directly).
pub(crate) fn note_response_owner(owner: &crate::transcode::MediaResponseOwner) {
    let (kind, attempt) = owner.log_identity();
    note_owner_identity(kind, attempt);
}

fn note_owner_identity(kind: &'static str, attempt: Option<u64>) {
    let _ = RESPONSE_CONTEXT.try_with(|context| {
        let mut context = context.borrow_mut();
        context.owner_kind = Some(kind);
        context.producer_attempt = attempt;
    });
}

/// Record that this request's answer came from another node.
pub(crate) fn note_relay(owner_node_id: &str) {
    let _ = RESPONSE_CONTEXT.try_with(|context| {
        context.borrow_mut().relayed_to = Some(owner_node_id.to_owned());
    });
}

/// Hold a read of an existing session that arrived while this node is
/// fenced until `TranscodeManager::serving_fence_loop` resolves the outage.
///
/// Returns as soon as authority is held, so the request then runs exactly as
/// a fresh arrival: through the gate, the relay check and its handler, with
/// every admission made against the generation that came back. If the
/// outage expires, the manager is gone or the hold's own bound passes, it
/// returns with the node still fenced and the gate answers the
/// `serving_fenced` 503 it answers today.
///
/// The hold is read-only: it never creates a `SessionGrace`, so dropping the
/// request cannot spend or reset the outage budget the manager owns, and a
/// node that keeps flapping is accounted for once, there. It is bounded by
/// [`crate::serving_fence::SERVING_FENCE_SESSION_GRACE`] from arrival and by
/// the outage's own `expires_at`, whichever is first.
pub(crate) async fn hold_for_outage_resolution(state: &AppState) {
    let deadline =
        tokio::time::Instant::now().into_std() + crate::serving_fence::SERVING_FENCE_SESSION_GRACE;
    while let Some(baseline) = state.serving.authority().outage_baseline() {
        match state
            .transcode
            .wait_for_outage_resolution(baseline, deadline)
            .await
        {
            // Authority came back; check again, since it may already have
            // been lost again, which is a new outage after a newer baseline.
            crate::serving_fence::OutageWait::Recovered => {}
            crate::serving_fence::OutageWait::Expired
            | crate::serving_fence::OutageWait::TimedOut => return,
        }
    }
}

/// Run one `/hls/{session}` request and, when it fails in a way a player
/// treats as fatal, log the refusal with what is needed to attribute it: the
/// typed code, the serving loss generation at arrival and at answer, the
/// owner kind and exact producer attempt the handler authorized against, and
/// whether the answer was relayed.
///
/// Unthrottled on purpose: these are the client-visible failures a fleet
/// proof has to correlate one by one, and their rate is bounded by the
/// players asking.
pub(crate) async fn observe_hls_session_response(
    serving: &crate::serving_fence::ServingFence,
    route: HlsSessionRoute,
    path: &str,
    run: impl std::future::Future<Output = Response>,
) -> Response {
    let started_at = Instant::now();
    let authority = serving.authority();
    let arrival = authority.state();
    let (response, context) = RESPONSE_CONTEXT
        .scope(RefCell::new(ResponseContext::default()), async {
            let response = run.await;
            let context =
                RESPONSE_CONTEXT.with(|context| std::mem::take(&mut *context.borrow_mut()));
            (response, context)
        })
        .await;
    let status = response.status();
    if status.is_server_error() || status == StatusCode::GONE {
        let answered = authority.state();
        let session = hls_session_id(path).map(crate::transcode::session_log_id);
        tracing::warn!(
            target: "plurxd::http::hls",
            route = route.template,
            status = status.as_u16(),
            code = response
                .extensions()
                .get::<ApiErrorCode>()
                .map_or("untyped", |code| code.0),
            session = session.as_deref().unwrap_or("unknown"),
            arrival_loss_generation = arrival.loss_generation,
            arrival_ready = arrival.ready,
            loss_generation = answered.loss_generation,
            ready = answered.ready,
            owner_kind = context.owner_kind.unwrap_or("none"),
            producer_attempt = context.producer_attempt,
            relayed_to = context.relayed_to.as_deref(),
            latency_ms = started_at.elapsed().as_millis() as u64,
            "playback response failed"
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every route registered under `/hls/{session}` is in the table, so a
    /// new one cannot silently inherit either behaviour.
    #[test]
    fn every_hls_session_route_is_classified() {
        let router = include_str!("../mod.rs");
        let mut registered = Vec::new();
        for (index, _) in router.match_indices("\"/hls/{session}") {
            let rest = &router[index + 1..];
            let template = &rest[..rest.find('"').expect("closing quote")];
            registered.push(template);
        }
        assert!(
            registered.len() >= 10,
            "the scan must actually find the hls session routes: {registered:?}"
        );
        for template in registered {
            assert!(
                HLS_SESSION_ROUTES
                    .iter()
                    .any(|(_, route, _)| *route == template),
                "{template} is registered under /hls/{{session}} but not classified in \
                 HLS_SESSION_ROUTES"
            );
        }
    }

    #[test]
    fn only_existing_session_reads_are_held_and_live_tv_is_never_matched() {
        let held = |method, path| {
            hls_session_route(method, path).is_some_and(|route| route.existing_session_read)
        };
        assert!(held("GET", "/api/v1/hls/{session}/{segment}"));
        assert!(held("HEAD", "/api/v1/hls/{session}/{segment}"));
        assert!(held("GET", "/api/v1/hls/{session}/subs/{index}/{segment}"));
        assert!(held(
            "GET",
            "/api/v1/hls/{session}/{role}/{rendition}/{kind}/{object}"
        ));
        assert!(held("GET", "/hls/{session}/master.m3u8"));
        assert!(!held("POST", "/api/v1/hls/{session}/control"));
        assert!(!held("DELETE", "/api/v1/hls/{session}"));
        assert!(hls_session_route("POST", "/api/v1/hls/sessions").is_none());
        assert!(hls_session_route("GET", "/api/v1/files/{id}/hls/start").is_none());
        assert!(hls_session_route("GET", "/api/v1/live-tv/sessions/{session}/{segment}").is_none());
    }

    /// M0: a refusal a player treats as fatal is logged with its typed code,
    /// the loss generation it arrived and was answered under, the exact owner
    /// the handler authorized against, and the node that answered it.
    #[tokio::test]
    async fn a_failed_hls_session_response_logs_code_generation_owner_and_relay() {
        use plurx_core::cluster::migration::status::ReplicationMonitor;
        use tracing_subscriber::prelude::*;
        let buffer = Arc::new(crate::logbuf::LogBuffer::new(16));
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::registry().with(crate::logbuf::BufferLayer(Arc::clone(&buffer))),
        );
        let fence =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        fence.validation_set_ready(false).await;
        let route = hls_session_route("GET", "/api/v1/hls/{session}/{segment}").expect("route");
        let response =
            observe_hls_session_response(&fence, route, "/api/v1/hls/abc/seg00277.m4s", async {
                note_owner_identity("rolling", Some(7));
                note_relay("node-b");
                ApiError::typed(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "response_state_changed",
                    "changed",
                )
                .into_response()
            })
            .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let lines = buffer.tail("trace", 16);
        let line = lines
            .iter()
            .find(|entry| entry.message.contains("playback response failed"))
            .unwrap_or_else(|| panic!("no failure line in {lines:?}"));
        for field in [
            "route=\"/hls/{session}/{segment}\"",
            "status=503",
            "code=\"response_state_changed\"",
            "arrival_loss_generation=1",
            "loss_generation=1",
            "ready=false",
            "owner_kind=\"rolling\"",
            "producer_attempt=7",
            "relayed_to=\"node-b\"",
        ] {
            assert!(
                line.message.contains(field),
                "{field} missing from {}",
                line.message
            );
        }
        assert!(
            !line.message.contains("abc"),
            "the session capability is never logged: {}",
            line.message
        );

        // A success is not logged at all.
        let ok = observe_hls_session_response(&fence, route, "/api/v1/hls/abc/x", async {
            StatusCode::OK.into_response()
        })
        .await;
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(
            buffer
                .tail("trace", 16)
                .iter()
                .filter(|entry| entry.message.contains("playback response failed"))
                .count(),
            1
        );
    }

    #[test]
    fn the_session_id_is_read_from_the_path() {
        assert_eq!(hls_session_id("/api/v1/hls/abc/seg00001.m4s"), Some("abc"));
        assert_eq!(hls_session_id("/api/v1/hls/"), None);
        assert_eq!(hls_session_id("/api/v1/files/1"), None);
    }
}
