use super::*;
use axum::body::to_bytes;
use std::sync::atomic::Ordering;
use tower::ServiceExt;

async fn login(state: &AppState, user: i64) -> String {
    let token = auth::generate_token().expect("random token");
    state
        .store
        .create_token(&auth::hash_token(&token), user, None)
        .await
        .expect("native login");
    token
}
async fn call(
    app: &axum::Router,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
    proof: Option<(&str, &str)>,
) -> (u16, Value) {
    let mut request = axum::http::Request::builder()
        .method(method)
        .uri(format!("/api/remote/v1/{path}"))
        .header("authorization", format!("Bearer {token}"));
    if let Some((key, value)) = proof {
        request = request.header(key, value);
    }
    let bytes = if method == "GET" || method == "DELETE" {
        Vec::new()
    } else {
        serde_json::to_vec(&body).expect("request JSON")
    };
    let response = app
        .clone()
        .oneshot(
            request
                .header("content-type", "application/json")
                .body(Body::from(bytes))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status().as_u16();
    let value = serde_json::from_slice(
        &to_bytes(response.into_body(), 65536)
            .await
            .expect("bounded response"),
    )
    .expect("versioned JSON");
    (status, value)
}
struct Fixture {
    app: axum::Router,
    state: AppState,
    tv: String,
    phone: String,
    target: Value,
    receiver_id: String,
    receiver_secret: String,
    user: i64,
}
async fn fixture() -> Fixture {
    let (app, state) = super::super::tests::test_app_with_state();
    let user = state
        .store
        .create_user("remote-test-user", "hash", true)
        .await
        .expect("user");
    state
        .store
        .put_setting(FEATURE_KEY, "1")
        .await
        .expect("enable");
    let tv = login(&state, user.id).await;
    let phone = login(&state, user.id).await;
    let (status, receiver) = call(
        &app,
        &tv,
        "POST",
        "receivers",
        json!({"version":"cinema.remote.v1","name":"Living room","platform":"web"}),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let receiver_id = receiver["receiver_id"].as_str().expect("id").to_owned();
    let receiver_secret = receiver["receiver_secret"]
        .as_str()
        .expect("secret")
        .to_owned();
    let (status,session)=call(&app,&tv,"POST","sessions",json!({"version":"cinema.remote.v1","receiver_id":receiver_id,"foreground_id":Uuid::new_v4()}),Some(("x-cinema-receiver-secret",&receiver_secret))).await;
    assert_eq!(status, 200);
    Fixture {
        app,
        state,
        tv,
        phone,
        target: session["target"].clone(),
        receiver_id,
        receiver_secret,
        user: user.id,
    }
}
async fn pending(f: &Fixture) -> (Value, String) {
    let (_, challenge) = call(
        &f.app,
        &f.tv,
        "POST",
        "pairing/start",
        json!({"version":"cinema.remote.v1","target":f.target}),
        Some(("x-cinema-receiver-secret", &f.receiver_secret)),
    )
    .await;
    // Manual pairing deliberately omits challenge_id: selected TV + code only.
    let (status,pending)=call(&f.app,&f.phone,"POST","pairing/claim",json!({"version":"cinema.remote.v1","target":f.target,"code":challenge["code"],"controller_name":"Phone"}),None).await;
    assert_eq!(status, 200);
    let secret = pending["poll_secret"]
        .as_str()
        .expect("claim proof")
        .to_owned();
    (pending["pending_id"].clone(), secret)
}
async fn result(f: &Fixture, id: &Value, secret: &str) -> (u16, Value) {
    call(
        &f.app,
        &f.phone,
        "POST",
        "pairing/result",
        json!({"version":"cinema.remote.v1","target":f.target,"pending_id":id}),
        Some(("x-cinema-pairing-secret", secret)),
    )
    .await
}
async fn paired(f: &Fixture) -> (Value, String, Value) {
    let (id, poll_secret) = pending(f).await;
    let (status, _) = call(
        &f.app,
        &f.tv,
        "POST",
        "pairing/approve",
        json!({"version":"cinema.remote.v1","target":f.target,"pending_id":id,"approve":true}),
        Some(("x-cinema-receiver-secret", &f.receiver_secret)),
    )
    .await;
    assert_eq!(status, 200);
    let grant = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (status, value) = result(f, &id, &poll_secret).await;
            assert_eq!(status, 200);
            if value["status"] == "approved" {
                break value;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("approval completion");
    let gid = grant["grant_id"].clone();
    let secret = grant["grant_secret"]
        .as_str()
        .expect("grant proof")
        .to_owned();
    let (status,control)=call(&f.app,&f.phone,"POST","control",json!({"version":"cinema.remote.v1","target":f.target,"grant_id":gid,"action":"acquire","control_epoch":null}),Some(("x-cinema-grant-secret",&secret))).await;
    assert_eq!(status, 200);
    assert_eq!(control["control"]["active_grant_id"], gid);
    (gid, secret, control["control"]["control_epoch"].clone())
}
fn command(f: &Fixture, gid: &Value, epoch: &Value, sequence: u64) -> Value {
    json!({"version":"cinema.remote.v1","target":f.target,"grant_id":gid,"control_epoch":epoch,"sequence":sequence,"credit":Uuid::new_v4(),"context_revision":1,"focus_revision":1,"action":{"type":"select"}})
}
async fn poll(f: &Fixture, delivery: u64, revision: u64, wait: u64) -> (u16, Value) {
    call(&f.app,&f.tv,"POST","poll",json!({"version":"cinema.remote.v1","target":f.target,"after_delivery_id":delivery,"after_response_revision":revision,"wait_ms":wait}),Some(("x-cinema-receiver-secret",&f.receiver_secret))).await
}

#[tokio::test]
async fn remote_pairing_result_stays_pending_during_approval_store_write() {
    let f = fixture().await;
    let (id, secret) = pending(&f).await;
    f.state
        .remote
        .approve_pause
        .enabled
        .store(true, Ordering::SeqCst);
    assert_eq!(
        call(
            &f.app,
            &f.tv,
            "POST",
            "pairing/approve",
            json!({"version":"cinema.remote.v1","target":f.target,"pending_id":id,"approve":true}),
            Some(("x-cinema-receiver-secret", &f.receiver_secret))
        )
        .await
        .0,
        200
    );
    f.state.remote.approve_pause.arrived.notified().await;
    assert_eq!(result(&f, &id, &secret).await.1["status"], "pending");
    assert_eq!(
        call(
            &f.app,
            &f.tv,
            "POST",
            "pairing/approve",
            json!({"version":"cinema.remote.v1","target":f.target,"pending_id":id,"approve":true}),
            Some(("x-cinema-receiver-secret", &f.receiver_secret))
        )
        .await
        .0,
        409
    );
    f.state.remote.approve_pause.resume.notify_one();
    let approved = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (_, value) = result(&f, &id, &secret).await;
            if value["status"] == "approved" {
                break value;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("approved result");
    assert!(approved["grant_secret"].is_string());
    assert_eq!(result(&f, &id, &secret).await.0, 403);
}

#[tokio::test]
async fn remote_delivery_preserves_commands_enqueued_during_authorization() {
    let f = fixture().await;
    let (gid, secret, epoch) = paired(&f).await;
    assert_eq!(
        call(
            &f.app,
            &f.phone,
            "POST",
            "commands",
            command(&f, &gid, &epoch, 1),
            Some(("x-cinema-grant-secret", &secret))
        )
        .await
        .0,
        202
    );
    f.state
        .remote
        .delivery_pause
        .enabled
        .store(true, Ordering::SeqCst);
    let app = f.app.clone();
    let tv = f.tv.clone();
    let target = f.target.clone();
    let receiver_secret = f.receiver_secret.clone();
    let first = tokio::spawn(async move {
        call(&app,&tv,"POST","poll",json!({"version":"cinema.remote.v1","target":target,"after_delivery_id":0,"after_response_revision":0,"wait_ms":0}),Some(("x-cinema-receiver-secret",&receiver_secret))).await
    });
    f.state.remote.delivery_pause.arrived.notified().await;
    assert_eq!(
        call(
            &f.app,
            &f.phone,
            "POST",
            "commands",
            command(&f, &gid, &epoch, 2),
            Some(("x-cinema-grant-secret", &secret))
        )
        .await
        .0,
        202
    );
    f.state.remote.delivery_pause.resume.notify_one();
    let (status, first) = first.await.expect("poll task");
    assert_eq!(status, 200);
    assert_eq!(first["commands"].as_array().expect("batch").len(), 1);
    assert_eq!(first["commands"][0]["sequence"], 1);
    let (_, second) = poll(
        &f,
        first["delivery_id"].as_u64().expect("delivery"),
        first["response_revision"].as_u64().expect("revision"),
        0,
    )
    .await;
    assert_eq!(second["commands"][0]["sequence"], 2);
}

#[tokio::test]
async fn remote_logout_and_grant_revocation_fence_queued_delivery() {
    let f = fixture().await;
    let (gid, secret, epoch) = paired(&f).await;
    assert_eq!(
        call(
            &f.app,
            &f.phone,
            "POST",
            "commands",
            command(&f, &gid, &epoch, 1),
            Some(("x-cinema-grant-secret", &secret))
        )
        .await
        .0,
        202
    );
    f.state
        .store
        .delete_token(&auth::hash_token(&f.phone))
        .await
        .expect("logout");
    let (status, value) = poll(&f, 0, 0, 0).await;
    assert_eq!(status, 200);
    assert!(value["control"].is_null());
    assert_eq!(value["commands"], json!([]));
    let new_phone = login(&f.state, f.user).await;
    assert_eq!(call(&f.app,&new_phone,"POST","control",json!({"version":"cinema.remote.v1","target":f.target,"grant_id":gid,"action":"acquire","control_epoch":null}),Some(("x-cinema-grant-secret",&secret))).await.0,200);
    assert_eq!(
        call(
            &f.app,
            &new_phone,
            "DELETE",
            &format!("grants/{}", gid.as_str().expect("grant id")),
            Value::Null,
            None
        )
        .await
        .0,
        200
    );
    let (_, value) = poll(&f, 0, 0, 0).await;
    assert!(value["control"].is_null());
}

#[tokio::test]
async fn remote_discovery_restricted_state_and_receiver_cleanup_preserve_authority() {
    let f = fixture().await;
    let state = json!({"state_revision":1,"context_revision":1,"focus_revision":1,"route":"restricted","capabilities":["select"],"focused_label":"private admin label","credits":[],"text_nonce":Uuid::new_v4(),"playback":{"media":{"type":"live_channel","channel_id":"news"},"title":"private title","playing":true,"position_ms":0,"duration_ms":0,"tracks":[]}});
    assert_eq!(
        call(
            &f.app,
            &f.tv,
            "POST",
            "presence",
            json!({"version":"cinema.remote.v1","target":f.target,"state":state}),
            Some(("x-cinema-receiver-secret", &f.receiver_secret))
        )
        .await
        .0,
        200
    );
    let (_, list) = call(&f.app, &f.phone, "GET", "receivers", Value::Null, None).await;
    assert!(!list.to_string().contains("private"));
    let (gid, secret, _) = paired(&f).await;
    let(status,state)=call(&f.app,&f.phone,"POST","state",json!({"version":"cinema.remote.v1","target":f.target,"grant_id":gid,"after_revision":0,"wait_ms":0}),Some(("x-cinema-grant-secret",&secret))).await;
    assert_eq!(status, 200);
    assert!(state["state"]["playback"].is_null());
    assert!(state["state"]["focused_label"].is_null());
    assert_eq!(
        call(
            &f.app,
            &f.phone,
            "DELETE",
            &format!("receivers/{}", f.receiver_id),
            Value::Null,
            None
        )
        .await
        .0,
        200
    );
    assert_eq!(poll(&f, 0, 0, 0).await.0, 404);
    assert!(f
        .state
        .store
        .remote_grants(f.user)
        .await
        .expect("grants")
        .is_empty());
}

#[test]
fn remote_header_parser_and_live_media_union_reject_alternate_credentials() {
    let uri: Uri = "/api/remote/v1/receivers".parse().expect("URI");
    let mut headers = HeaderMap::new();
    headers.insert("authorization", "Bearer native".parse().expect("header"));
    assert_eq!(bearer(&headers, &uri).expect("bearer"), "native");
    headers.insert("x-api-key", "key".parse().expect("header"));
    assert!(bearer(&headers, &uri).is_err());
    headers.remove("x-api-key");
    assert!(bearer(
        &headers,
        &"/api/remote/v1/receivers?token=native"
            .parse()
            .expect("URI")
    )
    .is_err());
    headers.insert("authorization", "Bearer plx_key".parse().expect("header"));
    assert!(bearer(&headers, &uri).is_err());
    headers.append("authorization", "Bearer native".parse().expect("header"));
    assert!(bearer(&headers, &uri).is_err());
    assert!(serde_json::from_value::<MediaIdentity>(
        json!({"type":"live_channel","channel_id":"a","item_id":1})
    )
    .is_err());
    assert!(
        serde_json::from_value::<MediaIdentity>(json!({"type":"unknown","channel_id":"a"}))
            .is_err()
    );
}

#[test]
fn remote_duplicate_owner_discovery_is_unavailable() {
    let one = json!({"receiver_id":"tv","target":{"owner_node_id":"one"}});
    let two = json!({"receiver_id":"tv","target":{"owner_node_id":"two"}});
    assert!(super::unique_summary(std::slice::from_ref(&one), "tv").is_some());
    assert!(super::unique_summary(&[one.clone(), two.clone()], "tv").is_none());
    assert!(super::unique_summary(&[two, one], "tv").is_none());
}

#[tokio::test]
async fn remote_expired_approval_revokes_orphan_grant() {
    let f = fixture().await;
    let (id, _) = pending(&f).await;
    f.state
        .remote
        .approve_pause
        .enabled
        .store(true, Ordering::SeqCst);
    let (status, _) = call(
        &f.app,
        &f.tv,
        "POST",
        "pairing/approve",
        json!({"version":"cinema.remote.v1","target":f.target,"pending_id":id,"approve":true}),
        Some(("x-cinema-receiver-secret", &f.receiver_secret)),
    )
    .await;
    assert_eq!(status, 200);
    f.state.remote.approve_pause.arrived.notified().await;
    // Pending expires while the durable create is paused; its new grant must
    // never survive without the one-time approval result.
    f.state.remote.expire_pending_for_test(
        Uuid::parse_str(f.target["session_id"].as_str().expect("string field"))
            .expect("valid UUID"),
    );
    f.state.remote.approve_pause.resume.notify_one();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if f.state.remote.approvals_finished() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("approval cleanup completion");
    let (status, grants) = call(&f.app, &f.phone, "GET", "grants", Value::Null, None).await;
    assert_eq!(status, 200);
    assert!(grants["grants"].as_array().expect("array field").is_empty());
}

#[tokio::test]
async fn remote_conditional_takeover_rejects_stale_epoch() {
    let f = fixture().await;
    let (gid, secret, epoch) = paired(&f).await;
    let body = |expected: Value| json!({"version":"cinema.remote.v1","target":f.target,"grant_id":gid,"action":"takeover","control_epoch":expected});
    let (status, _) = call(
        &f.app,
        &f.phone,
        "POST",
        "control",
        body(json!(Uuid::new_v4())),
        Some(("x-cinema-grant-secret", &secret)),
    )
    .await;
    assert_eq!(status, 409);
    let (status, value) = call(
        &f.app,
        &f.phone,
        "POST",
        "control",
        body(epoch.clone()),
        Some(("x-cinema-grant-secret", &secret)),
    )
    .await;
    assert_eq!(status, 200);
    assert_ne!(value["control"]["control_epoch"], epoch);
    assert_eq!(
        call(
            &f.app,
            &f.phone,
            "POST",
            "control",
            body(epoch),
            Some(("x-cinema-grant-secret", &secret))
        )
        .await
        .0,
        409
    );
}

#[tokio::test]
async fn remote_receiver_poll_is_bounded_and_cancellation_releases_permit() {
    let f = fixture().await;
    f.state
        .remote
        .delivery_pause
        .enabled
        .store(true, Ordering::SeqCst);
    let app = f.app.clone();
    let tv = f.tv.clone();
    let target = f.target.clone();
    let proof = f.receiver_secret.clone();
    let first = tokio::spawn(async move {
        call(&app,&tv,"POST","poll",json!({"version":"cinema.remote.v1","target":target,"after_delivery_id":0,"after_response_revision":0,"wait_ms":20000}),Some(("x-cinema-receiver-secret",&proof))).await
    });
    f.state.remote.delivery_pause.arrived.notified().await;
    assert_eq!(poll(&f, 0, 0, 0).await.0, 429);
    first.abort();
    let _ = first.await;
    assert_eq!(poll(&f, 0, 0, 0).await.0, 200);
}

#[test]
fn remote_required_nullable_fields_are_not_optional() {
    let target =
        json!({"owner_node_id":"n","session_id":Uuid::new_v4(),"receiver_epoch":Uuid::new_v4()});
    let mut control = json!({"version":"cinema.remote.v1","target":target,"grant_id":Uuid::new_v4(),"action":"acquire","control_epoch":null});
    assert!(serde_json::from_value::<wire::ControlRequest>(control.clone()).is_ok());
    control
        .as_object_mut()
        .expect("object")
        .remove("control_epoch");
    assert!(serde_json::from_value::<wire::ControlRequest>(control).is_err());
    let state = serde_json::from_value::<wire::ReceiverState>(json!({"state_revision":1,"context_revision":1,"focus_revision":1,"route":"home","capabilities":[],"credits":[]})).expect("optional presence metadata may be omitted");
    assert!(
        state.focused_label.is_none() && state.text_nonce.is_none() && state.playback.is_none()
    );
}
