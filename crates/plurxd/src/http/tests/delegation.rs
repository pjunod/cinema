use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};
#[tokio::test]
async fn delegation_http_requires_exact_callback_secret_pkce_and_live_source() {
    let (app, state) = test_app_with_state();
    let owner = setup_admin(&app).await;
    let (status, _) = call(
        &app,
        post(
            "/api/v1/delegation/clients",
            Some(&owner),
            json!({"name":"Curator","redirect":"http://curator.example/callback"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let redirect = "https://curator.example/api/v1/auth/cinema/callback";
    let (status, registered) = call(
        &app,
        post(
            "/api/v1/delegation/clients",
            Some(&owner),
            json!({"name":"Curator","redirect":redirect}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    let client = registered["client"]["id"].as_str().expect("id");
    let secret = registered["clientSecret"].as_str().expect("secret");
    let verifier = "a".repeat(64);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let (status, consent) = call(
        &app,
        post(
            "/api/v1/delegation/consent",
            Some(&owner),
            json!({"clientId":client,"challenge":challenge,"state":"s".repeat(64)}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{consent}");
    let callback =
        reqwest::Url::parse(consent["redirect"].as_str().expect("callback")).expect("URL");
    assert_eq!(
        callback.origin().ascii_serialization(),
        "https://curator.example"
    );
    let code = callback
        .query_pairs()
        .find(|(key, _)| key == "code")
        .expect("code")
        .1
        .into_owned();
    let body = |proof: &str, secret: &str, redirect: &str| json!({"clientId":client,"clientSecret":secret,"code":code,"verifier":proof,"redirect":redirect});
    let (status, _) = call(
        &app,
        post(
            "/api/v1/delegation/exchange",
            None,
            body(&verifier, "wrong", redirect),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &app,
        post(
            "/api/v1/delegation/exchange",
            None,
            body(&"b".repeat(64), secret, redirect),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &app,
        post(
            "/api/v1/delegation/exchange",
            None,
            body(&verifier, secret, "https://other.example/callback"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, issued) = call(
        &app,
        post(
            "/api/v1/delegation/exchange",
            None,
            body(&verifier, secret, redirect),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{issued}");
    assert_eq!(issued["scope"], "household-requests");
    assert!(issued.get("is_admin").is_none());
    assert!(issued.get("password_hash").is_none());
    let grant = issued["grant"].as_str().expect("grant");
    assert_ne!(grant, owner);
    let (status, _) = call(&app, get("/api/v1/me", Some(grant))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &app,
        post(
            "/api/v1/delegation/exchange",
            None,
            body(&verifier, secret, redirect),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "code replay");
    state
        .store
        .delete_token(&plurx_core::auth::hash_token(&owner))
        .await
        .expect("source logout");
    let (status, inactive) = call(
        &app,
        post(
            "/api/v1/delegation/introspect",
            None,
            json!({"clientId":client,"clientSecret":secret,"grant":grant}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(inactive["active"], false);
}
