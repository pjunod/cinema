use super::*;
use serde_json::json;
pub(super) async fn actual_resource_request(
    address: std::net::SocketAddr,
    h2: bool,
    url: &str,
    headers: HeaderMap,
    body: Vec<u8>,
) -> axum::response::Response {
    if !h2 {
        let client = reqwest::Client::builder()
            .http1_only()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("H1 client");
        let response = client
            .post(url)
            .headers(headers)
            .header("connection", "close")
            .body(body)
            .send()
            .await
            .expect("H1 response");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.bytes().await.expect("actual bytes");
        let mut response = axum::response::Response::new(axum::body::Body::from(bytes));
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        return response;
    }
    let socket = tokio::net::TcpStream::connect(address)
        .await
        .expect("H2 socket");
    let (mut sender, driver) =
        hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
            .handshake::<_, axum::body::Body>(hyper_util::rt::TokioIo::new(socket))
            .await
            .expect("H2 handshake");
    let driver = tokio::spawn(driver);
    let mut request = axum::http::Request::builder()
        .method("POST")
        .uri(url)
        .body(axum::body::Body::from(body))
        .expect("H2 request");
    *request.headers_mut() = headers;
    let response = sender.send_request(request).await.expect("H2 response");
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(axum::body::Body::new(body), 4 * 1024 * 1024)
        .await
        .expect("actual bounded resource bytes");
    drop(sender);
    driver.abort();
    let _ = driver.await;
    axum::response::Response::from_parts(parts, axum::body::Body::from(bytes))
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_end_recovers_only_after_actual_sql_settlement_retry() {
    Box::pin(actual_source_end_sql_recovery()).await;
}
async fn actual_source_end_sql_recovery() {
    use std::time::{Duration, Instant};
    let fixture = real_source_start_fixture().await;
    let response = fixture_start(
        axum::extract::State((*fixture.state).clone()),
        fixture.headers.clone(),
        axum::extract::Path((
            fixture.reference.item_id.as_str().to_owned(),
            fixture.reference.file_id.as_str().to_owned(),
        )),
        axum::body::Body::from(fixture.request.clone()),
    )
    .await
    .expect("actual Source Start");
    axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("actual body finished");
    let entry = fixture
        .state
        .transcode
        .source_http_starts
        .entries
        .lock()
        .expect("entry")
        .first()
        .cloned()
        .expect("actual entry");
    let owned = entry
        .wait(Instant::now() + Duration::from_secs(1))
        .await
        .expect("actual owned actor");
    let client = fixture
        .selected
        .local_client()
        .expect("actual selected one-voter client");
    // Inject a real transactional failure at the actual release write. No
    // Source actor flags/proofs or physical settlement results are forged.
    client.execute("CREATE TRIGGER fixture_source_release_failure BEFORE UPDATE OF reservation_state ON sharing_source_session_bindings WHEN NEW.reservation_state='released' BEGIN SELECT RAISE(ABORT,'fixture release temporarily unavailable'); END",hiqlite::params![]).await.expect("actual transient SQL failure fixture");
    let first = entry.end(std::sync::Arc::clone(&fixture.state));
    assert_eq!(
        first
            .wait(Instant::now() + Duration::from_secs(10))
            .await
            .err(),
        Some(SourceStartFailure::Unresolved)
    );
    assert_eq!(
        owned.actor.settlement_status(),
        Some(Err(
            crate::transcode::source_actor::SourceWorkerError::Unresolved
        ))
    );
    assert!(
        std::sync::Arc::ptr_eq(&first, &entry.end(std::sync::Arc::clone(&fixture.state))),
        "failed proof remains retained before actual successful retry"
    );
    client
        .execute(
            "DROP TRIGGER fixture_source_release_failure",
            hiqlite::params![],
        )
        .await
        .expect("restore actual SQL release");
    tokio::time::timeout(Duration::from_secs(15), async {
        while owned.actor.settlement_status() != Some(Ok(())) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("same actor physically/SQL settles through owned retry");
    let recovered = entry.end(std::sync::Arc::clone(&fixture.state));
    assert!(!std::sync::Arc::ptr_eq(&first, &recovered));
    let receipt = recovered
        .wait(Instant::now() + Duration::from_secs(5))
        .await
        .expect("actual recovered receipt");
    assert!(receipt.settled);
    assert_eq!(
        receipt.incarnation_id,
        owned.assignment.binding().incarnation_id()
    );
    assert_eq!(receipt.confirmation_id.get_version_num(), 4);
    let replay = entry.end(std::sync::Arc::clone(&fixture.state));
    assert!(std::sync::Arc::ptr_eq(&recovered, &replay));
    assert_eq!(
        replay
            .wait(Instant::now() + Duration::from_secs(1))
            .await
            .expect("stable confirmation")
            .confirmation_id,
        receipt.confirmation_id
    );
    fixture.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_failed_claim_and_assignment_retain_joined_exact_task_stages() {
    Box::pin(actual_source_failed_task_stages()).await;
}
async fn actual_source_failed_task_stages() {
    use std::time::{Duration, Instant};
    for assignment_failure in [false, true] {
        let fixture = real_source_start_fixture().await;
        let client = fixture.selected.local_client().expect("actual voter");
        let trigger = if assignment_failure {
            "CREATE TRIGGER fixture_source_task_failure BEFORE UPDATE OF dispatch_generation ON sharing_source_session_bindings WHEN NEW.dispatch_generation=1 BEGIN SELECT RAISE(ABORT,'fixture assignment write failure'); END"
        } else {
            "CREATE TRIGGER fixture_source_task_failure BEFORE INSERT ON sharing_source_session_bindings BEGIN SELECT RAISE(ABORT,'fixture claim write failure'); END"
        };
        client
            .execute(trigger, hiqlite::params![])
            .await
            .expect("actual transactional failure");
        let invoke = || {
            fixture_start(
                axum::extract::State((*fixture.state).clone()),
                fixture.headers.clone(),
                axum::extract::Path((
                    fixture.reference.item_id.as_str().to_owned(),
                    fixture.reference.file_id.as_str().to_owned(),
                )),
                axum::body::Body::from(fixture.request.clone()),
            )
        };
        let first = invoke()
            .await
            .expect_err("failed actual write stays unresolved");
        use axum::response::IntoResponse;
        assert_eq!(
            first.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        let entry = fixture
            .state
            .transcode
            .source_http_starts
            .entries
            .lock()
            .expect("retained task")
            .first()
            .cloned()
            .expect("actual owner entry");
        assert!(matches!(
            *entry.task.joined.lock().expect("actual joined task"),
            Some(SourceStartTaskJoined::Returned)
        ));
        let incarnation = {
            let stage = entry.task.stage.lock().expect("exact stage");
            if assignment_failure {
                assert!(
                    matches!(*stage, SourceStartTaskStage::Assigning(_)),
                    "retain the acquired g0 binding across failed assignment"
                );
            } else {
                assert!(
                    matches!(*stage, SourceStartTaskStage::Claiming { .. }),
                    "retain the actual intent and planned incarnation across unknown claim"
                );
            }
            stage.incarnation().expect("actual planned/claimed lineage")
        };
        assert_eq!(incarnation.get_version_num(), 4);
        assert_eq!(
            entry
                .wait(Instant::now() + Duration::from_secs(1))
                .await
                .err(),
            Some(SourceStartFailure::Unresolved)
        );
        let end = entry.end(std::sync::Arc::clone(&fixture.state));
        if assignment_failure {
            let receipt = end
                .wait(Instant::now() + Duration::from_secs(1))
                .await
                .expect("exact acquired g0 cleanup");
            assert_eq!(receipt.incarnation_id, incarnation);
            assert!(receipt.session_id.is_none());
            assert!(entry.actual_finished());
        } else {
            assert_eq!(
                end.wait(Instant::now() + Duration::from_secs(1))
                    .await
                    .err(),
                Some(SourceStartFailure::Unresolved),
                "unknown claim is never no-admission"
            );
            assert!(!entry.actual_finished());
        }
        client
            .execute(
                "DROP TRIGGER fixture_source_task_failure",
                hiqlite::params![],
            )
            .await
            .expect("restore real Store writes");
        assert_eq!(
            invoke()
                .await
                .expect_err("exact retry must not redispatch unknown outcome")
                .into_response()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            entry
                .task
                .stage
                .lock()
                .expect("retained immutable stage")
                .incarnation(),
            Some(incarnation)
        );
        let registry = &fixture.state.transcode.source_http_starts;
        if assignment_failure {
            assert!(registry.entries.lock().expect("start capacity").is_empty());
            assert!(registry
                .settled
                .lock()
                .expect("settled receipts")
                .iter()
                .any(|settled| std::sync::Arc::ptr_eq(settled, &entry)));
        } else {
            assert!(registry
                .entries
                .lock()
                .expect("unresolved custody")
                .iter()
                .any(|retained| std::sync::Arc::ptr_eq(retained, &entry)));
            assert!(registry
                .settled
                .lock()
                .expect("settled receipts")
                .is_empty());
        }
        let original_deadline = *entry
            .end_deadline
            .get()
            .expect("original bounded End deadline");
        let retry = entry.end(std::sync::Arc::clone(&fixture.state));
        if assignment_failure {
            assert!(
                std::sync::Arc::ptr_eq(&end, &retry),
                "successful settlement keeps its exact receipt owner"
            );
        } else {
            // An errored waiter may retry the retained actual UncertainG0
            // cleanup. The failed INSERT left no binding: removing its
            // trigger is not physical closure or positive no-admit proof.
            assert!(!std::sync::Arc::ptr_eq(&end, &retry));
            assert_eq!(
                retry
                    .wait(Instant::now() + Duration::from_secs(1))
                    .await
                    .err(),
                Some(SourceStartFailure::Unresolved)
            );
            assert!(!entry.actual_finished());
            assert!(
                matches!(&*entry.task.cleanup.lock().expect("retained cleanup intent"), Some(SourceUninvokedCleanup::UncertainG0 { planned_incarnation, .. }) if *planned_incarnation == incarnation)
            );
            assert_eq!(
                entry
                    .task
                    .stage
                    .lock()
                    .expect("same immutable Start stage")
                    .incarnation(),
                Some(incarnation)
            );
            assert!(registry
                .entries
                .lock()
                .expect("retained exact unresolved Start")
                .iter()
                .any(|held| std::sync::Arc::ptr_eq(held, &entry)));
            assert!(registry
                .settled
                .lock()
                .expect("no invented settlement")
                .is_empty());
        }
        assert_eq!(
            *entry.end_deadline.get().expect("same original End bound"),
            original_deadline
        );
        fixture.shutdown().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_uninvoked_g0_cleanup_retains_failed_release_and_retries_exact_end() {
    use std::time::{Duration, Instant};
    let fixture = real_source_start_fixture().await;
    let client = fixture.selected.local_client().expect("actual voter");
    client.execute("CREATE TRIGGER fixture_source_assignment_failure BEFORE UPDATE OF dispatch_generation ON sharing_source_session_bindings WHEN NEW.dispatch_generation=1 BEGIN SELECT RAISE(ABORT,'fixture assignment unavailable'); END", hiqlite::params![]).await.expect("actual failed assignment");
    client.execute("CREATE TRIGGER fixture_source_g0_release_failure BEFORE UPDATE OF reservation_state ON sharing_source_session_bindings WHEN NEW.reservation_state='released' BEGIN SELECT RAISE(ABORT,'fixture release unavailable'); END", hiqlite::params![]).await.expect("actual failed cleanup");
    fixture_start(
        axum::extract::State((*fixture.state).clone()),
        fixture.headers.clone(),
        axum::extract::Path((
            fixture.reference.item_id.as_str().to_owned(),
            fixture.reference.file_id.as_str().to_owned(),
        )),
        axum::body::Body::from(fixture.request.clone()),
    )
    .await
    .expect_err("failed assignment");
    let entry = fixture
        .state
        .transcode
        .source_http_starts
        .entries
        .lock()
        .expect("retained owner")
        .first()
        .cloned()
        .expect("actual fresh acquired owner");
    let incarnation = entry
        .task
        .stage
        .lock()
        .expect("retained stage")
        .incarnation()
        .expect("acquired lineage");
    assert!(!entry.actual_finished());
    let first = entry.end(std::sync::Arc::clone(&fixture.state));
    assert_eq!(
        first
            .wait(Instant::now() + Duration::from_secs(1))
            .await
            .err(),
        Some(SourceStartFailure::Unresolved)
    );
    assert!(entry
        .task
        .cleanup_confirmation
        .lock()
        .expect("cleanup receipt")
        .is_none());
    client
        .execute(
            "DROP TRIGGER fixture_source_g0_release_failure",
            hiqlite::params![],
        )
        .await
        .expect("restore cleanup");
    let recovered = entry.end(std::sync::Arc::clone(&fixture.state));
    let receipt = recovered
        .wait(Instant::now() + Duration::from_secs(2))
        .await
        .expect("actual g0 cleanup");
    assert_eq!(receipt.incarnation_id, incarnation);
    assert!(receipt.session_id.is_none());
    assert!(entry.actual_finished());
    let replay = entry.end(std::sync::Arc::clone(&fixture.state));
    assert!(std::sync::Arc::ptr_eq(&recovered, &replay));
    assert_eq!(
        replay
            .wait(Instant::now() + Duration::from_secs(1))
            .await
            .expect("same receipt")
            .confirmation_id,
        receipt.confirmation_id
    );
    client
        .execute(
            "DROP TRIGGER fixture_source_assignment_failure",
            hiqlite::params![],
        )
        .await
        .expect("restore assignment");
    fixture.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_lost_start_end_uses_actual_assignment_without_inventing_session() {
    Box::pin(actual_lost_start_end()).await;
}
async fn actual_lost_start_end() {
    use std::time::{Duration, Instant};
    use tokio::io::AsyncWriteExt;
    let fixture = real_source_start_fixture().await;
    let hold = fixture
        .state
        .transcode
        .test_hold_source_software_capacity()
        .expect("real SW4 admission held");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(crate::serve_http(
        listener,
        super::super::sharing::peer_router((*fixture.state).clone()),
        async move {
            let _ = stopped.await;
        },
        crate::HTTP_TIMEOUTS,
    ));
    let path = format!(
        "/sharing/v1/items/{}/files/{}/sessions",
        fixture.reference.item_id.as_str(),
        fixture.reference.file_id.as_str()
    );
    let mut socket = tokio::net::TcpStream::connect(address)
        .await
        .expect("initial TCP");
    let head=format!("POST {path} HTTP/1.1\r\nHost: fixture\r\nAuthorization: {}\r\nCinemaShare-Viewer: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",fixture.headers["authorization"].to_str().expect("auth"),fixture.headers["cinemashare-viewer"].to_str().expect("viewer"),fixture.request.len());
    socket.write_all(head.as_bytes()).await.expect("head");
    socket
        .write_all(&fixture.request)
        .await
        .expect("full recipe");
    let entry = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let entry = fixture
                .state
                .transcode
                .source_http_starts
                .entries
                .lock()
                .expect("entries")
                .first()
                .cloned();
            if let Some(entry) = entry {
                if entry.result.lock().expect("owner").is_some() {
                    break entry;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual assignment/actor owned");
    let owned = entry
        .wait(Instant::now() + Duration::from_secs(1))
        .await
        .expect("actual actor");
    assert!(entry.published.lock().expect("publication").is_none());
    drop(socket);
    fixture
        .state
        .store
        .revoke_share(fixture.grant, crate::state::clock_ms())
        .await
        .expect("revoke");
    fixture
        .state
        .store
        .put_setting(plurx_core::store::keys::SHARING_ENABLED, "false")
        .await
        .expect("off");
    let recipe: Value = serde_json::from_slice(&fixture.request).expect("original");
    let request = recipe["session"]["request_id"].as_str().expect("request");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("cleanup client");
    let url = format!("http://{address}{path}/{request}/end");
    let body = fixture.request.clone();
    let headers = fixture.headers.clone();
    let first_client = client.clone();
    let first_url = url.clone();
    let first = tokio::spawn(async move {
        first_client
            .post(first_url)
            .headers(headers)
            .header("connection", "close")
            .body(body)
            .send()
            .await
            .expect("lost Start End")
            .bytes()
            .await
            .expect("receipt")
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while entry.ending.lock().expect("End").is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached End owned");
    // Drop the real occupied admission; the actor must prove its actual
    // admitted/reserved no-spawn retirement, never infer it from no response.
    drop(hold);
    let receipt = tokio::time::timeout(Duration::from_secs(15), first)
        .await
        .expect("actual no-spawn settlement")
        .expect("End task");
    let value: Value = serde_json::from_slice(&receipt).expect("closed receipt");
    assert_eq!(
        value["incarnation_id"],
        json!(owned.assignment.binding().incarnation_id())
    );
    assert!(value.get("session_id").is_none());
    assert!(value.get("control_epoch").is_none());
    assert_eq!(value["settled"], true);
    assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
    assert!(
        fixture
            .state
            .store
            .media_session_route_by_incarnation(
                &owned.assignment.binding().incarnation_id().to_string()
            )
            .await
            .expect("route read")
            .is_none(),
        "actual never-published worker did not acquire a fake session"
    );
    assert_eq!(
        client
            .post(url)
            .headers(fixture.headers.clone())
            .header("connection", "close")
            .body(fixture.request.clone())
            .send()
            .await
            .expect("exact retry")
            .bytes()
            .await
            .expect("same receipt"),
        receipt
    );
    let _ = stop.send(());
    server.await.expect("server").expect("shutdown");
    drop((owned, client));
    fixture.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_real_h1_h2_playlist_init_and_segment_echo_exact_lineage() {
    Box::pin(actual_source_resource_delivery(SourceFixtureMode::Copy)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_real_encoded_h1_h2_playlist_init_and_segment() {
    Box::pin(actual_source_resource_delivery(SourceFixtureMode::Encoded)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_native_copy_h1_h2_vtt_and_held_body_end() {
    Box::pin(actual_source_resource_delivery(
        SourceFixtureMode::NativeCopy,
    ))
    .await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_native_encoded_h1_h2_vtt_and_held_body_end() {
    Box::pin(actual_source_resource_delivery(
        SourceFixtureMode::NativeEncoded,
    ))
    .await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_burned_text_h1_h2_and_confirmed_end() {
    Box::pin(actual_source_resource_delivery(SourceFixtureMode::BurnText)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_burned_bitmap_h1_h2_and_confirmed_end() {
    Box::pin(actual_source_resource_delivery(
        SourceFixtureMode::BurnBitmap,
    ))
    .await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_hdr10_to_sdr_h1_h2_and_confirmed_end() {
    Box::pin(actual_source_resource_delivery(SourceFixtureMode::Hdr10Sdr)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_hdr10_preserved_h1_h2_and_confirmed_end() {
    Box::pin(actual_source_resource_delivery(SourceFixtureMode::Hdr10)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_dolby_vision_refusal_fresh_claim_confirms_exact_end() {
    use axum::response::IntoResponse;
    let fixture = real_source_start_fixture_with(SourceFixtureMode::DolbyVisionEncoded, None).await;
    let denied = fixture_start(
        axum::extract::State((*fixture.state).clone()),
        fixture.headers.clone(),
        axum::extract::Path((
            fixture.reference.item_id.as_str().to_owned(),
            fixture.reference.file_id.as_str().to_owned(),
        )),
        axum::body::Body::from(fixture.request.clone()),
    )
    .await
    .expect_err("DV encode has no Source-owned RPU preparation")
    .into_response();
    assert_eq!(denied.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let bytes = axum::body::to_bytes(denied.into_body(), 4096)
        .await
        .expect("typed refusal");
    let body: Value = serde_json::from_slice(&bytes).expect("JSON refusal");
    assert_eq!(body["code"], SHARING_START_DOLBY_VISION_UNSUPPORTED);
    let entries = fixture
        .state
        .transcode
        .source_http_starts
        .entries
        .lock()
        .expect("registry")
        .clone();
    assert_eq!(
        entries.len(),
        1,
        "retain the exact failed invocation for cleanup"
    );
    assert!(
        matches!(
            *entries[0].task.stage.lock().expect("stage"),
            SourceStartTaskStage::Acquired(_)
        ),
        "fresh invocation is claimed before preparation but never dispatched"
    );
    let request = entries[0].identity.request_id.to_string();
    let first = end(
        axum::extract::State((*fixture.state).clone()),
        fixture.headers.clone().into(),
        axum::extract::Path((
            fixture.reference.item_id.as_str().to_owned(),
            fixture.reference.file_id.as_str().to_owned(),
            request.clone(),
        )),
        None,
        axum::body::Body::from(fixture.request.clone()),
    )
    .await
    .expect("authenticated exact g0 cleanup")
    .0;
    assert!(first.session_id.is_none());
    assert!(
        entries[0].actual_finished(),
        "accounting released only after exact cleanup"
    );
    for _ in 0..3 {
        fixture_start(
            axum::extract::State((*fixture.state).clone()),
            fixture.headers.clone(),
            axum::extract::Path((
                fixture.reference.item_id.as_str().to_owned(),
                fixture.reference.file_id.as_str().to_owned(),
            )),
            axum::body::Body::from(fixture.request.clone()),
        )
        .await
        .expect_err("exact refusal retry");
        let replay = end(
            axum::extract::State((*fixture.state).clone()),
            fixture.headers.clone().into(),
            axum::extract::Path((
                fixture.reference.item_id.as_str().to_owned(),
                fixture.reference.file_id.as_str().to_owned(),
                request.clone(),
            )),
            None,
            axum::body::Body::from(fixture.request.clone()),
        )
        .await
        .expect("same cleanup receipt")
        .0;
        assert_eq!(replay.confirmation_id, first.confirmation_id);
        assert_eq!(replay.incarnation_id, first.incarnation_id);
    }
    fixture.shutdown().await;
}
async fn actual_source_resource_delivery(mode: SourceFixtureMode) {
    use std::time::{Duration, Instant};
    let fixture = real_source_start_fixture_with(mode, None).await;
    let native = matches!(
        mode,
        SourceFixtureMode::NativeCopy | SourceFixtureMode::NativeEncoded
    );
    let mut unsupported: Value = serde_json::from_slice(&fixture.request).expect("recipe");
    unsupported["session"]["previous_session_id"] = json!(Uuid::new_v4());
    unsupported["session"]["request_id"] = json!(Uuid::new_v4());
    let denied = fixture_start(
        axum::extract::State((*fixture.state).clone()),
        fixture.headers.clone(),
        axum::extract::Path((
            fixture.reference.item_id.as_str().to_owned(),
            fixture.reference.file_id.as_str().to_owned(),
        )),
        axum::body::Body::from(serde_json::to_vec(&unsupported).expect("complete prior recipe")),
    )
    .await
    .expect_err("foreign predecessor denied before Source preparation");
    use axum::response::IntoResponse;
    assert_eq!(
        denied.into_response().status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert!(fixture
        .state
        .transcode
        .source_http_starts
        .entries
        .lock()
        .expect("registry")
        .iter()
        .all(|entry| entry.actual_finished()));
    let response = fixture_start(
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
    let decoded = super::super::decode_source_start_response(&bytes, &fixture.reference)
        .expect("strict actual Start");
    if matches!(mode, SourceFixtureMode::Hdr10 | SourceFixtureMode::Hdr10Sdr) {
        assert_eq!(
            decoded.response().delivered_dynamic_range.as_deref(),
            Some(if matches!(mode, SourceFixtureMode::Hdr10) {
                "hdr10"
            } else {
                "sdr"
            })
        );
    }
    let mut recipe: Value = serde_json::from_slice(&fixture.request).expect("recipe");
    let request = recipe["session"]["request_id"]
        .as_str()
        .expect("request")
        .to_owned();
    recipe["incarnation_id"] = json!(decoded.incarnation_id());
    recipe["session_id"] = json!(decoded.response().session_id);
    recipe["control_epoch"] = json!(
        decoded
            .response()
            .control
            .as_ref()
            .expect("control")
            .control_epoch
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(crate::serve_http(
        listener,
        super::super::sharing::peer_router((*fixture.state).clone()),
        async move {
            let _ = stopped.await;
        },
        crate::HTTP_TIMEOUTS,
    ));
    let url = format!(
        "http://{address}/sharing/v1/items/{}/files/{}/sessions/{request}/resources",
        fixture.reference.item_id.as_str(),
        fixture.reference.file_id.as_str()
    );
    for h2 in [false, true] {
        recipe["resource"] = json!(if native { "video.m3u8" } else { "index.m3u8" });
        let response = actual_resource_request(
            address,
            h2,
            &url,
            fixture.headers.clone(),
            serde_json::to_vec(&recipe).expect("request"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["cinemashare-session-id"],
            decoded.response().session_id
        );
        assert_eq!(
            response.headers()["cinemashare-incarnation-id"],
            decoded.incarnation_id().to_string()
        );
        assert_eq!(
            response.headers()["cinemashare-reference"],
            serde_json::to_string(&fixture.reference).expect("full ref")
        );
        let playlist = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("real playlist bytes");
        plurx_core::sharing_resources::validate_sharing_playlist(
            &plurx_core::sharing_resources::SharingHlsResource::parse(if native {
                "video.m3u8"
            } else {
                "index.m3u8"
            })
            .expect("path"),
            &playlist,
        )
        .expect("closed actual playlist");
        let playlist = std::str::from_utf8(&playlist).expect("playlist text");
        let segment = playlist
            .lines()
            .find(|line| !line.starts_with('#') && !line.is_empty())
            .expect("real media URI");
        for resource in ["init.mp4", segment] {
            recipe["resource"] = json!(resource);
            let response = actual_resource_request(
                address,
                h2,
                &url,
                fixture.headers.clone(),
                serde_json::to_vec(&recipe).expect("request"),
            )
            .await;
            if response.status() != StatusCode::OK {
                let status = response.status();
                let body = axum::body::to_bytes(response.into_body(), 128 * 1024)
                    .await
                    .expect("bounded failed resource body");
                eprintln!("Source failed resource diagnostic begins: resource={resource} h2={h2} status={status} body={}", String::from_utf8_lossy(&body));
                let retained = tokio::time::timeout(
                    Duration::from_secs(2),
                    fixture
                        .state
                        .transcode
                        .hls_session_status(&decoded.response().session_id),
                )
                .await;
                let retained = match retained {
                    Ok(value) => {
                        serde_json::to_value(value).expect("retained HLS diagnostic serialization")
                    }
                    Err(_) => json!({"diagnostic_timeout":"retained_status"}),
                };
                eprintln!("Source retained diagnostic completed: {retained}");
                let members = tokio::time::timeout(
                    Duration::from_secs(2),
                    fixture.state.membership.observe_source_admission_members(),
                )
                .await;
                panic!("actual Source resource {resource} h2={h2} refused: {status}; body={}; retained={retained:?}; current_member_floor={:?}",
                        String::from_utf8_lossy(&body), members.as_ref().map(|value| value.as_ref().map(|observation| observation.is_some())));
            }
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["cinemashare-resource"], resource);
            let len = response.headers()["content-length"]
                .to_str()
                .expect("len")
                .parse::<usize>()
                .expect("exact len");
            assert!(response.headers().contains_key("etag"));
            let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
                .await
                .expect("actual streamed file bytes");
            assert_eq!(bytes.len(), len);
            assert!(bytes.len() > 8);
            assert!(
                bytes.windows(4).any(|part| part
                    == if resource == "init.mp4" {
                        b"ftyp"
                    } else {
                        b"moof"
                    }),
                "actual FFmpeg MP4 object"
            );
        }
        if native {
            for resource in [
                "master.m3u8?subtitle=0",
                "subs/0/index.m3u8",
                "subs/0/seg00000.vtt",
                "subs/1/index.m3u8",
                "subs/1/seg00000.vtt",
            ] {
                recipe["resource"] = json!(resource);
                let response = actual_resource_request(
                    address,
                    h2,
                    &url,
                    fixture.headers.clone(),
                    serde_json::to_vec(&recipe).expect("native request"),
                )
                .await;
                assert_eq!(response.status(), StatusCode::OK, "native {resource}");
                assert_eq!(response.headers()["cinemashare-resource"], resource);
                let mime = response.headers()["content-type"]
                    .to_str()
                    .expect("actual MIME")
                    .to_owned();
                let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
                    .await
                    .expect("actual native bytes");
                if resource.ends_with(".vtt") {
                    assert_eq!(mime, "text/vtt");
                    let text = std::str::from_utf8(&bytes).expect("actual VTT UTF8");
                    assert!(text.starts_with("WEBVTT"));
                    assert!(text.contains("X-TIMESTAMP-MAP"));
                    assert!(text.contains(if resource.starts_with("subs/1/") {
                        "Alternative HTTP Source caption"
                    } else {
                        "Actual HTTP Source caption"
                    }));
                } else {
                    if resource.starts_with("master.") {
                        assert!(
                            std::str::from_utf8(&bytes)
                                .expect("actual master")
                                .contains("subs/1/index.m3u8"),
                            "wrong selected track exists and is actually advertised"
                        );
                    }
                    assert_eq!(mime, "application/vnd.apple.mpegurl");
                    plurx_core::sharing_resources::validate_sharing_playlist(
                        &plurx_core::sharing_resources::SharingHlsResource::parse(resource)
                            .expect("closed native path"),
                        &bytes,
                    )
                    .expect("closed actual native playlist");
                }
            }
            for resource in [
                "master.m3u8?subtitle=1",
                "subs/2/index.m3u8",
                "subs/2/seg00000.vtt",
            ] {
                recipe["resource"] = json!(resource);
                assert_eq!(
                    actual_resource_request(
                        address,
                        h2,
                        &url,
                        fixture.headers.clone(),
                        serde_json::to_vec(&recipe).expect("wrong frozen selector")
                    )
                    .await
                    .status(),
                    StatusCode::UNPROCESSABLE_ENTITY
                );
            }
        }
        recipe["resource"] = json!("../init.mp4");
        assert_eq!(
            actual_resource_request(
                address,
                h2,
                &url,
                fixture.headers.clone(),
                serde_json::to_vec(&recipe).expect("bad URI")
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
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
    if native {
        actual_vtt_writer_end(&fixture, &entry, &owned, &recipe, &request).await;
    }
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
async fn actual_vtt_writer_end(
    fixture: &RealSourceStartFixture,
    entry: &std::sync::Arc<SourceStartEntry>,
    owned: &SourceStartOwned,
    recipe: &Value,
    request_id: &str,
) {
    use std::{
        sync::{atomic::Ordering, Arc},
        time::Duration,
    };
    let probe = Arc::new(AcceptedWriterProbe::default());
    probe.gate.store(true, Ordering::SeqCst);
    probe.vtt.store(true, Ordering::SeqCst);
    *probe.actor.lock().expect("actual actor") = Some(owned.actor.clone());
    let (capture, captured) = tokio::sync::oneshot::channel();
    let capture = Arc::new(std::sync::Mutex::new(Some(capture)));
    let app = super::super::sharing::peer_router((*fixture.state).clone()).layer(
        axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let capture = Arc::clone(&capture);
                async move {
                    if request.uri().path().ends_with("/resources") {
                        capture
                            .lock()
                            .expect("capture")
                            .take()
                            .expect("one actual VTT")
                            .send(
                                request
                                    .extensions()
                                    .get::<crate::SharingConnectionCancellation>()
                                    .expect("actual accepted transport")
                                    .clone(),
                            )
                            .ok();
                    }
                    next.run(request).await
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("VTT listener");
    let address = listener.local_addr().expect("address");
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(crate::serve_http(
        GatedStartListener {
            listener,
            probe: Arc::clone(&probe),
        },
        app,
        async move {
            let _ = stopped.await;
        },
        crate::HTTP_TIMEOUTS,
    ));
    let path = format!(
        "/sharing/v1/items/{}/files/{}/sessions/{request_id}",
        fixture.reference.item_id.as_str(),
        fixture.reference.file_id.as_str()
    );
    let mut recipe = recipe.clone();
    recipe["resource"] = json!("subs/0/seg00000.vtt");
    let socket = tokio::net::TcpStream::connect(address)
        .await
        .expect("real VTT TCP");
    let (mut sender, driver) =
        hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
            .handshake::<_, axum::body::Body>(hyper_util::rt::TokioIo::new(socket))
            .await
            .expect("actual H2");
    let driver = tokio::spawn(driver);
    let mut request = axum::http::Request::builder()
        .method("POST")
        .uri(format!("http://fixture{path}/resources"))
        .body(axum::body::Body::from(
            serde_json::to_vec(&recipe).expect("actual VTT request"),
        ))
        .expect("request");
    *request.headers_mut() = fixture.headers.clone();
    let send = tokio::spawn(async move { sender.send_request(request).await });
    let connection = captured.await.expect("actual accepted VTT connection");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !probe.blocked.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual VTT DATA at blocked accepted writer");
    assert!(probe.queued_start.load(Ordering::SeqCst));
    assert!(!probe.dropped.load(Ordering::SeqCst));
    assert!(!connection.closed().is_closed());
    assert_eq!(owned.actor.settlement_status(), None);
    recipe.as_object_mut().expect("recipe").remove("resource");
    let end_url = format!("http://{address}{path}/end");
    let headers = fixture.headers.clone();
    let ending = tokio::spawn(async move {
        reqwest::Client::builder()
            .http1_only()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("End client")
            .post(end_url)
            .headers(headers)
            .header("connection", "close")
            .body(serde_json::to_vec(&recipe).expect("exact End recipe"))
            .send()
            .await
            .expect("actual End response")
    });
    tokio::time::timeout(Duration::from_secs(8), connection.drain_token().cancelled())
        .await
        .expect("End requests graceful writer drain");
    assert!(!connection.closed().is_closed());
    assert_ne!(owned.actor.settlement_status(), Some(Ok(())));
    probe.gate.store(false, Ordering::SeqCst);
    probe.writer_waker.wake();
    let response = tokio::time::timeout(Duration::from_secs(15), ending)
        .await
        .expect("actual VTT writer and physical settlement")
        .expect("End task");
    let status = response.status();
    if status != StatusCode::OK {
        panic!(
            "End response {status}: {}",
            response.text().await.expect("End failure body")
        );
    }
    assert!(entry
        .ending
        .lock()
        .expect("retained actual End owner")
        .is_some());
    assert!(
        connection.closed().is_closed(),
        "actual VTT accepted writer joined before End receipt"
    );
    assert!(probe.dropped.load(Ordering::SeqCst));
    assert!(
        !probe.settled_before_drop.load(Ordering::SeqCst),
        "Source must not settle while real VTT IO is still owned"
    );
    assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
    let receipt: Value = response.json().await.expect("actual terminal receipt");
    assert_eq!(receipt["settled"], true);
    assert_eq!(receipt["request_id"], request_id);
    assert_eq!(
        receipt["incarnation_id"],
        owned.assignment.binding().incarnation_id().to_string()
    );
    send.abort();
    let _ = send.await;
    driver.abort();
    let _ = driver.await;
    let _ = stop.send(());
    server.await.expect("VTT server").expect("shutdown");
}
#[test]
fn sharing_source_first_start_refuses_foreign_predecessor_and_control_recipe_before_prepare() {
    let value = fixture();
    assert!(validate_initial_source_start(&parse(&value).expect("initial").session).is_ok());
    for (key, field) in [
        ("previous_session_id", json!(Uuid::new_v4())),
        ("control_sequence", json!(0)),
        ("reopen_reason", json!("stall")),
    ] {
        let mut unsupported = value.clone();
        unsupported["session"][key] = field;
        let input = parse(&unsupported).expect("complete recipe remains parseable for cleanup");
        assert_eq!(
            validate_initial_source_start(&input.session),
            Err(SourceStartFailure::Unsupported)
        );
        assert!(
            input
                .canonical_recipe
                .windows(key.len())
                .any(|bytes| bytes == key.as_bytes()),
            "original recipe not stripped"
        );
    }
}
#[test]
fn sharing_source_operations_close_full_recipe_lineage_resource_and_body_bounds() {
    for id in ["0", "9223372036854775807"] {
        let mut value = fixture();
        value["reference"]["item_id"] = json!(id);
        value["reference"]["file_id"] = json!(id);
        let request = value["session"]["request_id"]
            .as_str()
            .expect("request")
            .to_owned();
        let encode = |value: &Value| serde_json::to_vec(value).expect("bounded fixture");
        assert!(parse_operation_request(&encode(&value), id, id, &request).is_ok());
        value["incarnation_id"] = json!(Uuid::new_v4());
        assert!(
            parse_operation_request(&encode(&value), id, id, &request).is_err(),
            "partial known tuple refused"
        );
        value["session_id"] = json!(Uuid::new_v4());
        value["control_epoch"] = json!(i64::MAX);
        assert!(parse_operation_request(&encode(&value), id, id, &request).is_ok());
        value["resource"] = json!("init.mp4");
        assert!(parse_resource_request(&encode(&value), id, id, &request).is_ok());
        for path in [
            "https://source/private",
            "../init.mp4",
            "%2e%2e/init.mp4",
            "/init.mp4",
            "init.mp4#fragment",
        ] {
            value["resource"] = json!(path);
            assert!(parse_resource_request(&encode(&value), id, id, &request).is_err());
        }
        value["resource"] = json!("init.mp4");
        value["control_epoch"] = json!(0);
        assert!(parse_resource_request(&encode(&value), id, id, &request).is_err());
        value["control_epoch"] = json!(1);
        value["session_id"] = json!(Uuid::nil());
        assert!(parse_resource_request(&encode(&value), id, id, &request).is_err());
        value["session_id"] = json!(Uuid::new_v4());
        value["unknown"] = json!(true);
        assert!(parse_resource_request(&encode(&value), id, id, &request).is_err());
        value.as_object_mut().expect("object").remove("unknown");
        assert!(
            parse_resource_request(&encode(&value), id, id, &Uuid::new_v4().to_string()).is_err()
        );
        let duplicate = format!(
            "{{\"resource\":\"init.mp4\",{}",
            String::from_utf8(encode(&value))
                .expect("UTF8")
                .trim_start_matches('{')
        );
        assert!(
            parse_resource_request(duplicate.as_bytes(), id, id, &request).is_err(),
            "duplicate resource refuses before authority"
        );
        assert!(parse_resource_request(&vec![b' '; 128 * 1024 + 1], id, id, &request).is_err());
        value.as_object_mut().expect("object").remove("session_id");
        value
            .as_object_mut()
            .expect("object")
            .remove("incarnation_id");
        value
            .as_object_mut()
            .expect("object")
            .remove("control_epoch");
        assert!(
            parse_resource_request(&encode(&value), id, id, &request).is_err(),
            "resource needs actual observed Source tuple"
        );
    }
}
#[test]
fn sharing_source_cleanup_hashes_are_bounded_exact_and_never_infer_missing_settlement() {
    let registry = SourceStartRegistry::default();
    let grant = Uuid::new_v4();
    let viewer = "a".repeat(64);
    let input = parse(&fixture()).expect("request");
    let (entry, _) = registry
        .register(grant, &viewer, &input, &"0".repeat(64))
        .expect("entry");
    assert!(registry
        .cleanup_entry(&"0".repeat(64), &viewer, &input)
        .is_ok());
    assert!(matches!(
        registry.cleanup_entry(&"f".repeat(64), &viewer, &input),
        Err(SourceStartFailure::Unresolved)
    ));
    assert!(matches!(
        registry.cleanup_entry(&"0".repeat(64), &"b".repeat(64), &input),
        Err(SourceStartFailure::Unresolved)
    ));
    for index in 1..8 {
        let hash = format!("{index:064x}");
        assert!(registry
            .current_entry(grant, &hash, &viewer, &input)
            .is_ok());
    }
    assert!(matches!(
        registry.current_entry(grant, &format!("{:064x}", 8), &viewer, &input),
        Err(SourceStartFailure::Capacity)
    ));
    assert!(
        registry
            .current_entry(grant, &"0".repeat(64), &viewer, &input)
            .is_ok(),
        "repeat remembered hash does not consume capacity"
    );
    assert!(!entry.actual_settled());
    assert!(entry.published.lock().expect("published").is_none());
    assert!(
        SourceStartRegistry::default()
            .cleanup_entry(&"0".repeat(64), &viewer, &input)
            .is_err(),
        "restart absence stays unresolved"
    );
    assert_eq!(registry.settled.lock().expect("cache").len(), 0);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_incomplete_h2_and_owned_read_join_before_end_receipt() {
    Box::pin(actual_source_read_join()).await;
}
async fn actual_source_read_join() {
    use std::{
        sync::{atomic::Ordering, Arc},
        time::{Duration, Instant},
    };
    let fixture = real_source_start_fixture().await;
    let response = fixture_start(
        axum::extract::State((*fixture.state).clone()),
        fixture.headers.clone(),
        axum::extract::Path((
            fixture.reference.item_id.as_str().to_owned(),
            fixture.reference.file_id.as_str().to_owned(),
        )),
        axum::body::Body::from(fixture.request.clone()),
    )
    .await
    .expect("actual Source Start");
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("real full DTO");
    let decoded = super::super::decode_source_start_response(&bytes, &fixture.reference)
        .expect("strict actual Start");
    let mut request: Value = serde_json::from_slice(&fixture.request).expect("recipe");
    let request_id = request["session"]["request_id"]
        .as_str()
        .expect("request")
        .to_owned();
    request["incarnation_id"] = json!(decoded.incarnation_id());
    request["session_id"] = json!(decoded.response().session_id);
    request["control_epoch"] = json!(
        decoded
            .response()
            .control
            .as_ref()
            .expect("control")
            .control_epoch
    );
    let known_body = serde_json::to_vec(&request).expect("known full recipe");
    request["resource"] = json!("init.mp4");
    let resource_body = serde_json::to_vec(&request).expect("resource request");
    let read_gate = Arc::new(SourceReadJobGate::default());
    let io_probe = Arc::new(AcceptedWriterProbe::default());
    let (capture, captured) = tokio::sync::oneshot::channel();
    let capture = Arc::new(std::sync::Mutex::new(Some(capture)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let app = super::super::sharing::peer_router((*fixture.state).clone())
        .layer(axum::Extension(Arc::clone(&read_gate)))
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let capture = Arc::clone(&capture);
                async move {
                    if request.uri().path().ends_with("/resources") {
                        let connection = request
                            .extensions()
                            .get::<crate::SharingConnectionCancellation>()
                            .expect("actual connection")
                            .clone();
                        capture
                            .lock()
                            .expect("capture")
                            .take()
                            .expect("one resource")
                            .send(connection)
                            .ok();
                    }
                    next.run(request).await
                }
            },
        ));
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(crate::serve_http(
        GatedStartListener {
            listener,
            probe: Arc::clone(&io_probe),
        },
        app,
        async move {
            let _ = stopped.await;
        },
        crate::HTTP_TIMEOUTS,
    ));
    let socket = tokio::net::TcpStream::connect(address).await.expect("TCP");
    let (mut sender, driver) =
        hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
            .handshake::<_, axum::body::Body>(hyper_util::rt::TokioIo::new(socket))
            .await
            .expect("real H2");
    let driver = tokio::spawn(driver);
    let path = format!(
        "/sharing/v1/items/{}/files/{}/sessions/{request_id}",
        fixture.reference.item_id.as_str(),
        fixture.reference.file_id.as_str()
    );
    let request = axum::http::Request::builder()
        .method("POST")
        .uri(format!("http://fixture{path}/resources"))
        .header(
            "authorization",
            fixture.headers.get("authorization").expect("auth"),
        )
        .header(
            "cinemashare-viewer",
            fixture.headers.get("cinemashare-viewer").expect("viewer"),
        )
        .header("content-type", "application/json")
        .body(axum::body::Body::from(resource_body))
        .expect("H2 request");
    let send = tokio::spawn(async move { sender.send_request(request).await });
    let connection = captured.await.expect("accepted resource connection");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !read_gate.entered.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual blocking read is running");
    let response = tokio::time::timeout(Duration::from_secs(5), send)
        .await
        .expect("H2 response headers")
        .expect("send task")
        .expect("actual response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cinemashare-resource"], "init.mp4");
    assert_eq!(response.headers()["cinemashare-request-id"], request_id);
    assert_eq!(response.headers()["content-type"], "video/mp4");
    assert!(!connection.closed().is_closed());
    assert!(!read_gate.complete.load(Ordering::SeqCst));
    assert!(!read_gate.file_closed.load(Ordering::SeqCst));
    let entry = fixture
        .state
        .transcode
        .source_http_starts
        .entries
        .lock()
        .expect("entry")
        .first()
        .cloned()
        .expect("actual owner");
    let owned = entry
        .wait(Instant::now() + Duration::from_secs(1))
        .await
        .expect("owner");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("End client");
    let end_url = format!("http://{address}{path}/end");
    let headers = fixture.headers.clone();
    let ending = tokio::spawn(async move {
        client
            .post(end_url)
            .headers(headers)
            .header("connection", "close")
            .body(known_body)
            .send()
            .await
            .expect("actual End")
            .bytes()
            .await
            .expect("receipt")
    });
    tokio::time::timeout(Duration::from_secs(10), connection.closed().wait())
        .await
        .expect("actual incomplete H2 connection closed");
    assert!(
        io_probe.dropped.load(Ordering::SeqCst),
        "accepted IO dropped before closure receipt"
    );
    assert!(
        !read_gate.complete.load(Ordering::SeqCst),
        "owned physical read is still parked"
    );
    assert!(
        !read_gate.file_closed.load(Ordering::SeqCst),
        "actual FD remains owned by read job"
    );
    assert_eq!(
        owned.actor.settlement_status(),
        None,
        "writer close alone cannot settle a live read job"
    );
    assert!(!ending.is_finished());
    drop(response);
    read_gate.release();
    let receipt = tokio::time::timeout(Duration::from_secs(15), ending)
        .await
        .expect("read/FD/writer joined")
        .expect("End owner");
    let receipt: Value = serde_json::from_slice(&receipt).expect("terminal receipt");
    assert_eq!(receipt["state"], "settled");
    assert_eq!(receipt["settled"], true);
    assert!(read_gate.complete.load(Ordering::SeqCst));
    assert!(read_gate.file_closed.load(Ordering::SeqCst));
    assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
    driver.abort();
    let _ = driver.await;
    let _ = stop.send(());
    server.await.expect("server").expect("shutdown");
    drop(owned);
    fixture.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_live_media_body_stops_on_revocation() {
    Box::pin(actual_source_media_revocation()).await;
}
/// The one body that keeps producing bytes after its handler returns keeps
/// its authority monitor, and replicated revocation still reaches it.
async fn actual_source_media_revocation() {
    use std::{
        sync::{atomic::Ordering, Arc},
        time::{Duration, Instant},
    };
    let fixture = real_source_start_fixture().await;
    let response = fixture_start(
        axum::extract::State((*fixture.state).clone()),
        fixture.headers.clone(),
        axum::extract::Path((
            fixture.reference.item_id.as_str().to_owned(),
            fixture.reference.file_id.as_str().to_owned(),
        )),
        axum::body::Body::from(fixture.request.clone()),
    )
    .await
    .expect("actual Source Start");
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("real full DTO");
    let decoded = super::super::decode_source_start_response(&bytes, &fixture.reference)
        .expect("strict actual Start");
    let mut request: Value = serde_json::from_slice(&fixture.request).expect("recipe");
    let request_id = request["session"]["request_id"]
        .as_str()
        .expect("request")
        .to_owned();
    request["incarnation_id"] = json!(decoded.incarnation_id());
    request["session_id"] = json!(decoded.response().session_id);
    request["control_epoch"] = json!(
        decoded
            .response()
            .control
            .as_ref()
            .expect("control")
            .control_epoch
    );
    request["resource"] = json!("init.mp4");
    let resource_body = serde_json::to_vec(&request).expect("resource request");
    let read_gate = Arc::new(SourceReadJobGate::default());
    let io_probe = Arc::new(AcceptedWriterProbe::default());
    let (capture, captured) = tokio::sync::oneshot::channel();
    let capture = Arc::new(std::sync::Mutex::new(Some(capture)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let app = super::super::sharing::peer_router((*fixture.state).clone())
        .layer(axum::Extension(Arc::clone(&read_gate)))
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let capture = Arc::clone(&capture);
                async move {
                    if request.uri().path().ends_with("/resources") {
                        let connection = request
                            .extensions()
                            .get::<crate::SharingConnectionCancellation>()
                            .expect("actual connection")
                            .clone();
                        capture
                            .lock()
                            .expect("capture")
                            .take()
                            .expect("one resource")
                            .send(connection)
                            .ok();
                    }
                    next.run(request).await
                }
            },
        ));
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(crate::serve_http(
        GatedStartListener {
            listener,
            probe: Arc::clone(&io_probe),
        },
        app,
        async move {
            let _ = stopped.await;
        },
        crate::HTTP_TIMEOUTS,
    ));
    let socket = tokio::net::TcpStream::connect(address).await.expect("TCP");
    let (mut sender, driver) =
        hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
            .handshake::<_, axum::body::Body>(hyper_util::rt::TokioIo::new(socket))
            .await
            .expect("real H2");
    let driver = tokio::spawn(driver);
    let path = format!(
        "/sharing/v1/items/{}/files/{}/sessions/{request_id}",
        fixture.reference.item_id.as_str(),
        fixture.reference.file_id.as_str()
    );
    let request = axum::http::Request::builder()
        .method("POST")
        .uri(format!("http://fixture{path}/resources"))
        .header(
            "authorization",
            fixture.headers.get("authorization").expect("auth"),
        )
        .header(
            "cinemashare-viewer",
            fixture.headers.get("cinemashare-viewer").expect("viewer"),
        )
        .header("content-type", "application/json")
        .body(axum::body::Body::from(resource_body))
        .expect("H2 request");
    let send = tokio::spawn(async move { sender.send_request(request).await });
    let connection = captured.await.expect("accepted resource connection");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !read_gate.entered.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual blocking read is running");
    let response = tokio::time::timeout(Duration::from_secs(5), send)
        .await
        .expect("H2 response headers")
        .expect("send task")
        .expect("actual response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cinemashare-resource"], "init.mp4");
    assert_eq!(response.headers()["cinemashare-request-id"], request_id);
    assert_eq!(response.headers()["content-type"], "video/mp4");
    assert!(!connection.closed().is_closed());
    assert!(!read_gate.complete.load(Ordering::SeqCst));
    assert!(!read_gate.file_closed.load(Ordering::SeqCst));
    let entry = fixture
        .state
        .transcode
        .source_http_starts
        .entries
        .lock()
        .expect("entry")
        .first()
        .cloned()
        .expect("actual owner");
    let owned = entry
        .wait(Instant::now() + Duration::from_secs(1))
        .await
        .expect("owner");
    fixture
        .state
        .store
        .revoke_share(fixture.grant, crate::state::clock_ms())
        .await
        .expect("actual grant revoke");
    tokio::time::timeout(Duration::from_secs(5), connection.0.cancelled())
        .await
        .expect("revocation closes the live media body's transport");
    tokio::time::timeout(Duration::from_secs(10), connection.closed().wait())
        .await
        .expect("accepted writer dropped");
    drop(response);
    read_gate.release();
    let actor = owned.actor.clone();
    let _ = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::spawn(async move { actor.retire().await }),
    )
    .await;
    driver.abort();
    let _ = driver.await;
    let _ = stop.send(());
    server.await.expect("server").expect("shutdown");
    drop(owned);
    fixture.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_source_http_fixture_uses_actual_startup_factory_and_full_current_file_reference() {
    let fixture = real_source_start_fixture().await;
    let (_, grant) = current_reference(&fixture.state, &fixture.headers, &fixture.reference)
        .await
        .expect("actual authenticated tuple");
    assert_eq!(grant, fixture.grant);
    let input = parse_start_request(
        &fixture.request,
        fixture.reference.item_id.as_str(),
        fixture.reference.file_id.as_str(),
    )
    .expect("complete canonical actual request");
    assert!(input.reference == fixture.reference);
    let mut stale = fixture.reference.clone();
    stale.revision = plurx_core::sharing_catalogue_details::FileRevision::parse(&"f".repeat(64))
        .expect("revision");
    assert!(current_reference(&fixture.state, &fixture.headers, &stale)
        .await
        .is_err());
    fixture.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_genuine_invitation_replays_actual_claim_and_selected_identity() {
    Box::pin(actual_source_genuine_invitation()).await;
}
async fn actual_source_genuine_invitation() {
    use plurx_core::sharing::{
        secret_hash, ClaimOutcome, Endpoint, GrantState, Invitation, SecretDomain, ShareClaim,
    };
    let recipient = Uuid::new_v4();
    let fixture = real_source_start_fixture_with(SourceFixtureMode::Copy, Some(recipient)).await;
    assert_eq!(fixture.state.node_id, fixture.selected.identity.node_id);
    let tls = plurx_core::sharing_tls::LiveNodeTls::open(
        &fixture._directory.path().join("actual-peer-tls"),
        crate::state::clock_ms() / 1000,
    )
    .expect("actual production TLS key");
    let (pin, _) = tls.status().expect("actual SPKI");
    let endpoint = Endpoint {
        ipv4: "100.127.89.2".parse().expect("bounded endpoint"),
        ipv6: None,
        ts_fqdn: "source.fixture.ts.net".into(),
        port: 32443,
        spki_sha256: pin,
    };
    // This verifies the real invitation credential and TLS key material;
    // an actual pinned network exchange belongs to the separate CGNAT drill.
    let invitation = fixture
        .invitation_for(endpoint.clone())
        .expect("actual invitation");
    let blob = invitation.encode().expect("real bootstrap blob");
    let parsed = Invitation::parse(&blob).expect("closed actual invitation");
    assert_eq!(parsed.identity.server_id, fixture.reference.server_id);
    assert_eq!(
        parsed.identity.catalogue_epoch,
        fixture.reference.catalogue_epoch
    );
    assert_eq!(parsed.endpoints, vec![endpoint]);
    let secret = plurx_core::secrets::Secret::from_cleartext(
        fixture
            .headers
            .get("authorization")
            .expect("actual grant credential")
            .to_str()
            .expect("closed header")
            .strip_prefix("CinemaShare ")
            .expect("actual scheme"),
    );
    let grant_hash = secret_hash(SecretDomain::Grant, &secret);
    let status = fixture
        .state
        .store
        .sharing_grant_status(&grant_hash)
        .await
        .expect("actual credential status")
        .expect("actual active claim");
    assert_eq!(status.invitation_id, parsed.id);
    assert_eq!(status.grant.id, fixture.grant);
    assert_eq!(status.grant.recipient_server_id, recipient);
    assert_eq!(status.grant.state, GrantState::Active);
    let replay = fixture
        .state
        .store
        .claim_share(ShareClaim {
            invitation_id: parsed.id,
            invitation_hash: secret_hash(SecretDomain::Invitation, &parsed.secret),
            claim_id: status.claim_id,
            grant_id: status.grant.id,
            recipient_server_id: recipient,
            recipient_name: status.recipient_name.clone(),
            credential_hash: grant_hash,
            now_ms: crate::state::clock_ms(),
        })
        .await
        .expect("real original claim replay");
    assert!(
        matches!(replay,ClaimOutcome::Replay(grant) if grant==status.grant),
        "bootstrap secret must match the actual consumed invitation and unchanged approved grant"
    );
    fixture.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_end_retains_actual_owner_after_disconnect_and_revoke() {
    Box::pin(actual_source_http_end()).await;
}
async fn actual_source_http_end() {
    use std::time::{Duration, Instant};
    use tokio::io::AsyncWriteExt;
    let fixture = real_source_start_fixture().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("actual private listener");
    let address = listener.local_addr().expect("address");
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(crate::serve_http(
        listener,
        super::super::sharing::peer_router((*fixture.state).clone()),
        async move {
            let _ = stopped.await;
        },
        crate::HTTP_TIMEOUTS,
    ));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");
    let path = format!(
        "http://{address}/sharing/v1/items/{}/files/{}/sessions",
        fixture.reference.item_id.as_str(),
        fixture.reference.file_id.as_str()
    );
    let started = client
        .post(&path)
        .headers(fixture.headers.clone())
        .header("connection", "close")
        .body(fixture.request.clone())
        .send()
        .await
        .expect("actual Start");
    assert_eq!(started.status(), StatusCode::OK);
    let start_bytes = started.bytes().await.expect("actual full Source envelope");
    let decoded = super::super::decode_source_start_response(&start_bytes, &fixture.reference)
        .expect("actual full strict Start");
    let mut request: Value = serde_json::from_slice(&fixture.request).expect("recipe");
    let request_id = request["session"]["request_id"]
        .as_str()
        .expect("request")
        .to_owned();
    let operation = format!("{path}/{request_id}");
    let entry = fixture
        .state
        .transcode
        .source_http_starts
        .entries
        .lock()
        .expect("registry")
        .first()
        .cloned()
        .expect("actual entry");
    let owned = entry
        .wait(Instant::now() + Duration::from_secs(1))
        .await
        .expect("actual actor");
    assert_eq!(owned.actor.settlement_status(), None);
    request["incarnation_id"] = json!(decoded.incarnation_id());
    request["session_id"] = json!(decoded.response().session_id);
    request["control_epoch"] = json!(
        decoded
            .response()
            .control
            .as_ref()
            .expect("actual control")
            .control_epoch
    );
    let known_body = serde_json::to_vec(&request).expect("exact known body");
    let status = client
        .post(format!("{operation}/status"))
        .headers(fixture.headers.clone())
        .header("connection", "close")
        .body(known_body.clone())
        .send()
        .await
        .expect("active status");
    assert_eq!(status.status(), StatusCode::OK);
    assert!(super::super::decode_source_start_response(
        &status.bytes().await.expect("active body"),
        &fixture.reference
    )
    .is_ok());
    let mut wrong = request.clone();
    wrong["session_id"] = json!(Uuid::new_v4());
    let denied = client
        .post(format!("{operation}/end"))
        .headers(fixture.headers.clone())
        .header("connection", "close")
        .body(serde_json::to_vec(&wrong).expect("wrong known"))
        .send()
        .await
        .expect("wrong End");
    assert_eq!(denied.status(), StatusCode::CONFLICT);
    assert!(entry.ending.lock().expect("End owner").is_none());
    // Hold the actual actor guard. A returned fixture HTTP response is
    // already copied client-side and cannot keep Source body debt alive.
    let (_, held) = owned
        .actor
        .open_start_response(Instant::now() + Duration::from_secs(5))
        .await
        .expect("held actual Source Body guard");
    fixture
        .state
        .store
        .revoke_share(fixture.grant, crate::state::clock_ms())
        .await
        .expect("actual grant revoke");
    let status = client
        .post(format!("{operation}/status"))
        .headers(fixture.headers.clone())
        .header("connection", "close")
        .body(known_body.clone())
        .send()
        .await
        .expect("cleanup status");
    assert_eq!(status.status(), StatusCode::OK);
    let facts: Value = status.json().await.expect("closed cleanup facts");
    assert!(facts.get("response").is_none());
    assert_eq!(facts["state"], json!("unresolved"));
    fixture
        .state
        .store
        .put_setting(plurx_core::store::keys::SHARING_ENABLED, "false")
        .await
        .expect("saved off");
    let denied = client
        .get(format!("http://{address}/sharing/v1/identity"))
        .header("connection", "close")
        .send()
        .await
        .expect("off identity");
    assert_eq!(denied.status(), StatusCode::SERVICE_UNAVAILABLE);
    let denied = client
        .post(format!("{operation}/status"))
        .headers(fixture.headers.clone())
        .header("connection", "close")
        .body(known_body.clone())
        .send()
        .await
        .expect("off status");
    assert_eq!(denied.status(), StatusCode::SERVICE_UNAVAILABLE);
    let mut socket = tokio::net::TcpStream::connect(address)
        .await
        .expect("actual End TCP");
    let end_path = format!(
        "/sharing/v1/items/{}/files/{}/sessions/{request_id}/end",
        fixture.reference.item_id.as_str(),
        fixture.reference.file_id.as_str()
    );
    let head=format!("POST {end_path} HTTP/1.1\r\nHost: fixture\r\nAuthorization: {}\r\nCinemaShare-Viewer: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",fixture.headers.get("authorization").expect("auth").to_str().expect("text"),fixture.headers.get("cinemashare-viewer").expect("viewer").to_str().expect("text"),known_body.len());
    socket.write_all(head.as_bytes()).await.expect("End header");
    socket.write_all(&known_body).await.expect("End request");
    let end_owner = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(end) = entry.ending.lock().expect("actual End task").clone() {
                break end;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached actual End owner");
    drop(socket);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(end_owner.result.lock().expect("End result").is_none());
    assert_eq!(owned.actor.settlement_status(), None);
    drop(held);
    let receipt = end_owner
        .wait(Instant::now() + Duration::from_secs(10))
        .await
        .expect("actual terminal receipt");
    assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
    let original = parse_start_request(
        &fixture.request,
        fixture.reference.item_id.as_str(),
        fixture.reference.file_id.as_str(),
    )
    .expect("exact original");
    let credential = super::super::sharing::credential(&fixture.headers)
        .expect("retained actually authenticated credential");
    let hash =
        plurx_core::sharing::secret_hash(plurx_core::sharing::SecretDomain::Grant, &credential);
    let (cached, new) = fixture
        .state
        .transcode
        .source_http_starts
        .register(
            fixture.grant,
            &viewer_hash(&fixture.headers).expect("viewer"),
            &original,
            &hash,
        )
        .expect("bookkeeping prunes only the actual settled actor");
    assert!(!new);
    assert!(std::sync::Arc::ptr_eq(&cached, &entry));
    assert!(fixture
        .state
        .transcode
        .source_http_starts
        .entries
        .lock()
        .expect("active entries")
        .is_empty());
    assert_eq!(
        fixture
            .state
            .transcode
            .source_http_starts
            .settled
            .lock()
            .expect("settled cache")
            .len(),
        1
    );

    assert_eq!(receipt.incarnation_id, decoded.incarnation_id());
    assert_eq!(
        receipt.session_id.map(|id| id.to_string()),
        Some(decoded.response().session_id.clone())
    );
    let first = client
        .post(format!("{operation}/end"))
        .headers(fixture.headers.clone())
        .header("connection", "close")
        .body(known_body.clone())
        .send()
        .await
        .expect("exact End ACK retry");
    assert_eq!(first.status(), StatusCode::OK);
    let first = first.bytes().await.expect("End receipt");
    assert_eq!(
        serde_json::from_slice::<Value>(&first).expect("receipt"),
        serde_json::to_value(&receipt).expect("actual stable receipt")
    );
    let second = client
        .post(format!("{operation}/end"))
        .headers(fixture.headers.clone())
        .header("connection", "close")
        .body(fixture.request.clone())
        .send()
        .await
        .expect("lost Start recipe cleanup retry");
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(second.bytes().await.expect("same actual receipt"), first);
    let mut wrong_headers = fixture.headers.clone();
    wrong_headers.insert(
        "cinemashare-viewer",
        "f".repeat(64).parse().expect("wrong viewer"),
    );
    let denied = client
        .post(format!("{operation}/end"))
        .headers(wrong_headers)
        .header("connection", "close")
        .body(known_body)
        .send()
        .await
        .expect("wrong cleanup principal");
    // The authenticated viewer has no matching retained route; routing
    // conceals another viewer's cleanup identity as not found.
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);
    stop.send(()).expect("stop");
    server.await.expect("server task").expect("shutdown");
    fixture.shutdown().await;
}

struct QueuedStartBytes {
    bytes: Vec<u8>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl AsRef<[u8]> for QueuedStartBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
impl Drop for QueuedStartBytes {
    fn drop(&mut self) {
        self.dropped
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
struct QueuedStartBody {
    data: Option<bytes::Bytes>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl hyper::body::Body for QueuedStartBody {
    type Data = bytes::Bytes;
    type Error = std::convert::Infallible;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        std::task::Poll::Ready(
            self.data
                .take()
                .map(|data| Ok(hyper::body::Frame::data(data))),
        )
    }
    fn is_end_stream(&self) -> bool {
        self.data.is_none()
    }
}
impl Drop for QueuedStartBody {
    fn drop(&mut self) {
        self.dropped
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
#[derive(Default)]
struct AcceptedWriterProbe {
    vtt: std::sync::atomic::AtomicBool,
    actor: std::sync::Mutex<Option<crate::transcode::source_actor::SourceViewerActor>>,
    settled_before_drop: std::sync::atomic::AtomicBool,
    gate: std::sync::atomic::AtomicBool,
    writer_waker: futures_util::task::AtomicWaker,
    blocked: std::sync::atomic::AtomicBool,
    queued_start: std::sync::atomic::AtomicBool,
    dropped: std::sync::atomic::AtomicBool,
}
struct GatedStartStream {
    stream: Option<tokio::net::TcpStream>,
    data_blocked: bool,
    probe: std::sync::Arc<AcceptedWriterProbe>,
}
impl Drop for GatedStartStream {
    fn drop(&mut self) {
        if self.data_blocked {
            if let Some(actor) = self
                .probe
                .actor
                .lock()
                .expect("actual Source actor probe")
                .as_ref()
            {
                self.probe.settled_before_drop.store(
                    actor.settlement_status() == Some(Ok(())),
                    std::sync::atomic::Ordering::SeqCst,
                );
            }
        }
        drop(self.stream.take());
        self.probe
            .dropped
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
impl tokio::io::AsyncRead for GatedStartStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(self.stream.as_mut().expect("actual accepted IO")).poll_read(cx, buffer)
    }
}
impl tokio::io::AsyncWrite for GatedStartStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let marker: &[u8] = if self.probe.vtt.load(std::sync::atomic::Ordering::SeqCst) {
            b"Actual HTTP Source caption"
        } else {
            b"incarnation_id"
        };
        let source_data = bytes.windows(marker.len()).any(|part| part == marker);
        self.probe.writer_waker.register(cx.waker());
        if self.probe.gate.load(std::sync::atomic::Ordering::SeqCst)
            && (source_data || self.data_blocked)
        {
            self.data_blocked = true;
            if source_data {
                self.probe
                    .queued_start
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            // Publish the observed DATA before the waiter sees blocked.
            self.probe
                .blocked
                .store(true, std::sync::atomic::Ordering::SeqCst);
            return std::task::Poll::Pending;
        }
        std::pin::Pin::new(self.stream.as_mut().expect("actual accepted IO")).poll_write(cx, bytes)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(self.stream.as_mut().expect("actual accepted IO")).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(self.stream.as_mut().expect("actual accepted IO")).poll_shutdown(cx)
    }
}
struct GatedStartListener {
    listener: tokio::net::TcpListener,
    probe: std::sync::Arc<AcceptedWriterProbe>,
}
impl crate::HttpAcceptor for GatedStartListener {
    type Stream = GatedStartStream;
    async fn accept(&self) -> std::io::Result<(Self::Stream, std::net::SocketAddr)> {
        let (stream, address) = self.listener.accept().await?;
        Ok((
            GatedStartStream {
                stream: Some(stream),
                data_blocked: false,
                probe: self.probe.clone(),
            },
            address,
        ))
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_queued_h1_h2_writer_closes_before_actual_settlement() {
    Box::pin(actual_queued_source_start()).await;
}
async fn actual_queued_source_start() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    use tokio::io::AsyncWriteExt;
    for h2 in [false, true] {
        let fixture = real_source_start_fixture().await;
        let probe = Arc::new(AcceptedWriterProbe::default());
        let body_dropped = Arc::new(AtomicBool::new(false));
        let bytes_dropped = Arc::new(AtomicBool::new(false));
        let (accepted, captured) = tokio::sync::oneshot::channel();
        let accepted = Arc::new(std::sync::Mutex::new(Some(accepted)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("private accepted listener");
        let address = listener.local_addr().expect("address");
        let app = super::super::sharing::peer_router((*fixture.state).clone()).layer(
            axum::middleware::from_fn({
                let probe = probe.clone();
                let body_dropped = body_dropped.clone();
                let bytes_dropped = bytes_dropped.clone();
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let probe = probe.clone();
                    let body_dropped = body_dropped.clone();
                    let bytes_dropped = bytes_dropped.clone();
                    let accepted = accepted.clone();
                    async move {
                        let connection = request
                            .extensions()
                            .get::<crate::SharingConnectionCancellation>()
                            .expect("actual accepted connection")
                            .clone();
                        accepted
                            .lock()
                            .expect("capture")
                            .take()
                            .expect("one request")
                            .send(connection)
                            .ok();
                        let response = next.run(request).await;
                        assert_eq!(response.status(), StatusCode::OK);
                        let (parts, body) = response.into_parts();
                        assert!(parts
                            .extensions
                            .get::<Arc<crate::transcode::source_actor::SourceResponseGuard>>()
                            .is_some());
                        let bytes = axum::body::to_bytes(body, 4 * 1024 * 1024)
                            .await
                            .expect("actual complete Source envelope");
                        assert!(serde_json::from_slice::<Value>(&bytes)
                            .expect("actual envelope")
                            .get("incarnation_id")
                            .is_some());
                        // Test-only controlled backpressure starts after actual
                        // production Start/guard admission. No DTO is fabricated.
                        probe.gate.store(true, Ordering::SeqCst);
                        axum::response::Response::from_parts(
                            parts,
                            axum::body::Body::new(QueuedStartBody {
                                data: Some(bytes::Bytes::from_owner(QueuedStartBytes {
                                    bytes: bytes.to_vec(),
                                    dropped: bytes_dropped,
                                })),
                                dropped: body_dropped,
                            }),
                        )
                    }
                }
            }),
        );
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(crate::serve_http(
            GatedStartListener {
                listener,
                probe: probe.clone(),
            },
            app,
            async move {
                let _ = stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        let mut socket = tokio::net::TcpStream::connect(address)
            .await
            .expect("actual TCP client");
        let path = format!(
            "/sharing/v1/items/{}/files/{}/sessions",
            fixture.reference.item_id.as_str(),
            fixture.reference.file_id.as_str()
        );
        let mut h2_tasks = None;
        if h2 {
            let (mut sender, driver) =
                hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .handshake::<_, axum::body::Body>(hyper_util::rt::TokioIo::new(socket))
                    .await
                    .expect("actual H2 handshake");
            let driver = tokio::spawn(driver);
            let request = axum::http::Request::builder()
                .method("POST")
                .uri(format!("http://fixture{path}"))
                .header(
                    "authorization",
                    fixture.headers.get("authorization").expect("auth"),
                )
                .header(
                    "cinemashare-viewer",
                    fixture.headers.get("cinemashare-viewer").expect("viewer"),
                )
                .header("content-type", "application/json")
                .body(axum::body::Body::from(fixture.request.clone()))
                .expect("actual H2 request");
            let send = tokio::spawn(async move { sender.send_request(request).await });
            h2_tasks = Some((driver, send));
        } else {
            let head = format!("POST {path} HTTP/1.1\r\nHost: fixture\r\nAuthorization: {}\r\nCinemaShare-Viewer: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",fixture.headers.get("authorization").expect("auth").to_str().expect("text"),fixture.headers.get("cinemashare-viewer").expect("viewer").to_str().expect("text"),fixture.request.len());
            socket
                .write_all(head.as_bytes())
                .await
                .expect("actual H1 head");
            socket
                .write_all(&fixture.request)
                .await
                .expect("actual H1 request");
        }
        let connection = captured.await.expect("accepted connection");
        tokio::time::timeout(Duration::from_secs(30), async {
            while !probe.blocked.load(Ordering::SeqCst) || !body_dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual Hyper drained body and blocked writer");
        // Small DATA can be copied into Hyper's own write buffer before
        // delivery. The gate passes headers and blocks only when the real
        // Source JSON is presented to the actual accepted writer.
        assert!(
            probe.queued_start.load(Ordering::SeqCst),
            "actual Source envelope remains in blocked H1/H2 writer buffer"
        );
        assert!(!probe.dropped.load(Ordering::SeqCst));
        assert!(!connection.closed().is_closed());
        let entry = fixture
            .state
            .transcode
            .source_http_starts
            .entries
            .lock()
            .expect("actual registry")
            .first()
            .cloned()
            .expect("actual owner");
        let owned = entry
            .wait(Instant::now() + Duration::from_secs(1))
            .await
            .expect("actual published owner");
        let actor = owned.actor.clone();
        assert_eq!(actor.settlement_status(), None);
        let retire = tokio::spawn(async move { actor.retire().await });
        tokio::time::timeout(Duration::from_secs(8), connection.drain_token().cancelled())
            .await
            .expect("retirement requests graceful writer drain");
        assert!(!connection.closed().is_closed());
        assert_ne!(owned.actor.settlement_status(), Some(Ok(())));
        probe.gate.store(false, Ordering::SeqCst);
        probe.writer_waker.wake();
        tokio::time::timeout(Duration::from_secs(10), connection.closed().wait())
            .await
            .expect("actual accepted writer dropped");
        assert!(
            probe.dropped.load(Ordering::SeqCst),
            "accepted IO dropped before closure receipt"
        );
        assert!(
            bytes_dropped.load(Ordering::SeqCst),
            "queued actual DATA dropped before closure receipt"
        );
        tokio::time::timeout(Duration::from_secs(10), retire)
            .await
            .expect("physical/body/SQL settlement deadline")
            .expect("retire task")
            .expect("actual settled retirement");
        assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
        if let Some((driver, send)) = h2_tasks {
            driver.abort();
            send.abort();
        }
        stop.send(()).expect("stop");
        server.await.expect("server task").expect("server shutdown");
        fixture.shutdown().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_http_start_disconnect_joins_actual_actor_and_fresh_exact_replay() {
    Box::pin(actual_source_http_start()).await;
}
async fn actual_source_http_start() {
    use std::time::{Duration, Instant};
    use tokio::io::AsyncWriteExt;
    let fixture = real_source_start_fixture().await;
    let hold = fixture
        .state
        .transcode
        .test_hold_source_software_capacity()
        .expect("actual occupied software capacity");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("private fixture listener");
    let address = listener.local_addr().expect("private address");
    let router = super::super::sharing::peer_router((*fixture.state).clone());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(crate::serve_http(
        listener,
        router,
        async move {
            let _ = stopped.await;
        },
        crate::HTTP_TIMEOUTS,
    ));
    let path = format!(
        "http://{address}/sharing/v1/items/{}/files/{}/sessions",
        fixture.reference.item_id.as_str(),
        fixture.reference.file_id.as_str()
    );
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("actual HTTP client");
    let mut initial = tokio::net::TcpStream::connect(address)
        .await
        .expect("actual initial TCP connection");
    let wire=format!("POST /sharing/v1/items/{}/files/{}/sessions HTTP/1.1\r\nHost: {address}\r\nAuthorization: {}\r\nCinemaShare-Viewer: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",fixture.reference.item_id.as_str(),fixture.reference.file_id.as_str(),fixture.headers.get("authorization").expect("auth").to_str().expect("auth text"),fixture.headers.get("cinemashare-viewer").expect("viewer").to_str().expect("viewer text"),fixture.request.len());
    initial
        .write_all(wire.as_bytes())
        .await
        .expect("actual start headers");
    initial
        .write_all(&fixture.request)
        .await
        .expect("actual start body");
    initial.flush().await.expect("actual start sent");
    let deadline = Instant::now() + Duration::from_secs(10);
    let owned = loop {
        let entry = fixture
            .state
            .transcode
            .source_http_starts
            .entries
            .lock()
            .expect("registry")
            .first()
            .cloned();
        if let Some(entry) = entry {
            let outcome = entry.result.lock().expect("actual owner outcome").clone();
            if let Some(result) = outcome {
                break result.expect("actual Source actor inserted");
            }
        }
        assert!(
            Instant::now() < deadline,
            "actual HTTP owner must be retained before cancelled waiter"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(fixture.state.transcode.test_software_threads_in_use(), 4);
    assert!(
        fixture
            .state
            .store
            .media_session_route_by_incarnation(
                &owned.assignment.binding().incarnation_id().to_string()
            )
            .await
            .expect("actual route read")
            .is_none(),
        "occupied admission has no published Source route"
    );
    drop(initial); // Actual TCP EOF drops only the HTTP waiter.
    assert_eq!(
        fixture
            .state
            .transcode
            .source_http_starts
            .entries
            .lock()
            .expect("registry")
            .len(),
        1
    );
    drop(hold);
    let response = tokio::time::timeout(
        Duration::from_secs(20),
        client
            .post(&path)
            .headers(fixture.headers.clone())
            .body(fixture.request.clone())
            .send(),
    )
    .await
    .expect("actual retry deadline")
    .expect("actual HTTP retry");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .expect("private response"),
        "no-store"
    );
    let bytes = response.bytes().await.expect("guarded full Source body");
    let decoded = super::super::decode_source_start_response(&bytes, &fixture.reference)
        .expect("actual complete strict Source envelope");
    assert_eq!(
        decoded.incarnation_id(),
        owned.assignment.binding().incarnation_id()
    );
    let decoded_start = decoded.response();
    assert_eq!(
        decoded_start
            .control
            .as_ref()
            .expect("actual controller")
            .generation,
        decoded.incarnation_id().to_string()
    );
    let route = fixture
        .state
        .store
        .media_session_route_by_incarnation(&decoded.incarnation_id().to_string())
        .await
        .expect("actual route read")
        .expect("actual producer publication");
    assert_eq!(route.session_id, decoded_start.session_id);
    assert_eq!(route.publication_ready_at_ms, 0);
    let retry = client
        .post(&path)
        .headers(fixture.headers.clone())
        .body(fixture.request.clone())
        .send()
        .await
        .expect("exact metadata replay");
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(retry.bytes().await.expect("replay body"), bytes);
    let mut different: Value = serde_json::from_slice(&fixture.request).expect("canonical recipe");
    different["session"]["start"] = serde_json::json!(1);
    let conflict = client
        .post(&path)
        .headers(fixture.headers.clone())
        .body(serde_json::to_vec(&different).expect("changed recipe"))
        .send()
        .await
        .expect("conflict HTTP");
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    // A benign rotation authenticates the same stable grant and joins the
    // same actual Source owner; a rotating hash never keys its identity.
    let replacement = plurx_core::sharing::new_secret().expect("rotation credential");
    let new_hash =
        plurx_core::sharing::secret_hash(plurx_core::sharing::SecretDomain::Grant, &replacement);
    let (old_hash, _) = current_reference(&fixture.state, &fixture.headers, &fixture.reference)
        .await
        .expect("old current credential");
    assert_eq!(
        fixture
            .state
            .store
            .rotate_share(
                fixture.grant,
                Uuid::new_v4(),
                &old_hash,
                &new_hash,
                crate::state::clock_ms()
            )
            .await
            .expect("actual rotation"),
        plurx_core::sharing::MutationOutcome::Applied
    );
    let mut rotated = fixture.headers.clone();
    rotated.insert(
        "authorization",
        format!("CinemaShare {}", replacement.expose())
            .parse()
            .expect("new auth"),
    );
    let after_rotation = client
        .post(&path)
        .headers(rotated.clone())
        .body(fixture.request.clone())
        .send()
        .await
        .expect("rotated exact retry");
    assert_eq!(after_rotation.status(), StatusCode::OK);
    assert_eq!(after_rotation.bytes().await.expect("rotated body"), bytes);
    // An empty manager registry is not readiness or no-activation proof.
    // Keep the original real producer alive, but model lost in-process
    // HTTP/worker ownership with a new actual manager over the same Store.
    let mut untracked = (*fixture.state).clone();
    untracked.transcode = std::sync::Arc::new(crate::transcode::TranscodeManager::new(
        std::sync::Arc::clone(&untracked.store),
        fixture._directory.path().join("untracked-workers"),
        plurx_core::transcode::EncoderCaps::default(),
        plurx_core::transcode::Pipeline::Cpu,
    ));
    let unknown = Box::pin(fixture_start(
        axum::extract::State(untracked.clone()),
        rotated.clone(),
        axum::extract::Path((
            fixture.reference.item_id.as_str().to_owned(),
            fixture.reference.file_id.as_str().to_owned(),
        )),
        axum::body::Body::from(fixture.request.clone()),
    ))
    .await
    .expect_err("stored response without actual owner must refuse");
    use axum::response::IntoResponse;
    assert_eq!(
        unknown.into_response().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(untracked.transcode.test_software_threads_in_use(), 0);
    assert_eq!(owned.actor.settlement_status(), None);
    assert_eq!(
        untracked
            .transcode
            .source_http_starts
            .entries
            .lock()
            .expect("unresolved registry")
            .len(),
        1
    );
    // Hold the actual returned HTTP Body, not an invented publication
    // flag. The actor cannot settle physical/body/SQL obligations while
    // the complete Start transport still owns this response guard.
    let held = Box::pin(fixture_start(
        axum::extract::State((*fixture.state).clone()),
        rotated.clone(),
        axum::extract::Path((
            fixture.reference.item_id.as_str().to_owned(),
            fixture.reference.file_id.as_str().to_owned(),
        )),
        axum::body::Body::from(fixture.request.clone()),
    ))
    .await
    .expect("actual guarded HTTP response");
    let retiring = owned.actor.clone();
    let retirement = tokio::spawn(async move { retiring.retire().await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !retirement.is_finished(),
        "actual returned Start Body keeps retirement barrier"
    );
    assert_eq!(owned.actor.settlement_status(), None);
    fixture
        .state
        .store
        .revoke_share(fixture.grant, crate::state::clock_ms())
        .await
        .expect("actual revoke");
    let revoked = client
        .post(&path)
        .headers(rotated)
        .body(fixture.request.clone())
        .send()
        .await
        .expect("revoked retry");
    assert!(!revoked.status().is_success());
    drop(held);
    tokio::time::timeout(Duration::from_secs(15), retirement)
        .await
        .expect("physical settlement deadline")
        .expect("detached retirement task")
        .expect("actual physical/body/SQL settlement");
    assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
    assert_eq!(fixture.state.transcode.test_software_threads_in_use(), 0);
    let _ = stop.send(());
    server
        .await
        .expect("server join")
        .expect("actual server shutdown");
    drop((owned, client));
    fixture.shutdown().await;
}
fn fixture() -> Value {
    json!({"reference":{"server_id":Uuid::new_v4(),"catalogue_epoch":Uuid::new_v4(),"library_id":"0","item_id":"9007199254740993","file_id":"9223372036854775807","revision":"a".repeat(64)},"session":{"playback_id":"actual-client-instance","request_id":Uuid::new_v4(),"copy":true,"presentation":"vod","start":0,"caps":{"v":2,"video":[{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"]}}})
}
fn parse(value: &Value) -> Result<SourceStartInput, ApiError> {
    parse_start_request(
        &serde_json::to_vec(value).expect("canonical JSON"),
        "9007199254740993",
        "9223372036854775807",
    )
}
#[test]
fn sharing_source_start_request_preserves_complete_actual_create_session_and_exact_source_ids() {
    let value = fixture();
    let input = parse(&value).expect("actual Source start body");
    assert_eq!(input.reference.library_id.as_str(), "0");
    assert_eq!(input.reference.item_id.as_str(), "9007199254740993");
    assert_eq!(input.reference.file_id.as_str(), "9223372036854775807");
    assert_eq!(
        input.session.request_id.as_deref(),
        Some(input.request_id.to_string().as_str())
    );
    assert_eq!(
        input.canonical_recipe,
        serde_json::to_vec(&value).expect("recipe")
    );
}
#[test]
fn sharing_source_start_request_refuses_unknown_nested_fields_duplicates_and_path_drift() {
    let value = fixture();
    for path in ["", "/session", "/session/caps", "/session/caps/video/0"] {
        let mut bad = value.clone();
        let field = if path.is_empty() {
            &mut bad
        } else {
            bad.pointer_mut(path).expect("path")
        };
        field["foreign_authority"] = json!(true);
        assert!(parse(&bad).is_err(), "unknown {path}");
    }
    assert!(parse_start_request(
        &serde_json::to_vec(&value).expect("JSON"),
        "1",
        "9223372036854775807"
    )
    .is_err());
    let duplicate = format!(
        "{{\"reference\":{},\"session\":{},\"session\":{}}}",
        value["reference"], value["session"], value["session"]
    );
    assert!(parse_start_request(
        duplicate.as_bytes(),
        "9007199254740993",
        "9223372036854775807"
    )
    .is_err());
    let mut bad = value;
    bad["session"]["request_id"] = json!(Uuid::nil());
    assert!(parse(&bad).is_err());
}
#[test]
fn sharing_source_start_viewer_requires_single_sensitive_hash_shape() {
    let mut headers = HeaderMap::new();
    headers.insert("cinemashare-viewer", "a".repeat(64).parse().expect("hash"));
    assert_eq!(viewer_hash(&headers).expect("viewer"), "a".repeat(64));
    headers.append(
        "cinemashare-viewer",
        "b".repeat(64).parse().expect("duplicate"),
    );
    assert!(viewer_hash(&headers).is_err());
    for invalid in ["A".repeat(64), "g".repeat(64), "a".repeat(63)] {
        headers.remove("cinemashare-viewer");
        headers.insert("cinemashare-viewer", invalid.parse().expect("header"));
        assert!(viewer_hash(&headers).is_err());
    }
}
#[test]
fn sharing_source_start_registry_binds_full_identity_and_keeps_owner_after_waiter_drop() {
    let value = fixture();
    let input = parse(&value).expect("request");
    let grant = Uuid::new_v4();
    let viewer = "a".repeat(64);
    let registry = SourceStartRegistry::default();
    let (first, new) = registry
        .register(grant, &viewer, &input, &"d".repeat(64))
        .expect("first");
    assert!(new);
    let pointer = std::sync::Arc::as_ptr(&first);
    drop(first);
    let (retry, new) = registry
        .register(grant, &viewer, &input, &"d".repeat(64))
        .expect("retry");
    assert!(!new);
    assert_eq!(std::sync::Arc::as_ptr(&retry), pointer);
    let mut changed = value.clone();
    changed["session"]["quality_auto"] = json!(true);
    assert!(matches!(
        registry.register(
            grant,
            &viewer,
            &parse(&changed).expect("changed body"),
            &"d".repeat(64)
        ),
        Err(SourceStartFailure::Conflict)
    ));
    let mut changed = value;
    changed["reference"]["revision"] = json!("b".repeat(64));
    assert!(matches!(
        registry.register(
            grant,
            &viewer,
            &parse(&changed).expect("changed reference"),
            &"d".repeat(64)
        ),
        Err(SourceStartFailure::Conflict)
    ));
    assert!(
        registry
            .register(Uuid::new_v4(), &viewer, &input, &"d".repeat(64))
            .expect("other grant")
            .1
    );
    assert!(
        registry
            .register(grant, &"b".repeat(64), &input, &"d".repeat(64))
            .expect("other viewer")
            .1
    );
    let second = SourceStartRegistry::default();
    let (other, new) = second
        .register(grant, &viewer, &input, &"d".repeat(64))
        .expect("independent registry");
    assert!(new);
    assert!(!std::sync::Arc::ptr_eq(&retry, &other));
}
#[test]
fn sharing_source_start_registry_refuses_ninth_retained_owner_without_evicting_unknown_requests() {
    let registry = SourceStartRegistry::default();
    let grant = Uuid::new_v4();
    let viewer = "a".repeat(64);
    let mut value = fixture();
    for _ in 0..8 {
        value["session"]["request_id"] = json!(Uuid::new_v4());
        let input = parse(&value).expect("request");
        assert!(
            registry
                .register(grant, &viewer, &input, &"d".repeat(64))
                .expect("bounded entry")
                .1
        );
    }
    value["session"]["request_id"] = json!(Uuid::new_v4());
    assert!(matches!(
        registry.register(
            grant,
            &viewer,
            &parse(&value).expect("ninth"),
            &"d".repeat(64)
        ),
        Err(SourceStartFailure::Capacity)
    ));
    assert_eq!(registry.entries.lock().expect("entries").len(), 8);
}
#[test]
fn sharing_source_start_registry_moves_a_joined_failed_start_to_the_settled_cache() {
    let registry = SourceStartRegistry::default();
    let grant = Uuid::new_v4();
    let viewer = "a".repeat(64);
    let mut value = fixture();
    let mut retained = Vec::new();
    for _ in 0..8 {
        value["session"]["request_id"] = json!(Uuid::new_v4());
        let input = parse(&value).expect("request");
        let (entry, new) = registry
            .register(grant, &viewer, &input, &"d".repeat(64))
            .expect("bounded entry");
        assert!(new);
        retained.push((input, entry));
    }
    let (failed_input, failed) = retained.remove(0);
    // The supervisor publishes an outcome only after its worker joined.
    // A failure there handed no actor to this entry, so nothing in this
    // process still works for it.
    *failed.task.joined.lock().expect("joined failed supervisor") =
        Some(SourceStartTaskJoined::Returned);
    *failed.result.lock().expect("Source HTTP outcome") =
        Some(Err(SourceStartFailure::Unavailable));
    value["session"]["request_id"] = json!(Uuid::new_v4());
    assert!(
        registry
            .register(
                grant,
                &viewer,
                &parse(&value).expect("ninth"),
                &"d".repeat(64)
            )
            .expect("the failed start's slot is free")
            .1
    );
    assert_eq!(registry.entries.lock().expect("entries").len(), 8);
    assert_eq!(registry.settled.lock().expect("cache").len(), 1);
    let (replay, new) = registry
        .register(grant, &viewer, &failed_input, &"d".repeat(64))
        .expect("the failed receipt still answers its request");
    assert!(!new);
    assert!(std::sync::Arc::ptr_eq(&replay, &failed));
}
