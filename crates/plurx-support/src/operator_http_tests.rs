//! Operator inbox behavior with real encrypted report storage. Session fixtures
//! begin at the authenticated-session boundary; they do not qualify a passkey.
use super::*;
use crate::store::{GrantRequest, CHUNK};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use plurx_core::diagnostics::{self, Manifest};
use std::collections::BTreeMap;
use tower::ServiceExt;

struct Fixture {
    root: tempfile::TempDir,
    service: Service,
    publisher_secret: String,
    operator: String,
    headers: HeaderMap,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("support operator fixture");
        let key = root.path().join("key");
        crate::store::atomic_secret(&key, &[39; 32]).expect("encryption key");
        let db = Database::open(&root.path().join("state"), &key).expect("real encrypted storage");
        let publisher = root.path().join("publisher");
        db.enroll_publisher("fixture", &["https://cinema.example".into()], &publisher)
            .expect("publisher");
        let publisher_secret = std::fs::read_to_string(publisher).expect("publisher credential");
        let origin = Url::parse("https://operators.example").expect("origin");
        let webauthn = WebauthnBuilder::new("operators.example", &origin)
            .expect("RP")
            .build()
            .expect("WebAuthn");
        let service = Service {
            db,
            support_origin: "https://support.example".into(),
            operator_origin: origin.origin().ascii_serialization(),
            webauthn: Arc::new(webauthn),
            challenges: Challenges::default(),
            requests: Arc::new(tokio::sync::Semaphore::new(8)),
            rates: crate::handoff::Rates::default(),
            inspections: Arc::new(tokio::sync::Semaphore::new(2)),
            trust_loopback_ingress: false,
        };
        let operator = uuid::Uuid::new_v4().to_string();
        // Trusted test setup of the already-authenticated boundary. Every HTTP
        // handler must still prove origin, session digest, CSRF, expiry/activity.
        service
            .db
            .with(|c| {
                c.execute(
                    "INSERT INTO operators VALUES(?1,'operator',1,NULL)",
                    [&operator],
                )?;
                c.execute(
                    "INSERT INTO operator_tokens VALUES(?1,?2,?3,?4)",
                    params![
                        hash(b"session-secret"),
                        operator,
                        format!("session-{}", hash(b"csrf-secret")),
                        now() + 900
                    ],
                )?;
                Ok(())
            })
            .expect("session fixture");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            "https://operators.example".parse().expect("origin"),
        );
        headers.insert(
            header::COOKIE,
            "unrelated=one; __Host-cinema_support=session-secret; other=two"
                .parse()
                .expect("cookie"),
        );
        headers.insert("x-csrf", "csrf-secret".parse().expect("CSRF"));
        Self {
            root,
            service,
            publisher_secret,
            operator,
            headers,
        }
    }
    fn submitted(&self) -> String {
        let cases: Value = serde_json::from_str(include_str!(
            "../../plurx-core/tests/fixtures/diagnostics-v1.json"
        ))
        .expect("diagnostic fixtures");
        let mut manifest = Manifest::decode(
            cases["cases"][0]["json"]
                .as_str()
                .expect("manifest JSON")
                .as_bytes(),
        )
        .expect("valid manifest");
        manifest.report_id = uuid::Uuid::new_v4().to_string();
        let id = manifest.report_id.clone();
        let mut files = BTreeMap::new();
        files.insert("description.txt".into(), b"original".to_vec());
        files.insert("client-events.jsonl".into(), Vec::new());
        let archive =
            diagnostics::export(manifest.clone(), files).expect("actual diagnostic archive");
        let request = GrantRequest {
            report_id: id.clone(),
            sha256: hash(&archive),
            length: archive.len() as u32,
            privacy: manifest.privacy,
        };
        let grant = self
            .service
            .db
            .grant(Some(("fixture", &self.publisher_secret)), None, &request)
            .expect("grant");
        let upload = grant["upload_grant"].as_str().expect("upload");
        let reserve = self.service.db.reserve(upload, &request).expect("reserve");
        let management = reserve["management_grant"].as_str().expect("management");
        for (index, chunk) in archive.chunks(CHUNK).enumerate() {
            self.service
                .db
                .chunk(upload, &id, index as u32, &hash(chunk), chunk)
                .expect("encrypted chunk");
        }
        self.service
            .db
            .complete(upload, &id)
            .expect("verified archive staging");
        self.service
            .db
            .submit(management, &id, "Operator lifecycle description")
            .expect("submission");
        id
    }
    fn count(&self, table: &str) -> i64 {
        assert!(["operators", "operator_tokens", "audit", "invitations"].contains(&table));
        self.service
            .db
            .with(|c| Ok(c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?))
            .expect("count")
    }
}
async fn value(response: Response, status: StatusCode) -> Value {
    assert_eq!(response.status(), status);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("response JSON")
}
async fn inbox(f: &Fixture, filter: Filter) -> Value {
    value(
        list(State(f.service.clone()), Query(filter), f.headers.clone()).await,
        StatusCode::OK,
    )
    .await
}

