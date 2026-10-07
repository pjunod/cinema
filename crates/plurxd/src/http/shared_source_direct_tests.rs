//! Real Source fixture qualification of the direct lane: actual voter, actual
//! grant, actual decision engine, the actual peer router over HTTP/1 and 2.
use super::*;
use crate::http::sharing_direct_wire::{DecodedSourceDirectStart, DirectMethod};
use axum::http::{header, HeaderValue, Method, StatusCode};
use serde_json::json;
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use super::super::{
    real_source_start_fixture_with, tests::actual_resource_request, RealSourceStartFixture,
    SourceFixtureMode,
};

async fn direct_start(
    fixture: &RealSourceStartFixture,
    request: &Value,
) -> Result<axum::response::Response, ApiError> {
    fixture_start(
        axum::extract::State((*fixture.state).clone()),
        fixture.headers.clone(),
        axum::extract::Path((
            fixture.reference.item_id.as_str().to_owned(),
            fixture.reference.file_id.as_str().to_owned(),
        )),
        axum::body::Body::from(serde_json::to_vec(request).expect("canonical recipe")),
    )
    .await
}

async fn status_code(result: Result<axum::response::Response, ApiError>) -> (StatusCode, Value) {
    use axum::response::IntoResponse;
    let response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("bounded body");
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

struct DirectSession {
    known: Value,
    request_id: String,
    length: u64,
    path: std::path::PathBuf,
}

async fn started(fixture: &RealSourceStartFixture) -> DirectSession {
    let recipe: Value = serde_json::from_slice(&fixture.request).expect("recipe");
    started_with(fixture, recipe).await
}
async fn started_with(fixture: &RealSourceStartFixture, recipe: Value) -> DirectSession {
    let response = direct_start(fixture, &recipe).await.expect("direct Start");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("direct envelope");
    let decoded =
        DecodedSourceDirectStart::parse(&bytes, &fixture.reference).expect("strict direct Start");
    let snapshot = fixture
        .state
        .store
        .playback_planning_snapshot(1, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await
        .expect("planning read")
        .expect("fixture file");
    assert_eq!(decoded.direct().length, snapshot.file.size as u64);
    assert_eq!(decoded.direct().mime, "video/mp4");
    let mut known = recipe.clone();
    known["incarnation_id"] = json!(decoded.incarnation_id());
    known["session_id"] = json!(decoded.direct().session_id);
    known["control_epoch"] = json!(decoded.direct().control_epoch);
    DirectSession {
        request_id: recipe["session"]["request_id"]
            .as_str()
            .expect("request")
            .to_owned(),
        known,
        length: decoded.direct().length,
        path: snapshot.file.path.clone(),
    }
}

fn direct_body(session: &DirectSession, demand: &DirectByteRequest) -> Vec<u8> {
    let mut value = session.known.clone();
    value["direct"] = serde_json::to_value(demand).expect("demand");
    serde_json::to_vec(&value).expect("direct request")
}

fn direct_url(
    address: std::net::SocketAddr,
    fixture: &RealSourceStartFixture,
    session: &DirectSession,
    operation: &str,
) -> String {
    format!(
        "http://{address}/sharing/v1/items/{}/files/{}/sessions/{}/{operation}",
        fixture.reference.item_id.as_str(),
        fixture.reference.file_id.as_str(),
        session.request_id
    )
}

struct Peer {
    address: std::net::SocketAddr,
    stop: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<anyhow::Result<crate::HttpDrain>>,
}
async fn peer(fixture: &RealSourceStartFixture, gate: Option<Arc<SourceReadJobGate>>) -> Peer {
    let mut app = crate::http::sharing::peer_router((*fixture.state).clone());
    if let Some(gate) = gate {
        app = app.layer(axum::Extension(gate));
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(crate::serve_http(
        listener,
        app,
        async move {
            let _ = stopped.await;
        },
        crate::HTTP_TIMEOUTS,
    ));
    Peer {
        address,
        stop,
        server,
    }
}
impl Peer {
    async fn stop(self) {
        let _ = self.stop.send(());
        assert_eq!(
            self.server.await.expect("server").expect("shutdown"),
            crate::HttpDrain::Complete
        );
    }
}

fn request_headers(values: &[&str], if_range: bool) -> axum::http::HeaderMap {
    let mut headers = axum::http::HeaderMap::new();
    for value in values {
        headers.append(header::RANGE, HeaderValue::from_str(value).expect("range"));
    }
    if if_range {
        headers.insert(
            header::IF_RANGE,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
    }
    headers
}

/// The Source direct answer equals Local `serve_file_range` on the same file
/// for the same request: status, the full header set and the exact bytes.
async fn assert_matches_local(
    fixture: &RealSourceStartFixture,
    session: &DirectSession,
    peer: &Peer,
    cases: &[(Method, Vec<String>, bool)],
) {
    let url = direct_url(peer.address, fixture, session, "direct");
    for h2 in [false, true] {
        for (method, values, if_range) in cases {
            let values: Vec<&str> = values.iter().map(String::as_str).collect();
            let headers = request_headers(&values, *if_range);
            let local = crate::http::stream::serve_file_range(
                &session.path,
                &headers,
                method,
                Some(session.length),
            )
            .await
            .expect("Local direct answer");
            let demand = DirectByteRequest::from_request(method, &headers).expect("relayable");
            let source = actual_resource_request(
                peer.address,
                h2,
                &url,
                fixture.headers.clone(),
                direct_body(session, &demand),
            )
            .await;
            let label = format!("{method} {values:?} if_range={if_range} h2={h2}");
            assert_eq!(source.status(), local.status(), "{label}");
            for name in [
                header::CONTENT_TYPE,
                header::ACCEPT_RANGES,
                header::CONTENT_RANGE,
            ] {
                assert_eq!(
                    source.headers().get(&name),
                    local.headers().get(&name),
                    "{label} {name}"
                );
            }
            assert_eq!(
                source.headers().get(PLANNED_LENGTH_HEADER),
                local.headers().get(header::CONTENT_LENGTH),
                "{label}"
            );
            assert_eq!(source.headers()["cache-control"], "no-store");
            assert_eq!(
                source.headers()["cinemashare-session-id"],
                session.known["session_id"].as_str().expect("session")
            );
            let local = axum::body::to_bytes(local.into_body(), 4 * 1024 * 1024)
                .await
                .expect("Local bytes");
            let source = axum::body::to_bytes(source.into_body(), 4 * 1024 * 1024)
                .await
                .expect("Source bytes");
            assert_eq!(source, local, "{label}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_direct_start_requires_actual_direct_decision() {
    let fixture = real_source_start_fixture_with(SourceFixtureMode::Direct, None).await;
    let recipe: Value = serde_json::from_slice(&fixture.request).expect("recipe");
    // The player cannot take this container: A recomputes the decision with
    // the real caps and refuses; B's word that it is direct counts for nothing.
    let mut remux = recipe.clone();
    remux["session"]["request_id"] = json!(Uuid::new_v4());
    remux["session"]["caps"]["containers"] = json!(["webm"]);
    let (status, body) = status_code(direct_start(&fixture, &remux).await).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "sharing_start_unsupported");
    let mut rendered = recipe.clone();
    rendered["session"]["request_id"] = json!(Uuid::new_v4());
    rendered["session"]["native_subtitles"] = json!(true);
    let (status, _) = status_code(direct_start(&fixture, &rendered).await).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let mut unknown = recipe.clone();
    unknown["session"]["request_id"] = json!(Uuid::new_v4());
    unknown["session"]["presentation"] = json!("progressive");
    let (status, _) = status_code(direct_start(&fixture, &unknown).await).await;
    assert_ne!(status, StatusCode::OK);
    let session = started(&fixture).await;
    // The same request ID as HLS is different intent, never a replay.
    let mut hls = recipe.clone();
    hls["session"]["presentation"] = json!("vod");
    let (status, _) = status_code(direct_start(&fixture, &hls).await).await;
    assert_eq!(status, StatusCode::CONFLICT);
    // The exact direct request replays the same published session.
    let replay = direct_start(&fixture, &recipe).await.expect("exact replay");
    let bytes = axum::body::to_bytes(replay.into_body(), 64 * 1024)
        .await
        .expect("replay envelope");
    let decoded = DecodedSourceDirectStart::parse(&bytes, &fixture.reference).expect("replay");
    assert_eq!(
        decoded.direct().session_id,
        session.known["session_id"].as_str().expect("session")
    );
    // A direct session has no HLS resources.
    let peer = peer(&fixture, None).await;
    let mut resource = session.known.clone();
    resource["resource"] = json!("index.m3u8");
    let response = actual_resource_request(
        peer.address,
        false,
        &direct_url(peer.address, &fixture, &session, "resources"),
        fixture.headers.clone(),
        serde_json::to_vec(&resource).expect("resource"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    peer.stop().await;
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_direct_counts_grant_and_source_slots() {
    let fixture = real_source_start_fixture_with(SourceFixtureMode::Direct, None).await;
    let recipe: Value = serde_json::from_slice(&fixture.request).expect("recipe");
    for n in 0..4 {
        let mut next = recipe.clone();
        next["session"]["request_id"] = json!(Uuid::new_v4());
        next["session"]["playback_id"] = json!(format!("direct-player-{n}"));
        let (status, body) = status_code(direct_start(&fixture, &next).await).await;
        assert_eq!(status, StatusCode::OK, "{n}: {body}");
    }
    // Direct sessions hold the same per-grant Source slots as VOD sessions.
    let mut fifth = recipe.clone();
    fifth["session"]["request_id"] = json!(Uuid::new_v4());
    fifth["session"]["playback_id"] = json!("direct-player-4");
    let (status, body) = status_code(direct_start(&fixture, &fifth).await).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["code"], "sharing_start_capacity");
    let mut vod = recipe.clone();
    vod["session"]["presentation"] = json!("vod");
    vod["session"]["copy"] = json!(true);
    vod["session"]["height"] = json!(180);
    vod["session"]["quality_auto"] = json!(false);
    vod["session"]["request_id"] = json!(Uuid::new_v4());
    vod["session"]["playback_id"] = json!("vod-player");
    let (status, _) = status_code(direct_start(&fixture, &vod).await).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_direct_range_matches_local_serve_file_range() {
    let fixture = real_source_start_fixture_with(SourceFixtureMode::Direct, None).await;
    let session = started(&fixture).await;
    let peer = peer(&fixture, None).await;
    let len = session.length;
    let s = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
    let cases = vec![
        (Method::GET, s(&[]), false),
        (Method::HEAD, s(&[]), false),
        (Method::GET, s(&["bytes=0-0"]), false),
        (Method::GET, s(&["bytes=2-40"]), false),
        (Method::GET, s(&["bytes=-7"]), false),
        (Method::GET, s(&["bytes=100-"]), false),
        (Method::GET, vec![format!("bytes={}-", len - 1)], false),
        (Method::GET, vec![format!("bytes={len}-")], false),
        (Method::GET, s(&["bytes=99999999-,2-3"]), false),
        (Method::GET, s(&["bytes=0-1,bad"]), false),
        (Method::GET, s(&["items=0-1"]), false),
        (Method::GET, s(&["bytes=0-1", "bytes=2-3"]), false),
        (Method::HEAD, s(&["bytes=0-1"]), false),
    ];
    assert_matches_local(&fixture, &session, &peer, &cases).await;
    peer.stop().await;
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_direct_if_range_ignores_range_like_local() {
    let fixture = real_source_start_fixture_with(SourceFixtureMode::Direct, None).await;
    let session = started(&fixture).await;
    let peer = peer(&fixture, None).await;
    let s = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
    let cases = vec![
        (Method::GET, s(&["bytes=2-4"]), true),
        (Method::GET, s(&["bytes=99999999-"]), true),
        (Method::HEAD, s(&["bytes=2-4"]), true),
        (Method::GET, s(&[]), true),
    ];
    assert_matches_local(&fixture, &session, &peer, &cases).await;
    peer.stop().await;
    fixture.shutdown().await;
}

fn get_all() -> DirectByteRequest {
    DirectByteRequest {
        method: DirectMethod::Get,
        range: Vec::new(),
        if_range: false,
    }
}

async fn parked_body(
    fixture: &RealSourceStartFixture,
    session: &DirectSession,
    peer: &Peer,
    gate: &Arc<SourceReadJobGate>,
) -> reqwest::Response {
    let client = reqwest::Client::builder()
        .http1_only()
        .build()
        .expect("H1 client");
    let response = client
        .post(direct_url(peer.address, fixture, session, "direct"))
        .headers(fixture.headers.clone())
        .body(direct_body(session, &get_all()))
        .send()
        .await
        .expect("direct headers");
    assert_eq!(response.status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !gate.entered.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual blocking read is parked");
    response
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_direct_body_stops_on_revocation() {
    let fixture = real_source_start_fixture_with(SourceFixtureMode::Direct, None).await;
    let session = started(&fixture).await;
    let gate = Arc::new(SourceReadJobGate::default());
    let peer = peer(&fixture, Some(Arc::clone(&gate))).await;
    let response = parked_body(&fixture, &session, &peer, &gate).await;
    let reading = tokio::spawn(async move { response.bytes().await });
    fixture
        .state
        .store
        .revoke_share(fixture.grant, crate::state::clock_ms())
        .await
        .expect("actual grant revoke");
    let read = tokio::time::timeout(Duration::from_secs(10), reading)
        .await
        .expect("revocation ends the open body")
        .expect("reader task");
    assert!(read.is_err(), "a revoked body never completes");
    gate.release();
    // Revoked: no further byte request is admitted.
    let response = actual_resource_request(
        peer.address,
        false,
        &direct_url(peer.address, &fixture, &session, "direct"),
        fixture.headers.clone(),
        direct_body(&session, &get_all()),
    )
    .await;
    assert_ne!(response.status(), StatusCode::OK);
    peer.stop().await;
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_direct_read_join_before_end_receipt() {
    let fixture = real_source_start_fixture_with(SourceFixtureMode::Direct, None).await;
    let session = started(&fixture).await;
    let gate = Arc::new(SourceReadJobGate::default());
    let peer = peer(&fixture, Some(Arc::clone(&gate))).await;
    let response = parked_body(&fixture, &session, &peer, &gate).await;
    let client = reqwest::Client::builder().build().expect("End client");
    let end_url = direct_url(peer.address, &fixture, &session, "end");
    let headers = fixture.headers.clone();
    let known = serde_json::to_vec(&session.known).expect("known lineage");
    let ending = tokio::spawn(async move {
        client
            .post(end_url)
            .headers(headers)
            .body(known)
            .send()
            .await
            .expect("actual End")
            .bytes()
            .await
            .expect("receipt")
    });
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!ending.is_finished(), "a parked read job holds settlement");
    drop(response);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !ending.is_finished(),
        "dropping the response cannot release a live read job"
    );
    assert!(!gate.complete.load(Ordering::SeqCst));
    gate.release();
    let receipt = tokio::time::timeout(Duration::from_secs(15), ending)
        .await
        .expect("read joined")
        .expect("End owner");
    let receipt: Value = serde_json::from_slice(&receipt).expect("terminal receipt");
    assert_eq!(receipt["state"], "settled");
    assert_eq!(receipt["settled"], true);
    assert_eq!(receipt["session_id"], session.known["session_id"]);
    assert!(gate.complete.load(Ordering::SeqCst));
    assert!(gate.file_closed.load(Ordering::SeqCst));
    peer.stop().await;
    fixture.shutdown().await;
}

// Creating a symlink is a Unix fixture step (Windows needs a privilege).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_direct_symlink_or_resized_file_refuses() {
    let fixture = real_source_start_fixture_with(SourceFixtureMode::Direct, None).await;
    let session = started(&fixture).await;
    let peer = peer(&fixture, None).await;
    let url = direct_url(peer.address, &fixture, &session, "direct");
    let request = || direct_body(&session, &get_all());
    let ok = actual_resource_request(
        peer.address,
        false,
        &url,
        fixture.headers.clone(),
        request(),
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);
    // A link at the catalogued path is never followed, even to the same bytes.
    let moved = session.path.with_extension("moved.mp4");
    std::fs::rename(&session.path, &moved).expect("move original");
    std::os::unix::fs::symlink(&moved, &session.path).expect("link in place");
    let linked = actual_resource_request(
        peer.address,
        false,
        &url,
        fixture.headers.clone(),
        request(),
    )
    .await;
    assert_eq!(linked.status(), StatusCode::SERVICE_UNAVAILABLE);
    // Moving the bytes back relinks the inode (its ctime moves), so the
    // planned object identity no longer holds for this session either.
    std::fs::remove_file(&session.path).expect("remove link");
    std::fs::rename(&moved, &session.path).expect("restore original");
    let relinked = actual_resource_request(
        peer.address,
        false,
        &url,
        fixture.headers.clone(),
        request(),
    )
    .await;
    assert_eq!(relinked.status(), StatusCode::SERVICE_UNAVAILABLE);
    // A fresh start plans the restored object and serves it.
    let mut recipe: Value = serde_json::from_slice(&fixture.request).expect("recipe");
    recipe["session"]["request_id"] = json!(Uuid::new_v4());
    recipe["session"]["playback_id"] = json!("direct-after-restore");
    let fresh = started_with(&fixture, recipe.clone()).await;
    let fresh_url = direct_url(peer.address, &fixture, &fresh, "direct");
    let served = actual_resource_request(
        peer.address,
        false,
        &fresh_url,
        fixture.headers.clone(),
        direct_body(&fresh, &get_all()),
    )
    .await;
    assert_eq!(served.status(), StatusCode::OK);
    // A resized file is a different object: refused, never a short or long body.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&session.path)
        .expect("append");
    std::io::Write::write_all(&mut file, b"x").expect("grow");
    drop(file);
    let resized = actual_resource_request(
        peer.address,
        false,
        &fresh_url,
        fixture.headers.clone(),
        direct_body(&fresh, &get_all()),
    )
    .await;
    assert_eq!(resized.status(), StatusCode::SERVICE_UNAVAILABLE);
    // And a new start against the resized file no longer matches the scanner.
    recipe["session"]["request_id"] = json!(Uuid::new_v4());
    recipe["session"]["playback_id"] = json!("direct-after-resize");
    let (status, _) = status_code(direct_start(&fixture, &recipe).await).await;
    assert_ne!(status, StatusCode::OK);
    peer.stop().await;
    fixture.shutdown().await;
}
