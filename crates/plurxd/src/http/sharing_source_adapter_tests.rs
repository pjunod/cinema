use super::tests::actual_resource_request;
use super::*;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_actor_adapters_actual_h1_h2_control_and_private_metrics() {
    Box::pin(actual_actor_adapters(false)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_source_actor_adapters_actual_encoded_h1_h2_control_and_private_metrics() {
    Box::pin(actual_actor_adapters(true)).await;
}
// Failure-only observations: never renew authority or issue another control.
async fn actor_control_failure_context(
    response: &mut axum::response::Response,
    fixture: &RealSourceStartFixture,
    actor: &crate::transcode::source_actor::SourceViewerActor,
    session_id: &str,
    index: usize,
    h2: bool,
    phase: &str,
    sequence: usize,
) -> String {
    use futures_util::FutureExt;
    let status = response.status().as_u16();
    let snapshot = std::panic::AssertUnwindSafe(async {
        let body = std::mem::replace(response.body_mut(), axum::body::Body::empty());
        let code = axum::body::to_bytes(body, 128 * 1024)
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| value.get("code").and_then(Value::as_str).map(str::to_owned));
        // Only closed error vocabulary is retained; never print arbitrary body data.
        let code = match code.as_deref() {
            Some(code @ ("sharing_start_unavailable" | "sharing_start_unresolved"
                | "sharing_start_deadline" | "sharing_start_conflict"
                | "sharing_playback_authority_unavailable" | "sharing_body_authority_unavailable"
                | "sharing_transport_authority_unavailable" | "sharing_invalid_request"
                | "sharing_body_capacity" | "sharing_connection_authority_capacity")) => code,
            Some(_) => "other_error_code",
            None => "no_error_code",
        };
        // wait_ready only reads retained state; a single poll cannot wait or renew.
        let readiness = actor.wait_ready(std::time::Instant::now()).now_or_never();
        let readiness = match readiness {
            Some(Ok(_)) => "ready".to_owned(),
            Some(Err(error)) => format!("{error:?}"),
            None => "pending".to_owned(),
        };
        let vod = fixture.state.transcode.vod_for_test();
        let recovered = vod.recovered_start(session_id).await.is_some();
        let facts = vod.hls_facts(session_id).await;
        let (owner_live, physical_fence) = if let Some(facts) = facts.as_ref() {
            let live = vod.response_owner_is_live(session_id, &facts.response_owner).await;
            let fence = match vod.source_response_physical_fence(&facts.response_owner).await {
                Ok(_) => "current",
                Err(error) => match error.as_str() {
                    "Source rendition is absent" => "rendition_absent",
                    "Source response has no Source rendition" => "not_source_rendition",
                    "Source held input is absent" => "held_input_absent",
                    "Source held input changed" => "held_input_changed",
                    "Source held input changed during response admission" => "held_input_changed_during_admission",
                    _ => "physical_fence_error",
                },
            };
            (Some(live), fence)
        } else {
            (None, "facts_absent")
        };
        let observation = vod.source_control_observation_for_test(session_id).await;
        json!({
            "index": index, "h2": h2, "phase": phase, "request_sequence": sequence,
            "expected_status": 200, "actual_status": status, "error_code": code,
            "actor_readiness": readiness,
            // No existing read-only API exposes the retirement flag or accepted sequence.
            "actor_settlement": format!("{:?}", actor.settlement_status()),
            "actor_retirement_requested": Value::Null, "accepted_sequence": Value::Null,
            "worker_count": fixture.state.transcode.source_worker_count(),
            "vod_recovered": recovered, "vod_facts": facts.is_some(),
            "response_owner_live": owner_live, "physical_fence": physical_fence,
            "control_observation": observation.as_ref().map(|(snapshot, touched)| json!({
                "last_touch_age_ms": touched.elapsed().as_millis(),
                "demand": snapshot.as_ref().map(|snapshot| format!("{:?}", snapshot.demand)),
                "position_ms": snapshot.as_ref().map(|snapshot| snapshot.position_ms),
                "buffered_through_ms": snapshot.as_ref().map(|snapshot| snapshot.buffered_through_ms),
            })),
        }).to_string()
    }).catch_unwind();
    // Diagnostic failure cannot replace the original status assertion.
    match tokio::time::timeout(std::time::Duration::from_millis(250), snapshot).await {
        Ok(Ok(context)) => context,
        outcome => json!({
            "index": index, "h2": h2, "phase": phase, "request_sequence": sequence,
            "expected_status": 200, "actual_status": status,
            "snapshot_error": if outcome.is_err() { "timeout" } else { "panic" },
        }).to_string(),
    }
}
async fn actual_actor_adapters(encoded: bool) {
    let mut fixture = real_source_start_fixture_with(
        if encoded {
            SourceFixtureMode::Encoded
        } else {
            SourceFixtureMode::Copy
        },
        None,
    )
    .await;
    if encoded {
        let mut recipe: Value =
            serde_json::from_slice(&fixture.request).expect("real encoded recipe");
        recipe["session"]["height"] = json!(144);
        fixture.request = serde_json::to_vec(&recipe).expect("admitted raw manual selection");
    }
    let started = fixture_start(
        axum::extract::State((*fixture.state).clone()),
        fixture.headers.clone(),
        axum::extract::Path((
            fixture.reference.item_id.as_str().to_owned(),
            fixture.reference.file_id.as_str().to_owned(),
        )),
        axum::body::Body::from(fixture.request.clone()),
    )
    .await
    .expect("actual Source actor");
    let bytes = axum::body::to_bytes(started.into_body(), 4 * 1024 * 1024)
        .await
        .expect("Start body");
    let decoded = crate::http::decode_source_start_response(&bytes, &fixture.reference)
        .expect("complete Start");
    let mut body: Value = serde_json::from_slice(&fixture.request).expect("original recipe");
    let request = body["session"]["request_id"]
        .as_str()
        .expect("request")
        .to_owned();
    body["incarnation_id"] = json!(decoded.incarnation_id());
    body["session_id"] = json!(decoded.response().session_id);
    let epoch = decoded
        .response()
        .control
        .as_ref()
        .expect("bootstrap")
        .control_epoch;
    body["control_epoch"] = json!(epoch);
    let input = parse_operation_request(
        &serde_json::to_vec(&body).expect("actual adapter fixture observation"),
        fixture.reference.item_id.as_str(),
        fixture.reference.file_id.as_str(),
        &request,
    )
    .expect("known full tuple");
    // Observe the actual retained owner established by the real HTTP Start.
    // This private fixture lookup opens no media response and creates no new
    // transport obligation; all public operations below use accepted Hyper.
    let source_headers: SourceHeaders = fixture.headers.clone().into();
    let viewer = viewer_hash(&source_headers).expect("actual original viewer");
    let (hash, grant) = current_reference(&fixture.state, &source_headers, &input.start.reference)
        .await
        .expect("fresh actual Source reference");
    let entry = fixture
        .state
        .transcode
        .source_http_starts
        .current_entry(grant, &hash, &viewer, &input.start)
        .expect("actual retained invocation");
    entry
        .validate_known(input.known.as_ref())
        .expect("complete actual tuple");
    let owned = entry
        .wait(std::time::Instant::now() + std::time::Duration::from_secs(5))
        .await
        .expect("actual retained owner");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener
        .local_addr()
        .expect("actual adapter fixture observation");
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(crate::serve_http(
        listener,
        crate::http::sharing::peer_router((*fixture.state).clone()),
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
    let client = Uuid::new_v4();
    for (index, h2) in [false, true].into_iter().enumerate() {
        let response = if index == 0 {
            let pause = owned.actor.pause_status_read_for_test();
            let url = format!("{base}/vod-status");
            let headers = fixture.headers.clone();
            let bytes = serde_json::to_vec(&body).expect("complete parked metadata request");
            let read = tokio::spawn(async move {
                actual_resource_request(address, h2, &url, headers, bytes).await
            });
            let held = pause.reached().await;
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            assert!(
                !read.is_finished(),
                "actual Source status outlives management timeout"
            );
            drop(held);
            read.await.expect("owned metadata request")
        } else {
            let response = actual_resource_request(
                address,
                h2,
                &format!("{base}/vod-status"),
                fixture.headers.clone(),
                serde_json::to_vec(&body).expect("actual adapter fixture observation"),
            )
            .await;
            response
        };
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 128 * 1024)
            .await
            .expect("actual adapter fixture observation");
        let facts: Value =
            serde_json::from_slice(&bytes).expect("actual adapter fixture observation");
        assert_eq!(facts["reference"], body["reference"]);
        assert_eq!(facts["session_id"], body["session_id"]);
        assert_eq!(facts["incarnation_id"], body["incarnation_id"]);
        assert_eq!(facts["control_epoch"], epoch);
        assert!(facts["status"].get("file_id").is_none());
        assert!(facts["status"].get("producer_failed").is_none());
        assert!(
            facts["status"]["status_generated_unix_ms"]
                .as_i64()
                .expect("actual adapter fixture observation")
                > 0
        );
        let mut control = body.clone();
        let quality = if encoded {
            json!({"mode":"manual","height":144})
        } else {
            json!({"mode":"original"})
        };
        control["control"] = json!({
            "protocol":crate::playback_control::PROTOCOL_V1,"generation":decoded.incarnation_id(),"control_epoch":epoch,
            "client_instance_id":client,"sequence":index+1,"demand":"active","position_ms":0,
            "buffered_from_ms":0,"buffered_through_ms":1000,"playback_rate":1.0,
            "render_state":"seeking","seek_target_ms":1000,
            "selection":{"quality":quality,"audio_track":null,"subtitle":{"mode":"off"},"audio_offset_ms":0,"codec":"auto","dynamic_range":"auto"},
            "capabilities":{"platform":"web","max_height":2160,"codecs":["h264"],"dynamic_ranges":["sdr"],"dual_player_preparation":false},"supported_actions":[],"intent":null
        });
        let mut response = actual_resource_request(
            address,
            h2,
            &format!("{base}/control"),
            fixture.headers.clone(),
            serde_json::to_vec(&control).expect("actual adapter fixture observation"),
        )
        .await;
        let failure_context = if response.status() != StatusCode::OK {
            actor_control_failure_context(
                &mut response, &fixture, &owned.actor, &decoded.response().session_id,
                index, h2, "control", index + 1,
            ).await
        } else {
            String::new()
        };
        assert_eq!(response.status(), StatusCode::OK, "{failure_context}");
        let bytes = axum::body::to_bytes(response.into_body(), 128 * 1024)
            .await
            .expect("actual adapter fixture observation");
        let facts: Value =
            serde_json::from_slice(&bytes).expect("actual adapter fixture observation");
        let reply: crate::playback_control::ControlResponseV1 =
            serde_json::from_value(facts["response"].clone())
                .expect("bounded complete Control response");
        assert_eq!(
            reply.effective_selection.codec,
            if encoded { "server_selected" } else { "source" },
            "actual retained encoder recipe, not a planned Copy label"
        );
        assert_eq!(reply.effective_selection.height, decoded.response().height);
        assert_eq!(reply.accepted_sequence, (index + 1) as u64);
        assert_eq!(reply.generation, decoded.incarnation_id().to_string());
        assert!(facts["response"].get("file_id").is_none());
        // A definitive actor refusal is a closed code in the body, never a
        // transport status B would have to guess about.
        let mut foreign_client = control.clone();
        foreign_client["control"]["client_instance_id"] = json!(uuid::Uuid::new_v4().to_string());
        let mut response = actual_resource_request(
            address,
            h2,
            &format!("{base}/control"),
            fixture.headers.clone(),
            serde_json::to_vec(&foreign_client).expect("actual adapter fixture observation"),
        )
        .await;
        let failure_context = if response.status() != StatusCode::OK {
            actor_control_failure_context(
                &mut response, &fixture, &owned.actor, &decoded.response().session_id,
                index, h2, "foreign_control", index + 1,
            ).await
        } else {
            String::new()
        };
        assert_eq!(response.status(), StatusCode::OK, "{failure_context}");
        let bytes = axum::body::to_bytes(response.into_body(), 128 * 1024)
            .await
            .expect("actual adapter refusal");
        let refused: Value = serde_json::from_slice(&bytes).expect("actual adapter refusal");
        assert!(refused.get("response").is_none());
        assert_eq!(
            refused["refusal"],
            json!({"code":"stale_control","retry_after_ms":null})
        );
        assert_eq!(refused["session_id"], body["session_id"]);
        // A changed ask is accepted on a new sequence (answered
        // `preparation: none`); reusing the accepted one is a stale fence.
        let mut directed = control.clone();
        directed["control"]["selection"]["audio_track"] = json!(0);
        let response = actual_resource_request(
            address,
            h2,
            &format!("{base}/control"),
            fixture.headers.clone(),
            serde_json::to_vec(&directed).expect("actual adapter fixture observation"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 128 * 1024)
            .await
            .expect("actual adapter refusal");
        let refused: Value = serde_json::from_slice(&bytes).expect("actual adapter refusal");
        assert_eq!(
            refused["refusal"],
            json!({"code":"stale_control","retry_after_ms":null})
        );
        // An acknowledgement names a successor this Source never offered.
        let mut acknowledged = control.clone();
        acknowledged["control"]["sequence"] = json!(index + 2);
        acknowledged["control"]["render_state"] = json!("rendering");
        acknowledged["control"]["seek_target_ms"] = Value::Null;
        acknowledged["control"]["acknowledgement"] = json!({
            "action_id":uuid::Uuid::new_v4().to_string(),"state":"metadata_ready",
            "buffered_through_ms":null,"committed_media_origin_ms":null,"first_frame_unix_ms":null
        });
        let response = actual_resource_request(
            address,
            h2,
            &format!("{base}/control"),
            fixture.headers.clone(),
            serde_json::to_vec(&acknowledged).expect("actual adapter fixture observation"),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "an acknowledgement without a Source slot stays unsupported"
        );
        let mut changed_recipe = body.clone();
        changed_recipe["session"]["height"] = json!(216);
        let response = actual_resource_request(
            address,
            h2,
            &format!("{base}/vod-status"),
            fixture.headers.clone(),
            serde_json::to_vec(&changed_recipe).expect("actual adapter fixture observation"),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "full original recipe, not just session UUID, binds metrics"
        );
        control["control"]["control_epoch"] = json!(epoch + 1);
        let response = actual_resource_request(
            address,
            h2,
            &format!("{base}/control"),
            fixture.headers.clone(),
            serde_json::to_vec(&control).expect("actual adapter fixture observation"),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "inner foreign epoch refused"
        );
        let mut missing = body.clone();
        missing
            .as_object_mut()
            .expect("actual adapter fixture observation")
            .remove("session_id");
        let response = actual_resource_request(
            address,
            h2,
            &format!("{base}/vod-status"),
            fixture.headers.clone(),
            serde_json::to_vec(&missing).expect("actual adapter fixture observation"),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "mandatory published identity"
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    // No metadata response itself proves physical settlement.
    assert_eq!(owned.actor.settlement_status(), None);
    owned
        .actor
        .retire()
        .await
        .expect("actual body/producer/SQL retirement");
    assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
    drop(entry);
    let _ = stop.send(());
    server
        .await
        .expect("actual adapter fixture observation")
        .expect("actual adapter fixture observation");
    drop(owned);
    fixture.shutdown().await;
}
