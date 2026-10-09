use super::*;
use axum::{body::to_bytes, http::Request};
use tower::ServiceExt;

pub(super) async fn call(
    app: &axum::Router,
    token: &str,
    path: &str,
    body: &str,
    phone: Option<&str>,
) -> (u16, Value) {
    call_proofs(app, token, path, body, phone, None).await
}
pub(super) async fn call_proofs(
    app: &axum::Router,
    token: &str,
    path: &str,
    body: &str,
    phone: Option<&str>,
    grant: Option<&str>,
) -> (u16, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("/api/remote/v1/{path}"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if let Some(phone) = phone {
        request = request.header("x-cinema-phone-secret", phone);
    }
    if let Some(grant) = grant {
        request = request.header("x-cinema-grant-secret", grant);
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
pub(super) async fn login(state: &AppState) -> String {
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
    let register=json!({"version":"cinema.invitation.v1","installation_id":id,"platform":"android","name":"Phone"}).to_string();
    assert_eq!(
        call(&app, &token, "phones", &register, Some(proof)).await.0,
        200
    );
    assert_eq!(
        call(
            &app,
            &token,
            "phones/list",
            r#"{"version":"cinema.invitation.v1","after_id":null,"limit":20}"#,
            Some(proof)
        )
        .await
        .0,
        200
    );
    assert_eq!(call(&app,&token,"invitations/consents/list",&json!({"version":"cinema.invitation.v1","installation_id":id,"after_receiver_id":null,"limit":20}).to_string(),Some(proof)).await.0,200);
    let receiver = Uuid::new_v4().to_string();
    let grant = Uuid::new_v4().to_string();
    let start=json!({"version":"cinema.invitation.v1","installation_id":id,"receiver_id":receiver,"grant_id":grant,"expected_phone_generation":2,"expected_consent_generation":1}).to_string();
    assert_eq!(
        call(
            &app,
            &token,
            "invitations/transport/start",
            &start,
            Some(proof)
        )
        .await
        .0,
        409
    );
    let confirm=json!({"version":"cinema.invitation.v1","installation_id":id,"receiver_id":receiver,"ticket_id":Uuid::new_v4().to_string(),"expected_phone_generation":2,"expected_consent_generation":1,"expected_transport_generation":1}).to_string();
    assert_eq!(
        call(
            &app,
            &token,
            "invitations/transport/confirm",
            &confirm,
            Some(proof)
        )
        .await
        .0,
        409
    );
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

#[tokio::test]
async fn consent_router_preserves_enabled_choice_and_allows_scoped_off_after_grant_revoke() {
    use plurx_core::store::remote::{
        NewRemoteGrant, NewRemoteReceiver, RemoteGrant, RemoteReceiver,
    };
    let (app, state) = crate::http::tests::test_app_with_state();
    let token = login(&state).await;
    let user = state
        .store
        .invitation_login(&auth::hash_token(&token), now_seconds().expect("clock"))
        .await
        .expect("login")
        .expect("live")
        .user_id;
    let phone_id = Uuid::new_v4().to_string();
    let (_,registered)=call(&app,&token,"phones",&json!({"version":"cinema.invitation.v1","installation_id":phone_id,"platform":"android","name":"Phone"}).to_string(),None).await;
    let phone = registered["phone_secret"].as_str().expect("proof");
    let receiver = Uuid::new_v4().to_string();
    let grant = Uuid::new_v4().to_string();
    let (raw, hash) = secret().expect("proof");
    assert!(state
        .store
        .create_remote_receiver(NewRemoteReceiver {
            receiver: RemoteReceiver {
                id: receiver.clone(),
                user_id: user,
                name: "TV".into(),
                platform: "web".into(),
                created_at: now_seconds().expect("clock")
            },
            secret_hash: auth::hash_token("synthetic receiver")
        })
        .await
        .expect("receiver"));
    assert!(state
        .store
        .create_remote_grant(NewRemoteGrant {
            grant: RemoteGrant {
                id: grant.clone(),
                receiver_id: receiver.clone(),
                name: "Phone".into(),
                created_at: now_seconds().expect("clock")
            },
            user_id: user,
            secret_hash: hash
        })
        .await
        .expect("grant"));
    let request=json!({"version":"cinema.invitation.v1","installation_id":phone_id,"receiver_id":receiver,"expected_phone_generation":1,"expected_consent_generation":0,"enabled":true,"grant_id":grant,"transport":"fcm"}).to_string();
    assert_eq!(
        call(&app, &token, "invitations/consent", &request, Some(phone))
            .await
            .0,
        401
    );
    let (status, enabled) = call_proofs(
        &app,
        &token,
        "invitations/consent",
        &request,
        Some(phone),
        Some(&raw),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(enabled["consent"]["enabled"], true);
    assert_eq!(enabled["consent"]["readiness"]["status"], "global_disabled");
    state
        .store
        .revoke_remote_grant(&grant, user, now_seconds().expect("clock"))
        .await
        .expect("revoke");
    let request=json!({"version":"cinema.invitation.v1","installation_id":phone_id,"receiver_id":receiver,"expected_phone_generation":1,"expected_consent_generation":1,"enabled":false}).to_string();
    let (status, disabled) = call(&app, &token, "invitations/consent", &request, Some(phone)).await;
    assert_eq!(status, 200);
    assert_eq!(disabled["consent"]["enabled"], false);
    assert_eq!(disabled["consent"]["consent_generation"], 2);
    assert_eq!(
        call(&app, &token, "invitations/consent", &request, Some(phone))
            .await
            .0,
        409
    );
    for generation in ["-0", "1e0", "-1", "9007199254740991"] {
        let body = request.replace(
            "\"expected_consent_generation\":1",
            &format!("\"expected_consent_generation\":{generation}"),
        );
        assert_eq!(
            call(&app, &token, "invitations/consent", &body, Some(phone))
                .await
                .0,
            400,
            "{generation}"
        );
    }
}
#[tokio::test]
async fn consent_router_pages_accumulated_revoked_receivers_without_truncation() {
    use plurx_core::store::remote::{
        NewRemoteGrant, NewRemoteReceiver, RemoteGrant, RemoteReceiver,
    };
    let (app, state) = crate::http::tests::test_app_with_state();
    let token = login(&state).await;
    let user = state
        .store
        .invitation_login(&auth::hash_token(&token), now_seconds().expect("clock"))
        .await
        .expect("login")
        .expect("live")
        .user_id;
    let phone_id = Uuid::new_v4().to_string();
    let (_,registered)=call(&app,&token,"phones",&json!({"version":"cinema.invitation.v1","installation_id":phone_id,"platform":"android","name":"Phone"}).to_string(),None).await;
    let phone = registered["phone_secret"].as_str().expect("proof");
    for _ in 0..21 {
        let receiver = Uuid::new_v4().to_string();
        let grant = Uuid::new_v4().to_string();
        let hash = auth::hash_token("synthetic grant");
        assert!(state
            .store
            .create_remote_receiver(NewRemoteReceiver {
                receiver: RemoteReceiver {
                    id: receiver.clone(),
                    user_id: user,
                    name: "TV".into(),
                    platform: "web".into(),
                    created_at: now_seconds().expect("clock")
                },
                secret_hash: auth::hash_token("synthetic receiver")
            })
            .await
            .expect("receiver"));
        assert!(state
            .store
            .create_remote_grant(NewRemoteGrant {
                grant: RemoteGrant {
                    id: grant.clone(),
                    receiver_id: receiver.clone(),
                    name: "Phone".into(),
                    created_at: now_seconds().expect("clock")
                },
                user_id: user,
                secret_hash: hash.clone()
            })
            .await
            .expect("grant"));
        assert!(state
            .store
            .save_invitation_consent(SaveInvitationConsent {
                id: Uuid::new_v4().to_string(),
                phone_id: phone_id.clone(),
                receiver_id: receiver.clone(),
                user_id: user,
                phone_hash: auth::hash_token(phone),
                expected_generation: 0,
                expected_phone_generation: 1,
                enable: Some((grant, hash, InvitationTransport::Fcm))
            })
            .await
            .expect("consent"));
        state
            .store
            .revoke_remote_receiver(&receiver, user, now_seconds().expect("clock"))
            .await
            .expect("revoke receiver");
    }
    let (status,page)=call(&app,&token,"invitations/consents/list",&json!({"version":"cinema.invitation.v1","installation_id":phone_id,"after_receiver_id":null,"limit":20}).to_string(),Some(phone)).await;
    assert_eq!(status, 200);
    assert_eq!(page["consents"].as_array().expect("rows").len(), 20);
    let cursor = page["next_cursor"].as_str().expect("continuation");
    let (status,last)=call(&app,&token,"invitations/consents/list",&json!({"version":"cinema.invitation.v1","installation_id":phone_id,"after_receiver_id":cursor,"limit":20}).to_string(),Some(phone)).await;
    assert_eq!(status, 200);
    assert_eq!(last["consents"].as_array().expect("rows").len(), 1);
    assert!(last["next_cursor"].is_null());
    assert!(page["consents"]
        .as_array()
        .expect("rows")
        .iter()
        .all(|row| row["readiness"]["status"] == "grant_revoked"));
}
