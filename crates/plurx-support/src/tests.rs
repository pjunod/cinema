//! Regression deliverables. Executed by the final applicable lane, not per packet.
use crate::{handoff, operator, store::*, transport::*};
use plurx_core::diagnostics::{self, Manifest};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};
use webauthn_rs::prelude::*;

fn storage() -> (tempfile::TempDir, Database, String) {
    let root = tempfile::tempdir().expect("temp storage");
    atomic_secret(&root.path().join("key"), &[42; 32]).expect("key");
    let db = Database::open(&root.path().join("state"), &root.path().join("key")).expect("open");
    let publisher = root.path().join("publisher");
    db.enroll_publisher("fixture", &["https://cinema.example".into()], &publisher)
        .expect("publisher");
    let secret = std::fs::read_to_string(publisher).expect("credential");
    (root, db, secret)
}
fn archive() -> (GrantRequest, Vec<u8>) {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../plurx-core/tests/fixtures/diagnostics-v1.json"
    ))
    .expect("fixtures");
    let mut manifest = Manifest::decode(
        fixtures["cases"][0]["json"]
            .as_str()
            .expect("fixture JSON")
            .as_bytes(),
    )
    .expect("manifest");
    manifest.report_id = uuid::Uuid::new_v4().to_string();
    let mut files = BTreeMap::new();
    files.insert("description.txt".into(), b"Fixture report".to_vec());
    files.insert("client-events.jsonl".into(), Vec::new());
    let bytes = diagnostics::export(manifest.clone(), files).expect("archive");
    (
        GrantRequest {
            report_id: manifest.report_id,
            sha256: hash(&bytes),
            length: bytes.len() as u32,
            privacy: manifest.privacy,
        },
        bytes,
    )
}
fn staged(db: &Database, publisher: &str) -> (GrantRequest, String, String) {
    let (request, bytes) = archive();
    let grant = db
        .grant(Some(("fixture", publisher)), None, &request)
        .expect("grant");
    let upload = grant["upload_grant"].as_str().expect("upload").to_owned();
    let reserve = db.reserve(&upload, &request).expect("reserve");
    let management = reserve["management_grant"]
        .as_str()
        .expect("management")
        .to_owned();
    for (index, chunk) in bytes.chunks(CHUNK).enumerate() {
        db.chunk(
            &upload,
            &request.report_id,
            index as u32,
            &hash(chunk),
            chunk,
        )
        .expect("chunk");
    }
    db.complete(&upload, &request.report_id).expect("stage");
    (request, upload, management)
}
fn service(db: Database) -> Service {
    let origin = Url::parse("https://operators.example").expect("origin");
    let webauthn = WebauthnBuilder::new("operators.example", &origin)
        .expect("RP")
        .build()
        .expect("WebAuthn");
    Service {
        db,
        trust_loopback_ingress: false,
        support_origin: "https://support.example".into(),
        operator_origin: origin.origin().ascii_serialization(),
        webauthn: Arc::new(webauthn),
        challenges: operator::Challenges::default(),
        requests: Arc::new(tokio::sync::Semaphore::new(8)),
        rates: handoff::Rates::default(),
        inspections: Arc::new(tokio::sync::Semaphore::new(2)),
    }
}
fn headers(secret: &str) -> axum::http::HeaderMap {
    let mut h = axum::http::HeaderMap::new();
    h.insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {secret}").parse().expect("header"),
    );
    h
}
fn handoff_call(s: &Service, path: &str, method: &str, secret: &str, body: Value) -> Result<Value> {
    handoff::execute(
        s,
        path,
        &method.parse().expect("method"),
        &headers(secret),
        &serde_json::to_vec(&body).expect("body"),
    )
}

#[test]
fn resumed_upload_and_immutable_receipt_survive_restart_without_read_authority() {
    let (root, db, publisher) = storage();
    let (request, bytes) = archive();
    let cap = db
        .grant(Some(("fixture", &publisher)), None, &request)
        .expect("grant")["upload_grant"]
        .as_str()
        .expect("cap")
        .to_owned();
    let creator = db.reserve(&cap, &request).expect("reserve")["management_grant"]
        .as_str()
        .expect("creator")
        .to_owned();
    db.chunk(&cap, &request.report_id, 0, &hash(&bytes), &bytes)
        .expect("first chunk");
    drop(db);
    let db = Database::open(&root.path().join("state"), &root.path().join("key")).expect("restart");
    assert_eq!(
        db.status(&cap, &request.report_id).expect("status")["received"],
        json!([0])
    );
    db.chunk(&cap, &request.report_id, 0, &hash(&bytes), &bytes)
        .expect("same chunk");
    db.complete(&cap, &request.report_id).expect("complete");
    db.complete(&cap, &request.report_id)
        .expect("ambiguous completion");
    assert!(db
        .submit(&cap, &request.report_id, "Forbidden upload-only submit")
        .is_err());
    let first = db
        .submit(&creator, &request.report_id, "CANARY_PRIVATE_DESCRIPTION")
        .expect("submit");
    assert_eq!(
        first,
        db.submit(
            &creator,
            &request.report_id,
            "Must not replace the description"
        )
        .expect("retry")
    );
    assert!(db
        .grant(Some(("fixture", &publisher)), None, &request)
        .is_err());
    db.with(|c| {
        let blob: Vec<u8> = c.query_row(
            "SELECT description FROM reports WHERE id=?1",
            [&request.report_id],
            |r| r.get(0),
        )?;
        assert!(!blob.windows(26).any(|v| v == b"CANARY_PRIVATE_DESCRIPTION"));
        let plain = db.unseal(&format!("{}/fixture/description", request.report_id), &blob)?;
        assert_eq!(plain, b"CANARY_PRIVATE_DESCRIPTION");
        Ok(())
    })
    .expect("encrypted description");
}

