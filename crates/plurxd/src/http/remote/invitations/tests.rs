use super::*;
use axum::{body::to_bytes, http::Request};
use tower::ServiceExt;

async fn call(
    app: &axum::Router,
    token: &str,
    path: &str,
    body: &str,
    phone: Option<&str>,
) -> (u16, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("/api/remote/v1/{path}"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if let Some(phone) = phone {
        request = request.header("x-cinema-phone-secret", phone);
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(Body::from(body.to_owned()))
                .expect("invitation fixture"),
        )
        .await
        .expect("invitation fixture");
    let status = response.status().as_u16();
    let value: Value = serde_json::from_slice(
        &to_bytes(response.into_body(), 65536)
            .await
            .expect("invitation fixture"),
    )
    .expect("invitation fixture");
    assert_eq!(value["version"], "cinema.invitation.v1");
    (status, value)
}
async fn login(state: &AppState) -> String {
    let user = state
        .store
        .create_user("invitation-test", "hash", false)
        .await
        .expect("invitation fixture");
    let token = auth::generate_token().expect("invitation fixture");
    state
        .store
        .create_token(&auth::hash_token(&token), user.id, None)
        .await
        .expect("invitation fixture");
    token
}
#[tokio::test]
async fn phone_router_rejects_ambiguous_json_and_generation_lexemes() {
    let (app, state) = crate::http::tests::test_app_with_state();
    let token = login(&state).await;
    let id = Uuid::new_v4().to_string();
    let register = json!({"version":"cinema.invitation.v1","installation_id":id,"platform":"android","name":"Phone"}).to_string();
    let (status, registered) = call(&app, &token, "phones", &register, None).await;
    assert_eq!(status, 200);
    let proof = registered["phone_secret"]
        .as_str()
        .expect("invitation fixture");
    for route in ["availability", "rebind"] {
        for generation in [
            "-0",
            "0",
            "-1",
            "1e0",
            "1.0",
            "9007199254740992",
            "18446744073709551616",
        ] {
            let flags = if route == "availability" {
                ",\"permission_granted\":false,\"resident_active\":false"
            } else {
                ""
            };
            let body = format!("{{\"version\":\"cinema.invitation.v1\",\"expected_phone_generation\":{generation}{flags}}}");
            assert_eq!(
                call(
                    &app,
                    &token,
                    &format!("phones/{id}/{route}"),
                    &body,
                    Some(proof)
                )
                .await
                .0,
                400,
                "{route}: {generation}"
            );
        }
    }
    for body in [
        r#"{"version":"cinema.invitation.v1","expected_phone_generation":1,"expected_phone_generation":1}"#,
        r#"{"version":"cinema.invitation.v1","expected_phone_generation":1,"extra":true}"#,
    ] {
        assert_eq!(
            call(
                &app,
                &token,
                &format!("phones/{id}/rebind"),
                body,
                Some(proof)
            )
            .await
            .0,
            400
        );
    }
    assert_eq!(call(&app, &token, "phones", &register, None).await.0, 401);
    let (status, retry) = call(&app, &token, "phones", &register, Some(proof)).await;
    assert_eq!(status, 200);
    assert!(retry["phone_secret"].is_null());
    assert_eq!(
        call(&app, &token, "phones", &" ".repeat(65537), None)
            .await
            .0,
        413
    );
}

#[tokio::test]
async fn phone_availability_router_does_not_refresh_native_activity() {
    use crate::state::Dirs;
    use plurx_core::store::SqliteStore;
    let tmp = tempfile::tempdir().expect("invitation fixture");
    let db = tmp.path().join("fixture.db");
    let state = AppState::new(
        "test".into(),
        Arc::new(SqliteStore::open(&db).expect("invitation fixture")),
        Dirs {
            artwork: tmp.path().join("art"),
            transcode: tmp.path().join("transcode"),
            cache: tmp.path().join("cache"),
            subs: tmp.path().join("subs"),
            runtime_cache: tmp.path().join("runtime"),
            renditions: tmp.path().join("renditions"),
        },
        "test-node".into(),
        Default::default(),
        Default::default(),
        Arc::new(crate::logbuf::LogBuffer::new(64)),
    );
    let app = crate::http::router(state.clone());
    let token = login(&state).await;
    let id = Uuid::new_v4().to_string();
    let (raw, hash) = secret().expect("invitation fixture");
    let user = state
        .store
        .invitation_login(
            &auth::hash_token(&token),
            now_seconds().expect("invitation fixture"),
        )
        .await
        .expect("invitation fixture")
        .expect("invitation fixture")
        .user_id;
    assert!(state
        .store
        .create_invitation_phone(NewInvitationPhone {
            phone: InvitationPhone {
                id: id.clone(),
                user_id: user,
                name: "Phone".into(),
                platform: "android".into(),
                generation: 1,
                created_at: now_seconds().expect("invitation fixture"),
                permission_granted: false,
                resident_active: false
            },
            secret_hash: hash,
            token_digest: auth::hash_token(&token),
        })
        .await
        .expect("invitation fixture"));
    let proof = raw.as_str();
    let connection = rusqlite::Connection::open(&db).expect("invitation fixture");
    let old = now_seconds().expect("invitation fixture") - 600;
    connection
        .execute(
            "UPDATE tokens SET last_seen_at=?1 WHERE token_hash=?2",
            rusqlite::params![old, auth::hash_token(&token)],
        )
        .expect("invitation fixture");
    let (status,_) = call(&app,&token,&format!("phones/{id}/availability"),r#"{"version":"cinema.invitation.v1","expected_phone_generation":1,"permission_granted":true,"resident_active":true}"#,Some(proof)).await;
    assert_eq!(status, 200);
    let seen: i64 = connection
        .query_row(
            "SELECT last_seen_at FROM tokens WHERE token_hash=?1",
            [auth::hash_token(&token)],
            |r| r.get(0),
        )
        .expect("invitation fixture");
    assert_eq!(
        seen, old,
        "production middleware and background route must both avoid touching activity"
    );
}