#[tokio::test]
async fn authenticated_inbox_reads_encrypted_reports_updates_metadata_and_audits_exact_actions() {
    let f = Fixture::new();
    let id = f.submitted();
    let all = inbox(&f, Filter::default()).await;
    assert_eq!(all["reports"].as_array().expect("reports").len(), 1);
    assert_eq!(all["reports"][0]["id"], id);
    let detail = value(
        detail(
            State(f.service.clone()),
            RoutePath(id.clone()),
            f.headers.clone(),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(detail["description"], "Operator lifecycle description");
    assert!(detail["summary"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(detail["timeline"].is_array());
    let downloaded = download(
        State(f.service.clone()),
        RoutePath(id.clone()),
        f.headers.clone(),
    )
    .await;
    assert_eq!(downloaded.status(), StatusCode::OK);
    assert_eq!(
        downloaded.headers()[header::CONTENT_TYPE],
        "application/zip"
    );
    assert!(downloaded.headers()[header::CONTENT_DISPOSITION]
        .to_str()
        .expect("disposition")
        .contains("attachment"));
    let bytes = downloaded
        .into_body()
        .collect()
        .await
        .expect("download")
        .to_bytes();
    let archive = diagnostics::inspect(&bytes).expect("download is a valid actual report");
    assert_eq!(archive.manifest.report_id, id);
    let updated = value(
        label(
            State(f.service.clone()),
            RoutePath(id.clone()),
            f.headers.clone(),
            Bytes::from_static(br#"{"label":"investigating"}"#),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(updated["label"], "investigating");
    value(
        retention(
            State(f.service.clone()),
            RoutePath(id.clone()),
            f.headers.clone(),
            Bytes::from_static(br#"{"days":7}"#),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    f.service
        .db
        .with(|c| {
            let (label, created, expires): (String, i64, i64) = c.query_row(
                "SELECT label,created,expires FROM reports WHERE id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            assert_eq!(label, "investigating");
            assert_eq!(expires - created, 7 * 86400);
            Ok(())
        })
        .expect("durable updates");
    let actions = f
        .service
        .db
        .with(|c| {
            Ok(
                c.prepare("SELECT action FROM audit WHERE operator=?1 ORDER BY id")?
                    .query_map([&f.operator], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?,
            )
        })
        .expect("audit trail");
    assert_eq!(
        actions,
        vec!["list", "view", "download", "label", "retention"]
    );
}

#[tokio::test]
async fn operator_deletion_erases_sealed_evidence_revokes_caps_and_is_not_a_read_capability() {
    type StoredReportEvidence = (
        String,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        i64,
    );
    let f = Fixture::new();
    let id = f.submitted();
    value(
        delete(
            State(f.service.clone()),
            RoutePath(id.clone()),
            f.headers.clone(),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    f.service
        .db
        .with(|c| {
            let (state, blob, metadata, description, length): StoredReportEvidence = c.query_row(
                "SELECT state,blob,metadata,description,length FROM reports WHERE id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
            assert_eq!(state, "deleted");
            assert!(blob.is_none() && metadata.is_none() && description.is_none());
            assert_eq!(length, 0);
            let active: i64 = c.query_row(
                "SELECT COUNT(*) FROM caps WHERE report=?1 AND active=1",
                [&id],
                |r| r.get(0),
            )?;
            assert_eq!(active, 0);
            Ok(())
        })
        .expect("durable erasure");
    value(
        detail(
            State(f.service.clone()),
            RoutePath(id.clone()),
            f.headers.clone(),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    value(
        download(State(f.service.clone()), RoutePath(id), f.headers.clone()).await,
        StatusCode::NOT_FOUND,
    )
    .await;
    assert!(inbox(&f, Filter::default()).await["reports"]
        .as_array()
        .expect("reports")
        .is_empty());
}

#[tokio::test]
async fn origin_cookie_csrf_expiry_and_operator_activity_each_fence_report_authority() {
    let f = Fixture::new();
    let id = f.submitted();
    for headers in [
        HeaderMap::new(),
        {
            let mut h = f.headers.clone();
            h.remove("x-csrf");
            h
        },
        {
            let mut h = f.headers.clone();
            h.insert("x-csrf", "wrong".parse().expect("header"));
            h
        },
        {
            let mut h = f.headers.clone();
            h.insert(
                header::ORIGIN,
                "https://foreign.example".parse().expect("header"),
            );
            h
        },
        {
            let mut h = f.headers.clone();
            h.insert(
                header::COOKIE,
                "__Host-cinema_support=unknown".parse().expect("header"),
            );
            h
        },
    ] {
        value(
            detail(State(f.service.clone()), RoutePath(id.clone()), headers).await,
            StatusCode::UNAUTHORIZED,
        )
        .await;
    }
    let mut no_origin = f.headers.clone();
    no_origin.remove(header::ORIGIN);
    value(
        detail(
            State(f.service.clone()),
            RoutePath(id.clone()),
            no_origin.clone(),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    value(
        label(
            State(f.service.clone()),
            RoutePath(id.clone()),
            no_origin,
            Bytes::from_static(br#"{"label":"resolved"}"#),
        )
        .await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    f.service
        .db
        .with(|c| {
            c.execute("UPDATE operator_tokens SET expires=?1", [now() - 1])?;
            Ok(())
        })
        .expect("expire");
    value(
        detail(
            State(f.service.clone()),
            RoutePath(id.clone()),
            f.headers.clone(),
        )
        .await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    f.service
        .db
        .with(|c| {
            c.execute("UPDATE operator_tokens SET expires=?1", [now() + 900])?;
            c.execute("UPDATE operators SET active=0", [])?;
            Ok(())
        })
        .expect("revoke");
    value(
        detail(State(f.service.clone()), RoutePath(id), f.headers.clone()).await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
}

#[tokio::test]
async fn filters_and_mutation_validation_preserve_reports_and_forbid_silent_extra_fields() {
    let f = Fixture::new();
    let id = f.submitted();
    for filter in [
        Filter {
            platform: "wrong-platform".into(),
            ..Default::default()
        },
        Filter {
            build: "wrong-build".into(),
            ..Default::default()
        },
        Filter {
            category: "wrong-category".into(),
            ..Default::default()
        },
        Filter {
            label: "resolved".into(),
            ..Default::default()
        },
        Filter {
            reference: "unknown".into(),
            ..Default::default()
        },
        Filter {
            offset: 1,
            ..Default::default()
        },
        Filter {
            until: 1,
            ..Default::default()
        },
    ] {
        assert!(inbox(&f, filter).await["reports"]
            .as_array()
            .expect("filtered reports")
            .is_empty());
    }
    let matching = inbox(
        &f,
        Filter {
            platform: "tvos".into(),
            build: "231".into(),
            category: "playback".into(),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        matching["reports"]
            .as_array()
            .expect("matching reports")
            .len(),
        1
    );
    for filter in [
        Filter {
            offset: 50001,
            ..Default::default()
        },
        Filter {
            since: u64::MAX,
            ..Default::default()
        },
        Filter {
            reference: "x".repeat(65),
            ..Default::default()
        },
    ] {
        value(
            list(State(f.service.clone()), Query(filter), f.headers.clone()).await,
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    for body in [
        br#"{"label":"untrusted"}"#.as_slice(),
        br#"{"label":"resolved","other":1}"#.as_slice(),
        b"not JSON".as_slice(),
    ] {
        value(
            label(
                State(f.service.clone()),
                RoutePath(id.clone()),
                f.headers.clone(),
                Bytes::copy_from_slice(body),
            )
            .await,
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    for body in [
        br#"{"days":0}"#.as_slice(),
        br#"{"days":91}"#.as_slice(),
        br#"{"days":"7"}"#.as_slice(),
    ] {
        value(
            retention(
                State(f.service.clone()),
                RoutePath(id.clone()),
                f.headers.clone(),
                Bytes::copy_from_slice(body),
            )
            .await,
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    assert_eq!(
        inbox(&f, Filter::default()).await["reports"][0]["label"],
        "new"
    );
}

#[test]
fn enrollment_bootstrap_validates_names_persists_only_digests_and_refuses_overwrite_or_quota() {
    let f = Fixture::new();
    for name in ["", "unsafe/name", "space name", &"x".repeat(65)] {
        assert!(matches!(
            bootstrap(&f.service.db, name, &f.root.path().join("invalid")),
            Err(Fault::Invalid)
        ));
    }
    let out = f.root.path().join("enrollment");
    bootstrap(&f.service.db, "new-operator", &out).expect("bootstrap");
    let token = std::fs::read_to_string(&out).expect("private enrollment token");
    assert_eq!(token.len(), 64);
    assert!(enrollment(&f.service.db, &token).is_ok());
    assert!(enrollment(&f.service.db, "wrong").is_err());
    let count = f.count("operators");
    assert!(matches!(
        bootstrap(&f.service.db, "another", &out),
        Err(Fault::Conflict)
    ));
    assert_eq!(f.count("operators"), count);
    let stored = f
        .service
        .db
        .with(|c| {
            Ok(c.query_row(
                "SELECT digest FROM operator_tokens WHERE kind='enroll'",
                [],
                |r| r.get::<_, String>(0),
            )?)
        })
        .expect("digest");
    assert_eq!(stored, hash(token.as_bytes()));
    assert_ne!(stored, token);
    f.service
        .db
        .with(|c| {
            for index in count..32 {
                c.execute(
                    "INSERT INTO operators VALUES(?1,?2,1,NULL)",
                    params![uuid::Uuid::new_v4().to_string(), format!("quota-{index}")],
                )?;
            }
            Ok(())
        })
        .expect("full operator quota");
    let rejected = f.root.path().join("quota-token");
    assert!(matches!(
        bootstrap(&f.service.db, "beyond-quota", &rejected),
        Err(Fault::Quota)
    ));
    assert!(!rejected.exists());
}

#[tokio::test]
async fn genuine_webauthn_enrollment_start_is_one_use_bound_and_refuses_invalid_finish() {
    let f = Fixture::new();
    let token_path = f.root.path().join("enroll");
    bootstrap(&f.service.db, "enroller", &token_path).expect("bootstrap");
    let token = std::fs::read_to_string(token_path).expect("token");
    let started = value(
        auth(
            State(f.service.clone()),
            RoutePath("enroll-start".into()),
            f.headers.clone(),
            Bytes::from(serde_json::to_vec(&json!({"token":token})).expect("JSON")),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert!(started["challenge"].is_object());
    let id = started["id"].as_str().expect("challenge id");
    let finish = json!({"token":token,"id":id,"credential":{}});
    value(
        auth(
            State(f.service.clone()),
            RoutePath("enroll-finish".into()),
            f.headers.clone(),
            Bytes::from(serde_json::to_vec(&finish).expect("JSON")),
        )
        .await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    value(
        auth(
            State(f.service.clone()),
            RoutePath("enroll-finish".into()),
            f.headers.clone(),
            Bytes::from(serde_json::to_vec(&finish).expect("JSON")),
        )
        .await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    assert!(
        enrollment(&f.service.db, &token).is_ok(),
        "invalid credentials never consume the enrollment token"
    );
    value(
        auth(
            State(f.service.clone()),
            RoutePath("login-start".into()),
            f.headers.clone(),
            Bytes::from_static(br#"{"name":"not-enrolled"}"#),
        )
        .await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    value(
        auth(
            State(f.service.clone()),
            RoutePath("unknown-action".into()),
            f.headers.clone(),
            Bytes::from_static(b"{}"),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    value(
        auth(
            State(f.service.clone()),
            RoutePath("enroll-start".into()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
}

#[test]
fn challenge_capacity_and_expiry_are_bounded_without_waiting_five_minutes() {
    let f = Fixture::new();
    let path = f.root.path().join("challenge-token");
    bootstrap(&f.service.db, "challenger", &path).expect("bootstrap");
    let token = std::fs::read_to_string(path).expect("token");
    for _ in 0..16 {
        authenticate(&f.service, "enroll-start", json!({"token":token}))
            .expect("genuine RP challenge");
    }
    assert!(matches!(
        authenticate(&f.service, "enroll-start", json!({"token":token})),
        Err(Fault::Quota)
    ));
    let first = f
        .service
        .challenges
        .0
        .lock()
        .expect("challenges")
        .keys()
        .next()
        .expect("pending")
        .clone();
    {
        let mut pending = f.service.challenges.0.lock().expect("challenges");
        pending.get_mut(&first).expect("challenge").at = Instant::now() - Duration::from_secs(301);
    }
    assert!(matches!(
        f.service.challenges.take(&first),
        Err(Fault::Expired)
    ));
    assert!(matches!(
        f.service.challenges.take(&first),
        Err(Fault::Unauthorized)
    ));
    authenticate(&f.service, "enroll-start", json!({"token":token}))
        .expect("expired slot reclaimed");
}

#[tokio::test]
async fn operator_invitations_are_audited_and_router_assets_are_private() {
    let f = Fixture::new();
    let invitation = value(
        invitation(
            State(f.service.clone()),
            f.headers.clone(),
            Bytes::from_static(br#"{"publisher":"fixture"}"#),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(invitation["authority"], "one_report_upload_only");
    assert_eq!(invitation["code"].as_str().expect("code").len(), 10);
    assert_eq!(f.count("invitations"), 1);
    value(
        super::invitation(
            State(f.service.clone()),
            f.headers.clone(),
            Bytes::from_static(br#"{"publisher":"missing"}"#),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    let router = crate::transport::operator_router(&f.service);
    for path in ["/", "/operator.js", "/support.css"] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("router");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(!response
            .into_body()
            .collect()
            .await
            .expect("asset")
            .to_bytes()
            .is_empty());
    }
    let forged = router
        .oneshot(
            Request::builder()
                .uri("/operator/v1/reports")
                .header(header::ORIGIN, "https://foreign.example")
                .body(Body::empty())
                .expect("forged origin"),
        )
        .await
        .expect("router refusal");
    assert_eq!(forged.status(), StatusCode::UNAUTHORIZED);
}