#[test]
fn two_phone_claim_requires_tv_approval_and_revocation_cannot_resurrect_on_restore() {
    let (root, db, publisher) = storage();
    let (request, _, creator) = staged(&db, &publisher);
    let s = service(db.clone());
    let offer = handoff_call(
        &s,
        "handoffs",
        "POST",
        &creator,
        json!({"report_id":request.report_id}),
    )
    .expect("offer");
    let phone = random().expect("phone");
    let attacker = random().expect("attacker");
    let claim = handoff_call(
        &s,
        "handoffs/claim",
        "POST",
        "",
        json!({"code":offer["manual_code"],"proof":phone}),
    )
    .expect("claim");
    assert!(claim.get("report_id").is_none());
    assert!(claim.get("description").is_none());
    assert!(handoff_call(
        &s,
        "handoffs/claim",
        "POST",
        "",
        json!({"code":offer["manual_code"],"proof":attacker})
    )
    .is_err());
    let id = offer["id"].as_str().expect("ID");
    let pending = handoff_call(
        &s,
        &format!("handoffs/{id}/phone-status"),
        "GET",
        &phone,
        json!({}),
    )
    .expect("pending");
    assert!(pending.get("management_grant").is_none());
    handoff_call(
        &s,
        &format!("handoffs/{id}/approve"),
        "POST",
        &creator,
        json!({"comparison":claim["comparison"]}),
    )
    .expect("TV approval");
    let approved = handoff_call(
        &s,
        &format!("handoffs/{id}/phone-status"),
        "GET",
        &phone,
        json!({}),
    )
    .expect("approved");
    let token = approved["management_grant"]
        .as_str()
        .expect("phone authority");
    handoff_call(
        &s,
        &format!("reports/{}/preview", request.report_id),
        "GET",
        token,
        json!({}),
    )
    .expect("approved preview");
    // Replaying an old database must still replay the external deletion fence.
    db.with(|c| {
        c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    })
    .expect("checkpoint");
    let backup = root.path().join("snapshot.db");
    std::fs::copy(root.path().join("state/support.db"), &backup).expect("snapshot");
    db.erase(&creator, &request.report_id).expect("delete");
    drop(s);
    drop(db);
    std::fs::copy(backup, root.path().join("state/support.db")).expect("restore old DB");
    let db = Database::open(&root.path().join("state"), &root.path().join("key"))
        .expect("reconcile deletion journal");
    assert!(db.status(token, &request.report_id).is_err());
}

#[tokio::test]
async fn real_http_upload_and_separate_operator_listener_refuse_unapproved_and_foreign_origins() {
    let (_root, db, publisher) = storage();
    let s = service(db);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let app = public_router(&s);
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("server")
    });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");
    let base = format!("http://{address}/support/v1");
    let (request, bytes) = archive();
    let response = client
        .post(format!("{base}/grants"))
        .header("X-Publisher-Id", "fixture")
        .bearer_auth(&publisher)
        .json(&request)
        .send()
        .await
        .expect("HTTP grant");
    assert!(response.status().is_success());
    let value: Value = response.json().await.expect("JSON");
    let upload = value["upload_grant"].as_str().expect("upload");
    let reserve: Value = client
        .post(format!("{base}/uploads"))
        .bearer_auth(upload)
        .json(&request)
        .send()
        .await
        .expect("reserve")
        .json()
        .await
        .expect("JSON");
    let creator = reserve["management_grant"].as_str().expect("creator");
    assert_eq!(
        client
            .put(format!("{base}/uploads/{}/chunks/0", request.report_id))
            .bearer_auth(upload)
            .header("X-Chunk-Sha256", hash(&bytes))
            .body(bytes)
            .send()
            .await
            .expect("chunk")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        client
            .post(format!("{base}/uploads/{}/complete", request.report_id))
            .bearer_auth(upload)
            .send()
            .await
            .expect("complete")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        client
            .get(format!("{base}/reports/{}/preview", request.report_id))
            .bearer_auth(creator)
            .send()
            .await
            .expect("preview refusal")
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("{base}/uploads/{}/status", request.report_id))
            .bearer_auth(upload)
            .header("Origin", "https://foreign.example")
            .send()
            .await
            .expect("foreign refusal")
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("http://{address}/operator/v1/reports"))
            .send()
            .await
            .expect("isolated path")
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    task.abort();
    let _ = task.await;
}
