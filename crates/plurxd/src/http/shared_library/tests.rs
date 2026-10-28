use super::*;
use http_body_util::BodyExt;
use plurx_core::{
    domain::{LibraryKind, NewLibrary},
    sharing::*,
};
use tower::ServiceExt;

static BODY_FIXTURES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
use bytes::Bytes;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
// Wraps the production Bytes without copying, so the writer's queue still
// holds whatever the response attached to them (artwork leases).
struct PayloadOwner {
    bytes: Bytes,
    dropped: Arc<AtomicBool>,
}
impl AsRef<[u8]> for PayloadOwner {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
impl Drop for PayloadOwner {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}
struct TrackedBody {
    inner: Body,
    body_dropped: Arc<AtomicBool>,
    data_dropped: Arc<AtomicBool>,
}
impl Drop for TrackedBody {
    fn drop(&mut self) {
        self.body_dropped.store(true, Ordering::SeqCst);
    }
}
impl http_body::Body for TrackedBody {
    type Data = Bytes;
    type Error = axum::Error;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        let flag = self.data_dropped.clone();
        std::pin::Pin::new(&mut self.inner)
            .poll_frame(cx)
            .map(|frame| {
                frame.map(|frame| {
                    frame.map(|frame| {
                        frame.map_data(|data| {
                            Bytes::from_owner(PayloadOwner {
                                bytes: data,
                                dropped: flag,
                            })
                        })
                    })
                })
            })
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}
struct BodyFixture {
    _directory: tempfile::TempDir,
    state: AppState,
    secret: plurx_core::secrets::Secret,
    grant: uuid::Uuid,
    library: i64,
    private: i64,
    item: i64,
    path: std::path::PathBuf,
}
async fn body_fixture() -> BodyFixture {
    use plurx_core::{
        domain::{ItemKind, MetadataPatch, NewItem},
        store::SqliteStore,
    };
    let directory = tempfile::tempdir().expect("source fixture directory");
    let path = directory.path().join("source.sqlite");
    let store = SqliteStore::open(&path).expect("source fixture store");
    let (_, mut state) = super::super::tests::test_app_with_state();
    state.store = Arc::new(store);
    state
        .store
        .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
        .await
        .expect("enable source fixture");
    state
        .store
        .sharing_identity(1000)
        .await
        .expect("source identity");
    let mut libraries = Vec::new();
    for name in ["Shared", "Private"] {
        libraries.push(
            state
                .store
                .create_library(&NewLibrary {
                    name: name.into(),
                    kind: LibraryKind::Movies,
                    paths: vec![directory.path().join(name)],
                    anime: false,
                })
                .await
                .expect("source library")
                .id,
        );
    }
    let connection = rusqlite::Connection::open(&path).expect("candidate fixture writer");
    connection
        .execute_batch(plurx_core::store::sharing_catalogue_source::CANDIDATE_ITEM_IDENTITY_SCHEMA)
        .expect("candidate item identity");
    connection
        .execute_batch(plurx_core::store::sharing_catalogue_source::CANDIDATE_SCHEMA)
        .expect("candidate catalogue");
    drop(connection);
    let mut first = 0;
    for index in 0..200 {
        let item = state
            .store
            .insert_item(&NewItem {
                library_id: libraries[0],
                kind: ItemKind::Movie,
                parent_id: None,
                title: format!("Fixture {index:03}"),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("source item");
        state
            .store
            .apply_metadata(
                item,
                &MetadataPatch {
                    overview: Some("x".repeat(8000)),
                    ..Default::default()
                },
            )
            .await
            .expect("bounded large metadata");
        if index == 0 {
            first = item;
        }
    }
    let invitation = uuid::Uuid::new_v4();
    let grant = uuid::Uuid::new_v4();
    let secret = new_secret().expect("synthetic grant secret");
    state
        .store
        .create_share_invitation(InvitationRecord {
            id: invitation,
            token_hash: "a".repeat(64),
            library_ids: vec![libraries[0]],
            created_at_ms: 1000,
            expires_at_ms: 2000,
        })
        .await
        .expect("invitation");
    state
        .store
        .claim_share(ShareClaim {
            invitation_id: invitation,
            invitation_hash: "a".repeat(64),
            claim_id: uuid::Uuid::new_v4(),
            grant_id: grant,
            recipient_server_id: uuid::Uuid::new_v4(),
            recipient_name: "Synthetic recipient".into(),
            credential_hash: secret_hash(SecretDomain::Grant, &secret),
            now_ms: 1001,
        })
        .await
        .expect("grant");
    state
        .store
        .approve_share(grant, 1, 1002)
        .await
        .expect("approval");
    BodyFixture {
        _directory: directory,
        state,
        secret,
        grant,
        library: libraries[0],
        private: libraries[1],
        item: first,
        path,
    }
}
async fn body_server(
    fixture: &BodyFixture,
    body_dropped: Arc<AtomicBool>,
    data_dropped: Arc<AtomicBool>,
) -> (
    std::net::SocketAddr,
    tokio::sync::oneshot::Receiver<crate::SharingConnectionCancellation>,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<anyhow::Result<crate::HttpDrain>>,
) {
    let app = sharing::peer_router(fixture.state.clone()).route(
        "/ordinary/pending",
        get(|| async {
            Response::new(Body::from_stream(futures_util::stream::pending::<
                Result<Bytes, std::convert::Infallible>,
            >()))
        }),
    );
    body_server_app(app, body_dropped, data_dropped).await
}
fn fixture_tracks_shared_body(path: &str) -> bool {
    if path.starts_with("/sharing/v1/art/")
        || (path.contains("/shared/imports/") && path.contains("/art/"))
    {
        return true;
    }
    path.ends_with("/items")
        || path.starts_with("/sharing/v1/items/")
        || (path.contains("/shared/imports/")
            && (path.contains("/items/") || path.ends_with("/continue-watching")))
        || (path.contains("/sharing/imports/") && path.ends_with("/libraries"))
}

#[test]
fn sharing_body_fixture_tracks_continue_watching_and_item_responses() {
    for path in [
        "/api/v1/shared/imports/fixture/continue-watching",
        "/api/v1/sharing/imports/fixture/libraries",
        "/api/v1/shared/imports/fixture/items/7",
        "/api/v1/shared/imports/fixture/libraries/7/items",
        "/sharing/v1/items/7",
    ] {
        assert!(fixture_tracks_shared_body(path), "{path}");
    }
    for path in ["/ordinary/pending", "/api/v1/shared/continue-watching"] {
        assert!(!fixture_tracks_shared_body(path), "{path}");
    }
}

async fn body_server_app(
    app: Router,
    body_dropped: Arc<AtomicBool>,
    data_dropped: Arc<AtomicBool>,
) -> (
    std::net::SocketAddr,
    tokio::sync::oneshot::Receiver<crate::SharingConnectionCancellation>,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<anyhow::Result<crate::HttpDrain>>,
) {
    let socket = tokio::net::TcpSocket::new_v4().expect("fixture socket");
    socket
        .set_send_buffer_size(4096)
        .expect("bounded send buffer");
    socket
        .bind("127.0.0.1:0".parse().expect("loopback address"))
        .expect("bind");
    let listener = socket.listen(8).expect("listen");
    let address = listener.local_addr().expect("address");
    let (accepted, captured) = tokio::sync::oneshot::channel();
    let accepted = Arc::new(Mutex::new(Some(accepted)));
    let app = app.layer(middleware::from_fn(
        move |request: Request<Body>, next: Next| {
            let accepted = accepted.clone();
            let body_dropped = body_dropped.clone();
            let data_dropped = data_dropped.clone();
            async move {
                if let Some(sender) = accepted.lock().expect("capture owner").take() {
                    sender
                        .send(
                            request
                                .extensions()
                                .get::<crate::SharingConnectionCancellation>()
                                .expect("accepted connection seam")
                                .clone(),
                        )
                        .ok();
                }
                let tracked = fixture_tracks_shared_body(request.uri().path());
                let response = next.run(request).await;
                if tracked {
                    let (parts, body) = response.into_parts();
                    Response::from_parts(
                        parts,
                        Body::new(TrackedBody {
                            inner: body,
                            body_dropped,
                            data_dropped,
                        }),
                    )
                } else {
                    response
                }
            }
        },
    ));
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let served = tokio::spawn(crate::serve_http(
        listener,
        app,
        async move {
            let _ = stopped.await;
        },
        crate::HTTP_TIMEOUTS,
    ));
    (address, captured, stop, served)
}
async fn await_flag(flag: &AtomicBool, expected: bool) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while flag.load(Ordering::SeqCst) != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bounded fixture observation");
}

struct ActiveMonitor(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for ActiveMonitor {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[test]
fn sharing_receiver_file_alias_binds_current_lifecycle_and_complete_lossless_reference() {
    use plurx_core::{
        secrets::CredentialKey,
        sharing::{ImportSummary, SharingIdentity},
        sharing_file_locators::FileLocatorKey,
    };
    let summary = ImportSummary {
        id: uuid::Uuid::new_v4(),
        source_server_id: uuid::Uuid::new_v4(),
        catalogue_epoch: uuid::Uuid::new_v4(),
        source_name: "Source".into(),
        claim_id: uuid::Uuid::new_v4(),
        remote_grant_id: Some(uuid::Uuid::new_v4()),
        state: "active".into(),
        assignment_generation: 1,
        lifecycle_generation: i64::MAX,
        endpoint_generation: 1,
        observed_endpoint_revision: None,
        endpoints: vec![],
    };
    let reference = SharedReference {
        import_id: summary.id,
        server_id: summary.source_server_id,
        catalogue_epoch: summary.catalogue_epoch,
        library_id: source_id("0").expect("zero library"),
        item_id: source_id("9223372036854775807").expect("lossless item"),
    };
    // A closed metadata DTO fixture; this test allocates no media/producer.
    let file: plurx_core::sharing_catalogue_details::SourcePlayableFile = serde_json::from_value(json!({"file_id":"0","revision":"a".repeat(64),"size":"23","dolby_vision":{},"audio_offset_ms":0,"probed":true,"audio_streams":[],"subtitle_streams":[],"chapters":[],"skip_regions":[]})).expect("closed Source metadata");
    let master = CredentialKey::from_bytes([59; 32]);
    let receiver = SharingIdentity {
        server_id: uuid::Uuid::new_v4(),
        catalogue_epoch: uuid::Uuid::new_v4(),
        created_at_ms: 1000,
    };
    let envelope =
        FileLocatorKey::generate_sealed(&master, &receiver).expect("sealed B purpose key");
    let key = FileLocatorKey::open(&master, &receiver, &envelope).expect("B key");
    let payload = receiver_file_metadata(&summary, reference.clone(), file.clone(), Some(&key))
        .expect("B alias");
    assert_eq!(payload["file_id"], "0");
    assert_eq!(
        payload["reference"]["item"]["item_id"],
        "9223372036854775807"
    );
    assert_eq!(
        payload["reference"]["lifecycle_generation"].as_i64(),
        Some(i64::MAX)
    );
    let base = payload["file_base"].as_str().expect("advertised alias");
    let locator = base
        .strip_prefix(&format!("/api/v1/shared/imports/{}/files/", summary.id))
        .expect("B-only file namespace");
    assert_eq!(locator.len(), 236);
    let recovered = key
        .verify(locator, summary.id, summary.lifecycle_generation)
        .expect("full signed identity");
    assert_eq!(recovered.item, reference);
    assert_eq!(recovered.file_id.as_str(), "0");
    assert_eq!(recovered.revision.as_str(), "a".repeat(64));
    assert!(key
        .verify(locator, summary.id, summary.lifecycle_generation - 1)
        .is_err());
    let browse = receiver_file_metadata(&summary, reference.clone(), file.clone(), None)
        .expect("browse without purpose readiness");
    assert!(browse.get("file_base").is_none());
    let mut foreign = reference;
    foreign.server_id = uuid::Uuid::new_v4();
    assert!(receiver_file_metadata(&summary, foreign, file, Some(&key)).is_err());
}

#[tokio::test]
async fn sharing_catalogue_connection_monitor_registry_is_bounded_and_owned() {
    let connection = crate::SharingConnectionCancellation::new();
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for _ in 0..32 {
        let active = active.clone();
        connection
            .monitor(async move {
                active.fetch_add(1, Ordering::SeqCst);
                let _owner = ActiveMonitor(active);
                std::future::pending::<()>().await;
            })
            .expect("bounded registry slot");
    }
    assert!(connection.monitor(std::future::pending::<()>()).is_err());
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while active.load(Ordering::SeqCst) != 32 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("all monitors started");
    drop(connection);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while active.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("connection owner aborts and releases every monitor");
}
#[tokio::test]
async fn sharing_catalogue_completed_responses_release_authority_on_keep_alive_connection() {
    let _serial = BODY_FIXTURES.lock().await;
    let fixture = body_fixture().await;
    let baseline = CONTENT_MONITORS.available_permits();
    let (address, captured, stop, served) = body_server(
        &fixture,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    let client = tokio::net::TcpStream::connect(address)
        .await
        .expect("client");
    let (mut sender, driver) =
        hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
            .handshake::<_, Body>(hyper_util::rt::TokioIo::new(client))
            .await
            .expect("HTTP2 handshake");
    let driver = tokio::spawn(driver);
    let request = || {
        Request::builder()
            .uri("http://fixture/sharing/v1/libraries")
            .header(
                "authorization",
                format!("CinemaShare {}", fixture.secret.expose()),
            )
            .body(Body::empty())
            .expect("request")
    };
    // Catalogue JSON is complete when its handler returns, so its
    // pre-return check is the authority point. An exhausted media-body
    // pool cannot refuse it.
    let occupied = CONTENT_MONITORS
        .clone()
        .try_acquire_many_owned(baseline as u32)
        .expect("occupy global media-body pool");
    let response = sender
        .send_request(request())
        .await
        .expect("metadata response");
    assert_eq!(response.status(), StatusCode::OK);
    response
        .into_body()
        .collect()
        .await
        .expect("complete metadata");
    drop(occupied);
    let connection = captured.await.expect("accepted connection");
    // Well past the former bounds (32 per connection, 64 per node): a
    // completed response leaves nothing behind on its keep-alive transport.
    for _ in 0..80 {
        let response = sender
            .send_request(request())
            .await
            .expect("allowed metadata");
        assert_eq!(response.status(), StatusCode::OK);
        response
            .into_body()
            .collect()
            .await
            .expect("fully consumed response");
        assert_eq!(
            CONTENT_MONITORS.available_permits(),
            baseline,
            "a completed response holds no monitor"
        );
    }
    assert!(!connection.0.is_cancelled());
    // Revocation is enforced by the next response's own authority point.
    fixture
        .state
        .store
        .revoke_share(fixture.grant, 2000)
        .await
        .expect("grant revocation");
    let refused = sender
        .send_request(request())
        .await
        .expect("revoked response");
    assert!(!refused.status().is_success());
    refused
        .into_body()
        .collect()
        .await
        .expect("closed refusal body");
    assert!(
        !connection.0.is_cancelled(),
        "a refusal does not close the shared transport"
    );
    drop(sender);
    let _ = driver.await;
    stop.send(()).expect("fixture stop");
    assert_eq!(
        served.await.expect("server task").expect("server result"),
        crate::HttpDrain::Complete
    );
}
#[tokio::test]
async fn sharing_monitored_body_drop_ends_its_monitor_and_releases_its_permit() {
    let pool = Arc::new(tokio::sync::Semaphore::new(1));
    let connection = tokio_util::sync::CancellationToken::new();
    let live = connection.child_token();
    let mut body = MonitoredBody {
        inner: Body::from("complete media"),
        _permit: pool.clone().try_acquire_owned().expect("one body permit"),
        _live: live.clone().drop_guard(),
    };
    assert_eq!(pool.available_permits(), 0);
    let data = body
        .frame()
        .await
        .expect("frame")
        .expect("data frame")
        .into_data()
        .expect("data");
    assert_eq!(&data[..], b"complete media");
    assert!(http_body::Body::is_end_stream(&body));
    // Hyper drops a finished body; EOF alone is not the release.
    assert!(!live.is_cancelled());
    assert_eq!(pool.available_permits(), 0);
    drop(body);
    assert!(live.is_cancelled(), "body drop ends its monitor");
    assert_eq!(pool.available_permits(), 1, "body drop returns its permit");
    assert!(
        !connection.is_cancelled(),
        "a finished response never closes its connection"
    );
}
#[tokio::test]
async fn sharing_catalogue_blocked_http1_body_holds_no_monitor_after_authorization() {
    let _serial = BODY_FIXTURES.lock().await;
    let fixture = body_fixture().await;
    let body_dropped = Arc::new(AtomicBool::new(false));
    let data_dropped = Arc::new(AtomicBool::new(false));
    let baseline = CONTENT_MONITORS.available_permits();
    let (address, captured, stop, served) =
        body_server(&fixture, body_dropped.clone(), data_dropped.clone()).await;
    let socket = tokio::net::TcpSocket::new_v4().expect("client socket");
    socket
        .set_recv_buffer_size(4096)
        .expect("small receive buffer");
    let mut client = socket.connect(address).await.expect("client connection");
    let path = format!("/sharing/v1/libraries/{}/items?limit=200", fixture.library);
    client
        .write_all(
            format!(
                "GET {path} HTTP/1.1\r\nHost: fixture\r\nAuthorization: CinemaShare {}\r\n\r\n",
                fixture.secret.expose()
            )
            .as_bytes(),
        )
        .await
        .expect("catalogue request");
    let mut head = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !head.ends_with(b"\r\n\r\n") {
            head.push(client.read_u8().await.expect("response head"));
        }
    })
    .await
    .expect("response head deadline");
    assert!(
        head.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&head)
    );
    let connection = captured.await.expect("captured connection");
    await_flag(&body_dropped, true).await;
    assert!(
        !data_dropped.load(Ordering::SeqCst),
        "writer retains data behind blocked socket"
    );
    assert_eq!(
        CONTENT_MONITORS.available_permits(),
        baseline,
        "an authorized in-memory body holds no monitor"
    );
    fixture
        .state
        .store
        .revoke_share(fixture.grant, 2000)
        .await
        .expect("grant revocation");
    // Bytes complete before revocation are not re-authorized; the next
    // response is refused at its own authority point.
    let refused = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("fresh client")
        .get(format!("http://{address}{path}"))
        .header(
            "authorization",
            format!("CinemaShare {}", fixture.secret.expose()),
        )
        .send()
        .await
        .expect("fresh request");
    assert!(!refused.status().is_success());
    assert!(!connection.0.is_cancelled());
    drop(client);
    await_flag(&data_dropped, true).await;
    stop.send(()).expect("fixture stop");
    assert_eq!(
        served.await.expect("server task").expect("server result"),
        crate::HttpDrain::Complete
    );
}
#[tokio::test]
async fn sharing_artwork_blocked_http1_http2_bytes_own_their_lease_without_a_monitor() {
    use plurx_core::{
        sharing_artwork::{artwork_expiry, ArtKind, ArtVariant, SourceArtReference},
        sharing_catalogue_details::CatalogueRevisionKey,
    };
    let _serial = BODY_FIXTURES.lock().await;
    let _art_serial = super::super::shared_artwork::ART_FIXTURES.lock().await;
    for h2 in [false, true] {
        let mut fixture = body_fixture().await;
        fixture.state.artwork_dir = fixture
            ._directory
            .path()
            .canonicalize()
            .expect("canonical owned path")
            .join("artwork");
        std::fs::create_dir(&fixture.state.artwork_dir).expect("owned artwork fixture");
        let mut bytes = vec![23u8; 8 * 1024 * 1024];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        std::fs::write(fixture.state.artwork_dir.join("fixture.png"), bytes)
            .expect("bounded original");
        let identity = fixture
            .state
            .store
            .sharing_identity(1000)
            .await
            .expect("identity");
        let envelope =
            CatalogueRevisionKey::generate_sealed(&fixture.state.sharing.key, identity.clone())
                .expect("fixture key");
        let key =
            CatalogueRevisionKey::open(&fixture.state.sharing.key, identity.clone(), &envelope)
                .expect("fixture key open");
        let writer = rusqlite::Connection::open(&fixture.path).expect("fixture writer");
        writer
            .execute_batch(
                plurx_core::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA,
            )
            .expect("candidate key only");
        writer
            .execute(
                "INSERT INTO sharing_catalogue_keys VALUES(1,?1,?2,?3)",
                rusqlite::params![
                    identity.server_id.to_string(),
                    identity.catalogue_epoch.to_string(),
                    envelope.as_stored()
                ],
            )
            .expect("key selection");
        writer
            .execute(
                "UPDATE items SET poster_path='fixture.png' WHERE id=?1",
                [fixture.item],
            )
            .expect("poster");
        drop(writer);
        let now = clock_ms();
        let resource = key
            .issue_art(
                &SourceArtReference {
                    server_id: identity.server_id,
                    catalogue_epoch: identity.catalogue_epoch,
                    grant_id: fixture.grant,
                    library_id: SourceId::parse(&fixture.library.to_string()).expect("library"),
                    item_id: SourceId::parse(&fixture.item.to_string()).expect("item"),
                    kind: ArtKind::Poster,
                    variant: ArtVariant::Original,
                    expires_at_ms: artwork_expiry(now).expect("expiry"),
                },
                now,
            )
            .expect("opaque resource");
        let reference = resource.reference_unverified().expect("closed reference");
        assert!(key.verify_art(&resource, fixture.grant, clock_ms()).is_ok());
        let snapshot = fixture
            .state
            .store
            .source_art_snapshot(
                &secret_hash(SecretDomain::Grant, &fixture.secret),
                fixture.grant,
                &reference,
            )
            .await
            .expect("snapshot query");
        let plurx_core::store::sharing_catalogue_artwork::SourceArtRead::Authorized(snapshot) =
            snapshot
        else {
            panic!("current source artwork snapshot absent");
        };
        assert_eq!(snapshot.filename(), "fixture.png");
        let asset = super::super::images::shared_artwork_asset(
            &fixture.state,
            &snapshot,
            &secret_hash(SecretDomain::Grant, &fixture.secret),
            super::super::shared_artwork::ArtBodyLease::source(false).expect("lease"),
        )
        .await
        .expect("opened image")
        .expect("original exists");
        assert!(asset.still_current().await);
        drop(asset);
        assert!(
            source_art_scope_current(&fixture.state, &reference).await,
            "current source scope"
        );
        if !h2 {
            // The handler bounds the whole request at 5 s and its pre-demand
            // authority re-read at 1 s. Both are real request deadlines, but
            // this block proves the variant answer and demand dedup, so the
            // engine attestation the daemon establishes once per process is
            // warmed first and Tokio time is held: blocking store work then
            // cannot advance those deadlines, however loaded the runner is.
            crate::ffmpeg::fragment_index_engine_digest().await;
            tokio::time::pause();
            let variant = plurx_core::sharing_artwork::SourceArtReference {
                variant: ArtVariant::W300,
                ..reference.clone()
            };
            let variant = key
                .issue_art(&variant, clock_ms())
                .expect("exact canonical variant");
            let mut headers = HeaderMap::new();
            headers.insert(
                "authorization",
                axum::http::HeaderValue::from_str(&format!(
                    "CinemaShare {}",
                    fixture.secret.expose()
                ))
                .expect("fixture auth"),
            );
            for _ in 0..2 {
                let response = super::super::shared_artwork::source(
                    State(fixture.state.clone()),
                    headers.clone(),
                    Path(variant.as_str().to_owned()),
                    RawQuery(None),
                )
                .await
                .expect_err("unpublished variant never falls back to original")
                .into_response();
                assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
            }
            let jobs = rusqlite::Connection::open(&fixture.path)
                .expect("job census")
                .query_row(
                    "SELECT count(*) FROM background_jobs WHERE kind='artwork_derivative'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("count");
            assert_eq!(jobs, 1, "canonical demand is deduplicated");
            headers.insert(
                "authorization",
                axum::http::HeaderValue::from_str(&format!(
                    "CinemaShare {}",
                    new_secret().expect("wrong credential").expose()
                ))
                .expect("auth"),
            );
            let denied = super::super::shared_artwork::source(
                State(fixture.state.clone()),
                headers,
                Path(variant.as_str().to_owned()),
                RawQuery(None),
            )
            .await
            .expect_err("wrong credential before demand")
            .into_response();
            assert_eq!(denied.status(), StatusCode::NOT_FOUND);
            assert_eq!(
                rusqlite::Connection::open(&fixture.path)
                    .expect("job census")
                    .query_row(
                        "SELECT count(*) FROM background_jobs WHERE kind='artwork_derivative'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .expect("count"),
                jobs,
                "refused authority cannot add render work"
            );
            tokio::time::resume();
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let data = Arc::new(AtomicBool::new(false));
        let before = super::super::shared_artwork::source_capacity();
        let monitors = CONTENT_MONITORS.available_permits();
        let (address, captured, stop, served) =
            body_server(&fixture, dropped.clone(), data.clone()).await;
        let socket = tokio::net::TcpSocket::new_v4().expect("socket");
        socket.set_recv_buffer_size(4096).expect("bounded window");
        let client = socket.connect(address).await.expect("connect");
        let mut raw = None;
        let mut h2_sender = None;
        let mut h2_driver = None;
        let mut h2_body = None;
        if h2 {
            let (mut sender, driver) =
                hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .initial_stream_window_size(1024)
                    .initial_connection_window_size(1024)
                    .handshake::<_, Body>(hyper_util::rt::TokioIo::new(client))
                    .await
                    .expect("H2");
            h2_driver = Some(tokio::spawn(driver));
            let response = sender
                .send_request(
                    Request::builder()
                        .uri(format!(
                            "http://fixture/sharing/v1/art/{}",
                            resource.as_str()
                        ))
                        .header(
                            "authorization",
                            format!("CinemaShare {}", fixture.secret.expose()),
                        )
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("art head");
            assert_eq!(response.status(), StatusCode::OK);
            h2_body = Some(response.into_body());
            h2_sender = Some(sender);
        } else {
            let mut client = client;
            client.write_all(format!("GET /sharing/v1/art/{} HTTP/1.1\r\nHost: fixture\r\nAuthorization: CinemaShare {}\r\n\r\n",resource.as_str(),fixture.secret.expose()).as_bytes()).await.expect("request");
            let mut head = Vec::new();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(client.read_u8().await.expect("head"));
                }
            })
            .await
            .expect("head deadline");
            assert!(
                head.starts_with(b"HTTP/1.1 200"),
                "{}",
                String::from_utf8_lossy(&head)
            );
            raw = Some(client);
        }
        let connection = captured.await.expect("connection");
        await_flag(&dropped, true).await;
        assert!(
            !data.load(Ordering::SeqCst),
            "queued DATA retains actual bytes"
        );
        assert_eq!(
            super::super::shared_artwork::source_capacity(),
            (before.0 - 1, before.1 - 8193),
            "queued bytes own their lease"
        );
        assert_eq!(
            CONTENT_MONITORS.available_permits(),
            monitors,
            "artwork is authorized once and holds no monitor"
        );
        let art_request = || {
            Request::builder()
                .uri(format!(
                    "http://fixture/sharing/v1/art/{}",
                    resource.as_str()
                ))
                .header(
                    "authorization",
                    format!("CinemaShare {}", fixture.secret.expose()),
                )
                .body(Body::empty())
                .expect("art request")
        };
        if h2 {
            h2_body
                .take()
                .expect("first artwork body")
                .collect()
                .await
                .expect("consume artwork");
            // Five completed reads against four operation permits: each
            // returns its lease once its bytes have left the writer.
            for _ in 0..4 {
                tokio::time::timeout(std::time::Duration::from_secs(3), async {
                    while super::super::shared_artwork::source_capacity() != before {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("written bytes return their lease");
                let response = h2_sender
                    .as_mut()
                    .expect("sender")
                    .send_request(art_request())
                    .await
                    .expect("completed art");
                assert_eq!(response.status(), StatusCode::OK);
                response.into_body().collect().await.expect("consume");
            }
            assert!(!connection.0.is_cancelled());
        } else {
            fixture
                .state
                .store
                .revoke_share(fixture.grant, clock_ms())
                .await
                .expect("revoke");
            // Revocation reaches the next fresh read, never bytes that were
            // already authorized and queued.
            let refused = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .expect("fresh client")
                .get(format!(
                    "http://{address}/sharing/v1/art/{}",
                    resource.as_str()
                ))
                .header(
                    "authorization",
                    format!("CinemaShare {}", fixture.secret.expose()),
                )
                .send()
                .await
                .expect("fresh art read");
            assert!(!refused.status().is_success());
            assert!(!connection.0.is_cancelled());
            drop(raw.take());
            await_flag(&data, true).await;
        }
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while super::super::shared_artwork::source_capacity() != before {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("bytes return their lease once they leave the writer");
        drop(h2_body);
        drop(h2_sender);
        if let Some(driver) = h2_driver {
            driver.abort();
            let _ = driver.await;
        }
        stop.send(()).expect("stop");
        assert_eq!(
            served.await.expect("server task").expect("server"),
            crate::HttpDrain::Complete
        );
    }
}
#[tokio::test]
async fn sharing_catalogue_viewer_routes_require_login_and_fail_closed_without_import() {
    let (app, state) = super::super::tests::test_app_with_state();
    let import = uuid::Uuid::new_v4();
    let path = format!("/api/v1/shared/imports/{import}/libraries/12/items");
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri(&path)
                .body(Body::empty())
                .expect("synthetic catalogue fixture"),
        )
        .await
        .expect("synthetic catalogue fixture");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/setup")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username":"catalogue-viewer","password":"synthetic-password"})
                        .to_string(),
                ))
                .expect("synthetic catalogue fixture"),
        )
        .await
        .expect("synthetic catalogue fixture");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("synthetic catalogue fixture")
        .to_bytes();
    let setup: Value = serde_json::from_slice(&bytes).expect("synthetic catalogue fixture");
    let token = setup["token"]
        .as_str()
        .expect("synthetic catalogue fixture");
    for (path, status) in [
        (path, StatusCode::SERVICE_UNAVAILABLE),
        (
            format!("/api/v1/shared/imports/{import}/libraries/01/items"),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("/api/v1/shared/imports/{import}/libraries/12/items?q=%FF"),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(path)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("synthetic catalogue fixture"),
            )
            .await
            .expect("synthetic catalogue fixture");
        assert_eq!(response.status(), status);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("synthetic catalogue fixture")
            .to_bytes();
        let body = String::from_utf8(bytes.to_vec()).expect("synthetic catalogue fixture");
        assert!(!body.contains("synthetic-password"));
    }
    // Manual watched state needs a login, canonical identities and a fresh
    // Source read under a current import; none of these fall back.
    let watched = format!("/api/v1/shared/imports/{import}/items/9007199254740993/watched");
    for (path, login, body, status) in [
        (
            watched.clone(),
            false,
            r#"{"watched":false}"#,
            StatusCode::UNAUTHORIZED,
        ),
        (
            watched.clone(),
            true,
            r#"{"watched":false}"#,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            watched.clone(),
            true,
            r#"{"watched":true}"#,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (watched.clone(), true, "", StatusCode::BAD_REQUEST),
        (
            watched.clone(),
            true,
            r#"{"watched":"no"}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            watched.clone(),
            true,
            r#"{"watched":false,"position_ms":0}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            watched.replace("9007199254740993", "01"),
            true,
            r#"{"watched":false}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            watched.replace(&import.to_string(), &import.to_string().to_uppercase()),
            true,
            r#"{"watched":false}"#,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let mut request = axum::http::Request::builder().method("POST").uri(path);
        if login {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = app
            .clone()
            .oneshot(
                request
                    .body(Body::from(body))
                    .expect("synthetic catalogue fixture"),
            )
            .await
            .expect("synthetic catalogue fixture");
        assert_eq!(response.status(), status, "{body}");
    }
    assert!(state
        .store
        .sharing_imports()
        .await
        .expect("synthetic catalogue fixture")
        .is_empty());
}
enum BlockedReceiverClient {
    Http1(tokio::net::TcpStream),
    Http2(
        hyper::client::conn::http2::SendRequest<Body>,
        hyper::body::Incoming,
        tokio::task::JoinHandle<Result<(), hyper::Error>>,
    ),
}
async fn blocked_receiver_client(
    address: std::net::SocketAddr,
    path: &str,
    token: &str,
    h2: bool,
) -> BlockedReceiverClient {
    let socket = tokio::net::TcpSocket::new_v4().expect("receiver client socket");
    socket
        .set_recv_buffer_size(4096)
        .expect("small receiver window");
    let mut client = socket.connect(address).await.expect("receiver client");
    if h2 {
        let (mut sender, driver) =
            hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                .initial_stream_window_size(1024)
                .initial_connection_window_size(2048)
                .handshake::<_, Body>(hyper_util::rt::TokioIo::new(client))
                .await
                .expect("receiver H2 handshake");
        let driver = tokio::spawn(driver);
        let response = sender
            .send_request(
                Request::builder()
                    .uri(format!("http://fixture{path}"))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("receiver response");
        assert_eq!(response.status(), StatusCode::OK);
        BlockedReceiverClient::Http2(sender, response.into_body(), driver)
    } else {
        client
            .write_all(
                format!(
                    "GET {path} HTTP/1.1\r\nHost: fixture\r\nAuthorization: Bearer {token}\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .expect("receiver request");
        let mut head = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !head.ends_with(b"\r\n\r\n") {
                head.push(client.read_u8().await.expect("receiver header"));
            }
        })
        .await
        .expect("receiver headers deadline");
        assert!(
            head.starts_with(b"HTTP/1.1 200"),
            "{}",
            String::from_utf8_lossy(&head)
        );
        BlockedReceiverClient::Http1(client)
    }
}
async fn release_receiver_client(client: BlockedReceiverClient) {
    match client {
        BlockedReceiverClient::Http1(socket) => drop(socket),
        BlockedReceiverClient::Http2(sender, body, driver) => {
            drop(body);
            drop(sender);
            let _ = driver.await;
        }
    }
}
#[tokio::test]
async fn sharing_receiver_blocked_http1_and_http2_refuse_next_response_on_login_loss() {
    let _serial = BODY_FIXTURES.lock().await;
    for (h2, loss) in [false, true]
        .into_iter()
        .flat_map(|h2| (0..3).map(move |loss| (h2, loss)))
    {
        let fixture = body_fixture().await;
        let user = fixture
            .state
            .store
            .create_user("receiver-login-viewer", "synthetic-hash", false)
            .await
            .expect("viewer");
        let token = "synthetic-receiver-login";
        let hash = plurx_core::auth::hash_token(token);
        fixture
            .state
            .store
            .create_token(&hash, user.id, None)
            .await
            .expect("login");
        let import = uuid::Uuid::new_v4();
        let local = fixture
            .state
            .store
            .sharing_identity(1000)
            .await
            .expect("B identity");
        let seal = |purpose, value: &str| {
            fixture
                .state
                .sharing
                .key
                .seal_sharing(purpose, local.server_id, import, value)
                .expect("sealed fixture")
        };
        fixture
            .state
            .store
            .create_share_import(NewImport {
                id: import,
                source: SharingIdentity {
                    server_id: uuid::Uuid::new_v4(),
                    catalogue_epoch: uuid::Uuid::new_v4(),
                    created_at_ms: 1000,
                },
                source_name: "Configured offline Source".into(),
                claim_id: uuid::Uuid::new_v4(),
                credential: seal(
                    plurx_core::secrets::SharingSecretPurpose::Credential,
                    "synthetic-credential",
                ),
                claim_secret: seal(
                    plurx_core::secrets::SharingSecretPurpose::Claim,
                    "synthetic-claim",
                ),
                endpoints: vec![Endpoint {
                    ipv4: "100.101.102.103".parse().expect("IP"),
                    ipv6: None,
                    ts_fqdn: "offline.fixture.ts.net".into(),
                    port: 1,
                    spki_sha256: "a".repeat(64),
                }],
                now_ms: 1000,
            })
            .await
            .expect("import");
        fixture
            .state
            .store
            .settle_share_claim(import, uuid::Uuid::new_v4(), true, 1001)
            .await
            .expect("claim");
        fixture
            .state
            .store
            .assign_share_viewers(
                import,
                1,
                vec![Assignment {
                    library_id: source_id("12").expect("ID"),
                    user_id: user.id,
                }],
                1002,
            )
            .await
            .expect("assignment");
        let summary = fixture
            .state
            .store
            .sharing_imports()
            .await
            .expect("imports")
            .into_iter()
            .find(|i| i.id == import)
            .expect("current import");
        let scope = receiver_scope(
            &summary,
            vec![source_id("12").expect("ID")],
            Vec::new(),
            Vec::new(),
        )
        .expect("scope")
        .scope;
        let app=Router::new().route("/test/shared/items",get(move |State(state):State<AppState>,super::super::extract::AuthUser(user):super::super::extract::AuthUser,super::super::extract::RawToken(token):super::super::extract::RawToken| { let scope = scope.clone(); async move{receiver_json_scoped(&state,&token,user.id,vec![scope],Vec::new(),json!({"receiver_test_payload":"x".repeat(2*1024*1024)})).await} }))
                .route_layer(middleware::from_fn_with_state(fixture.state.clone(),receiver_content_guard)).with_state(fixture.state.clone());
        let body_dropped = Arc::new(AtomicBool::new(false));
        let data_dropped = Arc::new(AtomicBool::new(false));
        let (address, captured, stop, served) =
            body_server_app(app, body_dropped.clone(), data_dropped.clone()).await;
        let client = blocked_receiver_client(address, "/test/shared/items", token, h2).await;
        let connection = captured.await.expect("accepted receiver connection");
        await_flag(&body_dropped, true).await;
        assert!(
            !data_dropped.load(Ordering::SeqCst),
            "actual receiver DATA remains blocked"
        );
        match loss {
            0 => {
                assert!(fixture
                    .state
                    .store
                    .delete_token(&hash)
                    .await
                    .expect("revoke login"));
            }
            1 => fixture
                .state
                .store
                .disable_share_import(import, 1003)
                .await
                .expect("disable import"),
            2 => {
                fixture
                    .state
                    .store
                    .assign_share_viewers(import, 2, Vec::new(), 1003)
                    .await
                    .expect("remove assignment");
            }
            _ => unreachable!(),
        }
        // The blocked body was complete and authorized before the loss; the
        // next response is refused at its own authority point.
        let refused = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .expect("fresh client")
            .get(format!("http://{address}/test/shared/items"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .await
            .expect("fresh receiver request");
        assert!(!refused.status().is_success(), "loss {loss}");
        assert!(!connection.0.is_cancelled());
        release_receiver_client(client).await;
        await_flag(&data_dropped, true).await;
        stop.send(()).expect("stop");
        assert_eq!(
            served.await.expect("server task").expect("server result"),
            crate::HttpDrain::Complete
        );
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires disposable CGNAT network and PLURX_SHARING_FIXTURE_IP"]
async fn sharing_receiver_pinned_decision_http1_http2_revalidates_file_assignment_and_login() {
    use http_body_util::BodyExt;
    use plurx_core::{
        config::{SharingEgressConfig, SharingNetworkConfig},
        secrets::SharingSecretPurpose,
        sharing_catalogue_details::CatalogueRevisionKey,
        sharing_file_locators::{FileLocatorKey, FileLocatorReference},
        sharing_tls::{LiveNodeTls, SharingTlsListener},
        store::SqliteStore,
    };
    async fn exchange(
        address: std::net::SocketAddr,
        h2: bool,
        request: Request<Body>,
    ) -> (StatusCode, Bytes) {
        let socket = tokio::net::TcpStream::connect(address)
            .await
            .expect("B socket");
        if h2 {
            let (mut sender, driver) =
                hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .handshake::<_, Body>(hyper_util::rt::TokioIo::new(socket))
                    .await
                    .expect("H2 handshake");
            let driver = tokio::spawn(driver);
            let response = sender.send_request(request).await.expect("B response");
            let status = response.status();
            let payload = response
                .into_body()
                .collect()
                .await
                .expect("B body")
                .to_bytes();
            drop(sender);
            driver.abort();
            let _ = driver.await;
            (status, payload)
        } else {
            let (mut sender, driver) = hyper::client::conn::http1::handshake::<_, Body>(
                hyper_util::rt::TokioIo::new(socket),
            )
            .await
            .expect("H1 handshake");
            let driver = tokio::spawn(driver);
            let response = sender.send_request(request).await.expect("B response");
            let status = response.status();
            let payload = response
                .into_body()
                .collect()
                .await
                .expect("B body")
                .to_bytes();
            drop(sender);
            driver.abort();
            let _ = driver.await;
            (status, payload)
        }
    }
    let _serial = BODY_FIXTURES.lock().await;
    let address: std::net::IpAddr = std::env::var("PLURX_SHARING_FIXTURE_IP")
        .expect("explicit CGNAT fixture")
        .parse()
        .expect("address");
    assert!(is_tailnet_address(address));
    for h2 in [false, true] {
        let source = body_fixture().await;
        let tls_dir = tempfile::tempdir().expect("Source TLS fixture");
        let tls = Arc::new(
            LiveNodeTls::open(&tls_dir.path().join("tls"), clock_ms() / 1000).expect("Source TLS"),
        );
        let (pin, _) = tls.status().expect("Source SPKI");
        let listener = tokio::net::TcpListener::bind((address, 0))
            .await
            .expect("Source CGNAT listener");
        let endpoint = Endpoint {
            ipv4: match address {
                std::net::IpAddr::V4(ip) => ip,
                _ => panic!("IPv4 fixture"),
            },
            ipv6: None,
            ts_fqdn: "source.fixture.ts.net".into(),
            port: listener.local_addr().expect("Source address").port(),
            spki_sha256: pin,
        };
        let (source_stop, source_stopped) = tokio::sync::oneshot::channel();
        let source_task = tokio::spawn(crate::serve_http(
            SharingTlsListener::new(listener, tls),
            sharing::peer_router(source.state.clone()),
            async move {
                let _ = source_stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        let receiver_dir = tempfile::tempdir().expect("B fixture");
        let receiver_path = receiver_dir.path().join("receiver.sqlite");
        let (_, mut receiver) = super::super::tests::test_app_with_state();
        receiver.store = Arc::new(SqliteStore::open(&receiver_path).expect("B Store"));
        receiver.runtime_cache_dir = receiver_dir
            .path()
            .canonicalize()
            .expect("canonical B cache")
            .join("runtime");
        std::fs::create_dir(&receiver.runtime_cache_dir).expect("runtime cache fixture");
        receiver.sharing = Arc::new(crate::sharing::SharingManager::new(
            receiver.sharing.key.clone(),
            receiver_dir.path().join("unused-tls"),
            SharingNetworkConfig {
                bind: "127.0.0.1:32444".parse().expect("unused bind"),
                egress: SharingEgressConfig::LocalAddress { address },
                ..SharingNetworkConfig::default()
            },
        ));
        receiver
            .store
            .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
            .await
            .expect("enable B");
        let user = receiver
            .store
            .create_user("receiver-current-viewer", "synthetic-hash", false)
            .await
            .expect("viewer");
        let token = "synthetic-receiver-current-login";
        let hash = plurx_core::auth::hash_token(token);
        receiver
            .store
            .create_token(&hash, user.id, None)
            .await
            .expect("login");
        let local = receiver
            .store
            .sharing_identity(1000)
            .await
            .expect("B identity");
        let remote = source
            .state
            .store
            .sharing_identity(1000)
            .await
            .expect("Source identity");
        rusqlite::Connection::open(&source.path)
            .expect("Source fixture writer")
            .execute(
                "UPDATE sharing_exports SET recipient_server_id=?1 WHERE id=?2",
                rusqlite::params![local.server_id.to_string(), source.grant.to_string()],
            )
            .expect("bind grant to actual B identity");
        let import = uuid::Uuid::new_v4();
        let credential = crate::sharing::ImportCredential::encode(
            &source.secret,
            uuid::Uuid::new_v4(),
            1000,
            "Fixture B",
            2000,
            None,
        )
        .expect("closed credential");
        assert_eq!(
            receiver
                .store
                .create_share_import(NewImport {
                    id: import,
                    source: remote.clone(),
                    source_name: "Fixture Source".into(),
                    claim_id: uuid::Uuid::new_v4(),
                    credential: receiver
                        .sharing
                        .key
                        .seal_sharing(
                            SharingSecretPurpose::Credential,
                            local.server_id,
                            import,
                            credential.expose()
                        )
                        .expect("seal credential"),
                    claim_secret: receiver
                        .sharing
                        .key
                        .seal_sharing(
                            SharingSecretPurpose::Claim,
                            local.server_id,
                            import,
                            "synthetic-claim"
                        )
                        .expect("seal claim"),
                    endpoints: vec![endpoint.clone()],
                    now_ms: 1000
                })
                .await
                .expect("B import"),
            ImportOutcome::Created
        );
        receiver
            .store
            .settle_share_claim(import, source.grant, true, 1001)
            .await
            .expect("active B import");
        let library = source_id(&source.library.to_string()).expect("ID");
        receiver
            .store
            .assign_share_viewers(
                import,
                1,
                vec![Assignment {
                    library_id: library.clone(),
                    user_id: user.id,
                }],
                1002,
            )
            .await
            .expect("assignment");

        let source_file = source._directory.path().join("decision.mp4");
        std::fs::write(&source_file, b"actual fixture presence").expect("Source file");
        let envelope =
            CatalogueRevisionKey::generate_sealed(&source.state.sharing.key, remote.clone())
                .expect("Source key");
        let source_key =
            CatalogueRevisionKey::open(&source.state.sharing.key, remote.clone(), &envelope)
                .expect("Source signer");
        let writer = rusqlite::Connection::open(&source.path).expect("Source writer");
        writer
            .execute_batch(
                plurx_core::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA,
            )
            .expect("Source schema");
        writer
            .execute(
                "INSERT INTO sharing_catalogue_keys VALUES(1,?1,?2,?3)",
                rusqlite::params![
                    remote.server_id.to_string(),
                    remote.catalogue_epoch.to_string(),
                    envelope.as_stored()
                ],
            )
            .expect("Source key");
        writer.execute("INSERT INTO files(id,item_id,path,size,mtime,duration_ms,container,video_codec,width,height,bit_depth,bitrate) VALUES(0,?1,?2,23,1000,60000,'mp4','h264',1920,1080,8,1000000)",rusqlite::params![source.item,source_file.to_str().expect("path")]).expect("Source file identity zero");
        drop(writer);
        let hash = secret_hash(SecretDomain::Grant, &source.secret);
        let plurx_core::store::sharing_catalogue_details::SourceDetailsRead::Authorized(witness) =
            source
                .state
                .store
                .source_item_file_witness(
                    &hash,
                    source.grant,
                    source_id(&source.item.to_string()).expect("item"),
                    source_id("0").expect("file"),
                )
                .await
                .expect("witness")
        else {
            panic!("authorized")
        };
        let reference = FileLocatorReference {
            item: SharedReference {
                import_id: import,
                server_id: remote.server_id,
                catalogue_epoch: remote.catalogue_epoch,
                library_id: library.clone(),
                item_id: source_id(&source.item.to_string()).expect("item"),
            },
            file_id: source_id("0").expect("zero"),
            revision: source_key.file_revision(&witness).expect("revision"),
            lifecycle_generation: 1,
        };
        let envelope =
            FileLocatorKey::generate_sealed(&receiver.sharing.key, &local).expect("B key");
        let writer = rusqlite::Connection::open(&receiver_path).expect("B writer");
        writer
            .execute_batch(
                plurx_core::store::sharing_file_locators::CANDIDATE_FILE_LOCATOR_KEY_SCHEMA,
            )
            .expect("B schema");
        writer
            .execute(
                "INSERT INTO sharing_file_locator_keys VALUES(1,?1,?2,?3)",
                rusqlite::params![
                    local.server_id.to_string(),
                    local.catalogue_epoch.to_string(),
                    envelope.as_stored()
                ],
            )
            .expect("B key");
        drop(writer);
        let key = FileLocatorKey::open(&receiver.sharing.key, &local, &envelope).expect("B signer");
        let locator = key.issue(&reference).expect("signed locator");
        let caps:plurx_core::playback::DeviceCaps=serde_json::from_value(json!({"v":2,"video":[{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"]})).expect("caps");
        let input = super::super::shared_playback::SourceDecisionRequest {
            reference: super::super::hls::SourcePlaybackTarget {
                server_id: remote.server_id,
                catalogue_epoch: remote.catalogue_epoch,
                library_id: library.clone(),
                item_id: reference.item.item_id.clone(),
                file_id: reference.file_id.clone(),
                revision: reference.revision.clone(),
            },
            caps: caps.clone(),
            audio: None,
            subtitle: None,
            audio_offset_ms: None,
            force: None,
        };
        let (_, expected) = receiver
            .sharing
            .read_file_decision(&receiver, user.id, &reference, &input)
            .await
            .expect("actual pinned Source decision");
        let expected =
            super::super::sharing_decision_decode::DecodedDecision::parse(expected.decision, 0)
                .expect("complete engine");
        let expected = serde_json::to_value(
            super::super::sharing_playback_wire::project_decoded_decision(
                expected, &reference, &key,
            )
            .expect("expected B projection"),
        )
        .expect("JSON");
        let (b_address, _captured, stop, served) = body_server_app(
            super::super::router(receiver.clone()),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        // A real authenticated item read must advertise the same signed
        // tuple used by decision clients; no client invents its lifecycle.
        let request = Request::builder()
            .uri(format!(
                "http://fixture/api/v1/shared/imports/{import}/items/{}",
                source.item
            ))
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .expect("B item request");
        let (status, payload) = exchange(b_address, h2, request).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&payload)
        );
        let details: Value = serde_json::from_slice(&payload).expect("actual B item details");
        assert_eq!(details["lifecycle_generation"].as_i64(), Some(1));
        let advertised = details["files"]
            .as_array()
            .expect("files")
            .iter()
            .find(|file| file["file_id"] == "0")
            .expect("Source file zero");
        assert_eq!(advertised["file_base"], locator.file_base());
        assert_eq!(
            advertised["reference"]["lifecycle_generation"].as_i64(),
            Some(1)
        );
        assert_eq!(
            advertised["reference"]["item"],
            serde_json::to_value(&reference.item).expect("full reference")
        );
        let path = format!(
            "{}/decision",
            advertised["file_base"].as_str().expect("B alias")
        );
        for case in 0..5 {
            if case == 2 {
                rusqlite::Connection::open(&source.path)
                    .expect("Source writer")
                    .execute("UPDATE files SET mtime=mtime+1 WHERE id=0", [])
                    .expect("change actual revision");
            }
            if case == 3 {
                rusqlite::Connection::open(&source.path)
                    .expect("Source writer")
                    .execute("UPDATE files SET mtime=1000 WHERE id=0", [])
                    .expect("restore revision");
                receiver
                    .store
                    .assign_share_viewers(import, 2, vec![], 1003)
                    .await
                    .expect("remove current viewer assignment");
            }
            if case == 4 {
                receiver
                    .store
                    .assign_share_viewers(
                        import,
                        3,
                        vec![Assignment {
                            library_id: library.clone(),
                            user_id: user.id,
                        }],
                        1004,
                    )
                    .await
                    .expect("restore current assignment");
                receiver
                    .store
                    .delete_token(&plurx_core::auth::hash_token(token))
                    .await
                    .expect("revoke actual B login");
            }
            let request_path = if case == 1 {
                let mut forged = locator.file_base();
                let last = forged.pop().expect("nonempty signed locator");
                forged.push(if last == 'a' { 'b' } else { 'a' });
                format!("{forged}/decision")
            } else {
                path.clone()
            };
            let request = Request::builder()
                .method("POST")
                .uri(format!("http://fixture{request_path}"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"caps":caps})).expect("body"),
                ))
                .expect("request");
            let (status, payload) = exchange(b_address, h2, request).await;
            if case == 0 {
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "{}",
                    String::from_utf8_lossy(&payload)
                );
                assert_eq!(
                    serde_json::from_slice::<Value>(&payload).expect("complete B engine"),
                    expected
                );
            } else {
                assert!(
                    !status.is_success(),
                    "must refuse changed authority H2={h2} case={case}"
                );
            }
            for database in [&source.path, &receiver_path] {
                let sessions: i64 = rusqlite::Connection::open(database)
                    .expect("session census")
                    .query_row("SELECT count(*) FROM media_sessions", [], |r| r.get(0))
                    .expect("census");
                assert_eq!(sessions, 0, "decisions never allocate Source/B sessions");
            }
        }
        let _ = stop.send(());
        served.await.expect("B server").expect("B shutdown");
        let _ = source_stop.send(());
        source_task
            .await
            .expect("Source server")
            .expect("Source shutdown");
    }
}
/// Runs only inside the explicitly assigned disposable CGNAT container.
/// The real Source router, SPKI dialer, identity check, receiver router and
/// Store authorities remain in the path.
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires disposable CGNAT network and PLURX_SHARING_FIXTURE_IP"]
async fn sharing_receiver_pinned_source_blocked_http1_http2_revalidate_current_scope() {
    use plurx_core::{
        config::{SharingEgressConfig, SharingNetworkConfig},
        secrets::SharingSecretPurpose,
        sharing_tls::{LiveNodeTls, SharingTlsListener},
        store::SqliteStore,
    };
    let _serial = BODY_FIXTURES.lock().await;
    let address: std::net::IpAddr = std::env::var("PLURX_SHARING_FIXTURE_IP")
        .expect("explicit disposable CGNAT address")
        .parse()
        .expect("numeric address");
    assert!(is_tailnet_address(address));
    for h2 in [false, true] {
        for change in 0..16 {
            let mut source = body_fixture().await;
            let artwork = change >= 9;
            if artwork {
                source.state.artwork_dir = source
                    ._directory
                    .path()
                    .canonicalize()
                    .expect("canonical Source art")
                    .join("artwork");
                std::fs::create_dir(&source.state.artwork_dir).expect("art namespace");
                let mut bytes = vec![23u8; 8 * 1024 * 1024];
                bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
                std::fs::write(source.state.artwork_dir.join("fixture.png"), bytes)
                    .expect("bounded asset");
                rusqlite::Connection::open(&source.path)
                    .expect("poster writer")
                    .execute(
                        "UPDATE items SET poster_path='fixture.png' WHERE id=?1",
                        [source.item],
                    )
                    .expect("poster");
            }
            let tls_dir = tempfile::tempdir().expect("Source TLS fixture");
            let tls = Arc::new(
                LiveNodeTls::open(&tls_dir.path().join("tls"), clock_ms() / 1000)
                    .expect("Source TLS"),
            );
            let (pin, _) = tls.status().expect("Source SPKI");
            let listener = tokio::net::TcpListener::bind((address, 0))
                .await
                .expect("Source CGNAT listener");
            let endpoint = Endpoint {
                ipv4: match address {
                    std::net::IpAddr::V4(ip) => ip,
                    _ => panic!("IPv4 fixture"),
                },
                ipv6: None,
                ts_fqdn: "source.fixture.ts.net".into(),
                port: listener.local_addr().expect("Source address").port(),
                spki_sha256: pin,
            };
            let (source_stop, source_stopped) = tokio::sync::oneshot::channel();
            let source_task = tokio::spawn(crate::serve_http(
                SharingTlsListener::new(listener, tls),
                sharing::peer_router(source.state.clone()),
                async move {
                    let _ = source_stopped.await;
                },
                crate::HTTP_TIMEOUTS,
            ));
            let receiver_dir = tempfile::tempdir().expect("B fixture");
            let receiver_path = receiver_dir.path().join("receiver.sqlite");
            let (_, mut receiver) = super::super::tests::test_app_with_state();
            receiver.store = Arc::new(SqliteStore::open(&receiver_path).expect("B Store"));
            receiver.runtime_cache_dir = receiver_dir
                .path()
                .canonicalize()
                .expect("canonical B cache")
                .join("runtime");
            std::fs::create_dir(&receiver.runtime_cache_dir).expect("runtime cache fixture");
            receiver.sharing = Arc::new(crate::sharing::SharingManager::new(
                receiver.sharing.key.clone(),
                receiver_dir.path().join("unused-tls"),
                SharingNetworkConfig {
                    bind: "127.0.0.1:32444".parse().expect("unused bind"),
                    egress: SharingEgressConfig::LocalAddress { address },
                    ..SharingNetworkConfig::default()
                },
            ));
            receiver
                .store
                .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
                .await
                .expect("enable B");
            let user = receiver
                .store
                .create_user("receiver-current-viewer", "synthetic-hash", false)
                .await
                .expect("viewer");
            let token = "synthetic-receiver-current-login";
            let hash = plurx_core::auth::hash_token(token);
            receiver
                .store
                .create_token(&hash, user.id, None)
                .await
                .expect("login");
            let local = receiver
                .store
                .sharing_identity(1000)
                .await
                .expect("B identity");
            let remote = source
                .state
                .store
                .sharing_identity(1000)
                .await
                .expect("Source identity");
            rusqlite::Connection::open(&source.path)
                .expect("Source fixture writer")
                .execute(
                    "UPDATE sharing_exports SET recipient_server_id=?1 WHERE id=?2",
                    rusqlite::params![local.server_id.to_string(), source.grant.to_string()],
                )
                .expect("bind grant to actual B identity");
            let import = uuid::Uuid::new_v4();
            let credential = crate::sharing::ImportCredential::encode(
                &source.secret,
                uuid::Uuid::new_v4(),
                1000,
                "Fixture B",
                2000,
                None,
            )
            .expect("closed credential");
            assert_eq!(
                receiver
                    .store
                    .create_share_import(NewImport {
                        id: import,
                        source: remote.clone(),
                        source_name: "Fixture Source".into(),
                        claim_id: uuid::Uuid::new_v4(),
                        credential: receiver
                            .sharing
                            .key
                            .seal_sharing(
                                SharingSecretPurpose::Credential,
                                local.server_id,
                                import,
                                credential.expose()
                            )
                            .expect("seal credential"),
                        claim_secret: receiver
                            .sharing
                            .key
                            .seal_sharing(
                                SharingSecretPurpose::Claim,
                                local.server_id,
                                import,
                                "synthetic-claim"
                            )
                            .expect("seal claim"),
                        endpoints: vec![endpoint.clone()],
                        now_ms: 1000
                    })
                    .await
                    .expect("B import"),
                ImportOutcome::Created
            );
            receiver
                .store
                .settle_share_claim(import, source.grant, true, 1001)
                .await
                .expect("active B import");
            let library = source_id(&source.library.to_string()).expect("ID");
            receiver
                .store
                .assign_share_viewers(
                    import,
                    1,
                    vec![Assignment {
                        library_id: library.clone(),
                        user_id: user.id,
                    }],
                    1002,
                )
                .await
                .expect("assignment");
            if change == 0 {
                let mut refused = endpoint.clone();
                refused.port = 1;
                let started = std::time::Instant::now();
                let (peer, identity) = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    crate::sharing_client::PeerConnection::verified(
                        &receiver.sharing,
                        &[refused, endpoint.clone()],
                        &remote,
                    ),
                )
                .await
                .expect("dead DNS/refused hint cannot delay reachable approved hint")
                .expect("pinned reachable hint");
                assert_eq!(identity.server_id, remote.server_id);
                assert!(started.elapsed() < std::time::Duration::from_secs(1));
                drop(peer);
            }
            if change == 1 {
                use plurx_core::store::sharing_catalogue::{RemoteWatch, RemoteWatchUpdate};
                let item_ids: Vec<i64> = {
                    let writer =
                        rusqlite::Connection::open(&source.path).expect("Source identity reader");
                    let mut statement = writer
                        .prepare("SELECT id FROM items WHERE library_id=?1 ORDER BY id")
                        .expect("IDs");
                    let rows = statement
                        .query_map([source.library], |row| row.get(0))
                        .expect("current IDs")
                        .collect::<Result<Vec<_>, _>>()
                        .expect("closed IDs");
                    rows
                };
                assert_eq!(item_ids.len(), 200);
                for item in item_ids {
                    receiver
                        .store
                        .save_remote_watch(RemoteWatchUpdate {
                            import_id: import,
                            library_id: library.clone(),
                            item_id: source_id(&item.to_string()).expect("ID"),
                            user_id: user.id,
                            lifecycle_generation: 1,
                            assignment_generation: 2,
                            progress: RemoteWatch {
                                position_ms: 1000,
                                duration_ms: Some(60000),
                                watched: false,
                                sequence: 1,
                                updated_at_ms: 1001,
                            },
                        })
                        .await
                        .expect("B-owned Continue Watching history");
                }
            }
            let details = matches!(change, 3 | 4);
            if details || artwork {
                use plurx_core::sharing_catalogue_details::CatalogueRevisionKey;
                let envelope = CatalogueRevisionKey::generate_sealed(
                    &source.state.sharing.key,
                    remote.clone(),
                )
                .expect("revision fixture key");
                let writer =
                    rusqlite::Connection::open(&source.path).expect("details fixture writer");
                writer
                    .execute_batch(
                        plurx_core::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA,
                    )
                    .expect("candidate key table");
                writer
                    .execute(
                        "INSERT INTO sharing_catalogue_keys VALUES(1,?1,?2,?3)",
                        rusqlite::params![
                            remote.server_id.to_string(),
                            remote.catalogue_epoch.to_string(),
                            envelope.as_stored()
                        ],
                    )
                    .expect("fixture purpose key");
                let probe=json!({"chapters":(0..1024).map(|n|json!({"start_time":n.to_string(),"end_time":(n+1).to_string(),"tags":{"title":"x".repeat(512)}})).collect::<Vec<_>>()}).to_string();
                for file in 1..=if details { 3 } else { 0 } {
                    writer.execute("INSERT INTO files(id,item_id,path,size,mtime,probe_json) VALUES(?1,?2,?4,20,1000,?3)",rusqlite::params![file,source.item,probe,format!("/private/fixture-{file}.mkv")]).expect("bounded details file");
                }
            }
            let art_path = if artwork {
                use plurx_core::{
                    sharing_artwork::{ArtVariant, ReceiverArtReference},
                    sharing_file_locators::FileLocatorKey,
                };
                let envelope = FileLocatorKey::generate_sealed(&receiver.sharing.key, &local)
                    .expect("B fixture purpose material");
                let writer =
                    rusqlite::Connection::open(&receiver_path).expect("B key fixture writer");
                writer
                    .execute_batch(
                        plurx_core::store::sharing_file_locators::CANDIDATE_FILE_LOCATOR_KEY_SCHEMA,
                    )
                    .expect("candidate B key schema");
                writer
                    .execute(
                        "INSERT INTO sharing_file_locator_keys VALUES(1,?1,?2,?3)",
                        rusqlite::params![
                            local.server_id.to_string(),
                            local.catalogue_epoch.to_string(),
                            envelope.as_stored()
                        ],
                    )
                    .expect("B key selection");
                drop(writer);
                let key =
                    FileLocatorKey::open(&receiver.sharing.key, &local, &envelope).expect("B key");
                let (summary, reply) = receiver
                    .sharing
                    .read_catalogue(
                        &receiver,
                        import,
                        user.id,
                        crate::sharing::CatalogueRead::Item(
                            source_id(&source.item.to_string()).expect("item"),
                        ),
                    )
                    .await
                    .expect("fresh pinned Source art resources");
                let crate::sharing::CatalogueReply::Item(details) = reply else {
                    panic!("details");
                };
                let art = details
                    .item
                    .art
                    .into_iter()
                    .find(|art| art.variant == ArtVariant::Original)
                    .expect("fresh Source opaque original");
                Some(
                    key.issue_art(
                        &ReceiverArtReference {
                            item: SharedReference {
                                import_id: import,
                                server_id: remote.server_id,
                                catalogue_epoch: remote.catalogue_epoch,
                                library_id: library.clone(),
                                item_id: source_id(&source.item.to_string()).expect("item"),
                            },
                            user_id: user.id,
                            lifecycle_generation: summary.lifecycle_generation,
                            source: art.resource,
                        },
                        clock_ms(),
                    )
                    .expect("user/import bound B resource")
                    .url(),
                )
            } else {
                None
            };
            let app = super::super::router(receiver.clone()).route(
                "/ordinary/pending",
                get(|| async {
                    Response::new(Body::from_stream(futures_util::stream::pending::<
                        Result<Bytes, std::convert::Infallible>,
                    >()))
                }),
            );
            let body_dropped = Arc::new(AtomicBool::new(false));
            let data_dropped = Arc::new(AtomicBool::new(false));
            let (b_address, captured, stop, served) =
                body_server_app(app, body_dropped.clone(), data_dropped.clone()).await;
            let art_baseline = super::super::shared_artwork::receiver_capacity();
            let path = if let Some(path) = art_path {
                path
            } else if change == 1 {
                format!("/api/v1/shared/imports/{import}/continue-watching?limit=200")
            } else if details {
                format!("/api/v1/shared/imports/{import}/items/{}", source.item)
            } else {
                format!(
                    "/api/v1/shared/imports/{import}/libraries/{}/items?limit=200",
                    source.library
                )
            };
            let mut client = blocked_receiver_client(b_address, &path, token, h2).await;
            let connection = captured.await.expect("accepted B connection");
            await_flag(&body_dropped, true).await;
            assert!(
                !data_dropped.load(Ordering::SeqCst),
                "B DATA blocked h2={h2}/change={change}"
            );
            if artwork {
                assert_eq!(
                    super::super::shared_artwork::receiver_capacity(),
                    (art_baseline.0 - 1, art_baseline.1 - 8193),
                    "actual B artwork bytes held behind DATA"
                );
            }
            let ordinary = if let BlockedReceiverClient::Http2(sender, _, _) = &mut client {
                Some(
                    sender
                        .send_request(
                            Request::builder()
                                .uri("http://fixture/ordinary/pending")
                                .body(Body::empty())
                                .expect("ordinary multiplex request"),
                        )
                        .await
                        .expect("ordinary multiplex response")
                        .into_body(),
                )
            } else {
                None
            };
            if change == 0 {
                rusqlite::Connection::open(&source.path)
                    .expect("metadata writer")
                    .execute(
                        "UPDATE items SET title='Fresh changed metadata' WHERE id=?1",
                        [source.item],
                    )
                    .expect("benign metadata update");
                let (_, fresh) = receiver
                    .sharing
                    .read_catalogue(
                        &receiver,
                        import,
                        user.id,
                        crate::sharing::CatalogueRead::Page {
                            library: library.clone(),
                            parent: None,
                            q: String::new(),
                            cursor: None,
                            limit: 200,
                        },
                    )
                    .await
                    .expect("full fresh read despite populated cache");
                let crate::sharing::CatalogueReply::Page(fresh) = fresh else {
                    panic!("page reply");
                };
                assert_eq!(
                    fresh
                        .items
                        .iter()
                        .find(|item| item.item_id.as_str() == source.item.to_string())
                        .expect("current item")
                        .title,
                    "Fresh changed metadata",
                    "cache must replace changed fresh bytes"
                );
                // Both real credential rotation and assignment/endpoint additions
                // preserve captured effective tuples. No counter-only revocation.
                let active = receiver
                    .store
                    .sharing_import(import)
                    .await
                    .expect("load")
                    .expect("active");
                receiver
                    .sharing
                    .resume_rotation(&receiver, active, true)
                    .await
                    .expect("real pinned benign credential rotation");
                receiver
                    .store
                    .assign_share_viewers(
                        import,
                        2,
                        vec![
                            Assignment {
                                library_id: library.clone(),
                                user_id: user.id,
                            },
                            Assignment {
                                library_id: source_id(&source.private.to_string()).expect("ID"),
                                user_id: user.id,
                            },
                        ],
                        1003,
                    )
                    .await
                    .expect("benign assignment expansion");
                receiver
                    .store
                    .set_sharing_import_endpoints(import, 1, vec![endpoint], None, 1004)
                    .await
                    .expect("benign endpoint refresh");
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                assert!(
                    !connection.0.is_cancelled(),
                    "benign changes preserve accepted B response"
                );
                assert!(!data_dropped.load(Ordering::SeqCst));
            }
            match change {
                0 => source
                    .state
                    .store
                    .revoke_share(source.grant, 2000)
                    .await
                    .expect("Source grant revoke"),
                1 => {
                    let grant = source
                        .state
                        .store
                        .sharing_exports(None)
                        .await
                        .expect("exports")
                        .remove(0)
                        .grant;
                    source
                        .state
                        .store
                        .share_scope(
                            source.grant,
                            grant.mutation_generation,
                            vec![source.private],
                            2000,
                        )
                        .await
                        .expect("Source scope loss");
                }
                2 => {
                    rusqlite::Connection::open(&source.path)
                        .expect("Source writer")
                        .execute(
                            "UPDATE items SET library_id=?1 WHERE id=?2",
                            rusqlite::params![source.private, source.item],
                        )
                        .expect("Source item move");
                }
                3 => {
                    rusqlite::Connection::open(&source.path)
                        .expect("Source writer")
                        .execute("DELETE FROM files WHERE id=1", [])
                        .expect("Source file delete");
                }
                4 => {
                    rusqlite::Connection::open(&source.path)
                        .expect("Source writer")
                        .execute("UPDATE files SET item_id=?1 WHERE id=1", [source.item + 1])
                        .expect("Source file move");
                }
                5 => {
                    assert!(receiver
                        .store
                        .delete_token(&hash)
                        .await
                        .expect("B login revoke"));
                }
                6 => receiver
                    .store
                    .disable_share_import(import, 2000)
                    .await
                    .expect("B import loss"),
                7 => {
                    receiver
                        .store
                        .assign_share_viewers(import, 2, Vec::new(), 2000)
                        .await
                        .expect("B assignment loss");
                }
                9 => source
                    .state
                    .store
                    .revoke_share(source.grant, clock_ms())
                    .await
                    .expect("art grant loss"),
                10 => {
                    rusqlite::Connection::open(&source.path)
                        .expect("writer")
                        .execute("DELETE FROM items WHERE id=?1", [source.item])
                        .expect("art item delete");
                }
                11 => {
                    rusqlite::Connection::open(&source.path)
                        .expect("writer")
                        .execute(
                            "UPDATE items SET library_id=?1 WHERE id=?2",
                            rusqlite::params![source.private, source.item],
                        )
                        .expect("art item move");
                }
                12 => {
                    assert!(receiver
                        .store
                        .delete_token(&hash)
                        .await
                        .expect("art B login loss"));
                }
                13 => receiver
                    .store
                    .disable_share_import(import, clock_ms())
                    .await
                    .expect("art B import loss"),
                14 => {
                    receiver
                        .store
                        .assign_share_viewers(import, 2, Vec::new(), clock_ms())
                        .await
                        .expect("art B assignment loss");
                }
                15 => {
                    source
                        .state
                        .store
                        .put_setting(plurx_core::store::keys::SHARING_ENABLED, "false")
                        .await
                        .expect("art Source offline");
                }
                _ => {
                    assert!(
                        receiver.sharing.catalogue_cache_entries() > 0,
                        "fresh metadata populated cache"
                    );
                    source
                        .state
                        .store
                        .put_setting(plurx_core::store::keys::SHARING_ENABLED, "false")
                        .await
                        .expect("Source unavailable");
                    assert!(
                        receiver
                            .sharing
                            .read_catalogue(
                                &receiver,
                                import,
                                user.id,
                                crate::sharing::CatalogueRead::Page {
                                    library: library.clone(),
                                    parent: None,
                                    q: String::new(),
                                    cursor: None,
                                    limit: 200
                                }
                            )
                            .await
                            .is_err(),
                        "cached metadata cannot replace fresh Source authority"
                    );
                }
            }
            // A delivered B response is never re-observed; each change
            // reaches the next response's own authority point instead.
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            assert!(
                !connection.0.is_cancelled(),
                "no monitor re-authorizes a delivered B response"
            );
            drop(ordinary);
            release_receiver_client(client).await;
            await_flag(&data_dropped, true).await;
            assert_eq!(
                receiver.sharing.scope_control_available(),
                32,
                "B control permit cleanup"
            );
            assert_eq!(
                SCOPE_CONTROL.available_permits(),
                32,
                "Source control permit cleanup"
            );
            if artwork {
                tokio::time::timeout(std::time::Duration::from_secs(3), async {
                    while super::super::shared_artwork::receiver_capacity() != art_baseline {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("B art bytes return their lease once released");
            }
            stop.send(()).expect("stop B");
            assert_eq!(
                served.await.expect("B task").expect("B result"),
                crate::HttpDrain::Complete
            );
            source_stop.send(()).expect("stop Source");
            source_task
                .await
                .expect("Source task")
                .expect("Source result");
        }
    }
}

/// Actual pinned Source read and per-response authority for an administrator
/// whose import has no viewer assignments. This is an isolated CGNAT fixture.
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires disposable CGNAT network and PLURX_SHARING_FIXTURE_IP"]
async fn sharing_admin_pinned_library_read_bootstraps_empty_matrix_and_fences_connection() {
    use plurx_core::{
        config::{SharingEgressConfig, SharingNetworkConfig},
        secrets::SharingSecretPurpose,
        sharing_tls::{LiveNodeTls, SharingTlsListener},
        store::SqliteStore,
    };
    let _serial = BODY_FIXTURES.lock().await;
    let address: std::net::IpAddr = std::env::var("PLURX_SHARING_FIXTURE_IP")
        .expect("explicit CGNAT")
        .parse()
        .expect("address");
    assert!(is_tailnet_address(address));
    for h2 in [false, true] {
        for loss in 0..5 {
            let source = body_fixture().await;
            let tls_dir = tempfile::tempdir().expect("Source TLS fixture");
            let tls = Arc::new(
                LiveNodeTls::open(&tls_dir.path().join("tls"), clock_ms() / 1000)
                    .expect("Source TLS"),
            );
            let (pin, _) = tls.status().expect("Source SPKI");
            let listener = tokio::net::TcpListener::bind((address, 0))
                .await
                .expect("Source CGNAT listener");
            let endpoint = Endpoint {
                ipv4: match address {
                    std::net::IpAddr::V4(ip) => ip,
                    _ => panic!("IPv4 fixture"),
                },
                ipv6: None,
                ts_fqdn: "source.fixture.ts.net".into(),
                port: listener.local_addr().expect("Source address").port(),
                spki_sha256: pin,
            };
            let (source_stop, source_stopped) = tokio::sync::oneshot::channel();
            let source_task = tokio::spawn(crate::serve_http(
                SharingTlsListener::new(listener, tls),
                sharing::peer_router(source.state.clone()),
                async move {
                    let _ = source_stopped.await;
                },
                crate::HTTP_TIMEOUTS,
            ));
            let receiver_dir = tempfile::tempdir().expect("B fixture");
            let receiver_path = receiver_dir.path().join("receiver.sqlite");
            let (_, mut receiver) = super::super::tests::test_app_with_state();
            receiver.store = Arc::new(SqliteStore::open(&receiver_path).expect("B Store"));
            receiver.sharing = Arc::new(crate::sharing::SharingManager::new(
                receiver.sharing.key.clone(),
                receiver_dir.path().join("unused-tls"),
                SharingNetworkConfig {
                    bind: "127.0.0.1:32444".parse().expect("unused bind"),
                    egress: SharingEgressConfig::LocalAddress { address },
                    ..SharingNetworkConfig::default()
                },
            ));
            receiver
                .store
                .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
                .await
                .expect("enable B");
            let user = receiver
                .store
                .create_user("receiver-library-admin", "synthetic-hash", true)
                .await
                .expect("viewer");
            let token = "synthetic-receiver-current-login";
            let hash = plurx_core::auth::hash_token(token);
            receiver
                .store
                .create_token(&hash, user.id, None)
                .await
                .expect("login");
            let local = receiver
                .store
                .sharing_identity(1000)
                .await
                .expect("B identity");
            let remote = source
                .state
                .store
                .sharing_identity(1000)
                .await
                .expect("Source identity");
            rusqlite::Connection::open(&source.path)
                .expect("Source fixture writer")
                .execute(
                    "UPDATE sharing_exports SET recipient_server_id=?1 WHERE id=?2",
                    rusqlite::params![local.server_id.to_string(), source.grant.to_string()],
                )
                .expect("bind grant to actual B identity");
            let import = uuid::Uuid::new_v4();
            let credential = crate::sharing::ImportCredential::encode(
                &source.secret,
                uuid::Uuid::new_v4(),
                1000,
                "Fixture B",
                2000,
                None,
            )
            .expect("closed credential");
            assert_eq!(
                receiver
                    .store
                    .create_share_import(NewImport {
                        id: import,
                        source: remote.clone(),
                        source_name: "Fixture Source".into(),
                        claim_id: uuid::Uuid::new_v4(),
                        credential: receiver
                            .sharing
                            .key
                            .seal_sharing(
                                SharingSecretPurpose::Credential,
                                local.server_id,
                                import,
                                credential.expose()
                            )
                            .expect("seal credential"),
                        claim_secret: receiver
                            .sharing
                            .key
                            .seal_sharing(
                                SharingSecretPurpose::Claim,
                                local.server_id,
                                import,
                                "synthetic-claim"
                            )
                            .expect("seal claim"),
                        endpoints: vec![endpoint.clone()],
                        now_ms: 1000
                    })
                    .await
                    .expect("B import"),
                ImportOutcome::Created
            );
            receiver
                .store
                .settle_share_claim(import, source.grant, true, 1001)
                .await
                .expect("active B import");

            let snapshot = receiver
                .store
                .sharing_import_assignments(import)
                .await
                .expect("snapshot")
                .expect("import");
            assert!(snapshot.assignments.is_empty());
            let (summary, libraries) = receiver
                .sharing
                .read_admin_libraries(&receiver, import, user.id, &hash)
                .await
                .expect("actual fresh pinned admin read");
            assert_eq!(summary.id, import);
            assert_eq!(libraries.len(), 1);
            assert_eq!(libraries[0].library_id.as_str(), source.library.to_string());
            assert!(
                receiver
                    .sharing
                    .read_catalogue(
                        &receiver,
                        import,
                        user.id,
                        crate::sharing::CatalogueRead::Libraries
                    )
                    .await
                    .is_err(),
                "admin does not gain viewer authority"
            );
            let app = super::super::router(receiver.clone());
            let body_dropped = Arc::new(AtomicBool::new(false));
            let data_dropped = Arc::new(AtomicBool::new(false));
            let (b_address, captured, stop, served) =
                body_server_app(app, body_dropped.clone(), data_dropped.clone()).await;
            let path = format!("/api/v1/sharing/imports/{import}/libraries");
            let client = blocked_receiver_client(b_address, &path, token, h2).await;
            let connection = captured.await.expect("accepted B connection");
            await_flag(&body_dropped, true).await;
            match loss {
                0 => {
                    receiver
                        .store
                        .set_admin(user.id, false)
                        .await
                        .expect("demote current admin");
                }
                1 => {
                    assert!(receiver.store.delete_token(&hash).await.expect("logout"));
                }
                2 => receiver
                    .store
                    .disable_share_import(import, 1003)
                    .await
                    .expect("disconnect"),
                3 => source
                    .state
                    .store
                    .revoke_share(source.grant, 1003)
                    .await
                    .expect("Source revoke"),
                4 => receiver
                    .store
                    .put_setting(plurx_core::store::keys::SHARING_ENABLED, "false")
                    .await
                    .expect("sharing off"),
                _ => unreachable!(),
            }
            // The delivered admin read is not re-observed; the loss is
            // enforced by the next read's own authority point.
            let refused = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .expect("fresh B client")
                .get(format!("http://{b_address}{path}"))
                .header("authorization", format!("Bearer {token}"))
                .send()
                .await
                .expect("fresh admin read");
            assert!(!refused.status().is_success(), "loss {loss}");
            assert!(!connection.0.is_cancelled());
            release_receiver_client(client).await;
            stop.send(()).expect("stop B");
            assert_eq!(
                served.await.expect("B task").expect("B server"),
                crate::HttpDrain::Complete
            );
            source_stop.send(()).expect("stop Source");
            source_task
                .await
                .expect("Source task")
                .expect("Source server");
        }
    }
}

#[tokio::test]
async fn sharing_continue_http_keeps_offline_groups_separate_without_stale_titles() {
    use plurx_core::{
        secrets::SharingSecretPurpose,
        store::sharing_catalogue::{RemoteWatch, RemoteWatchUpdate},
    };
    let _serial = BODY_FIXTURES.lock().await;
    let (_, state) = super::super::tests::test_app_with_state();
    state
        .store
        .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
        .await
        .expect("enable B");
    let user = state
        .store
        .create_user("continue-current-viewer", "synthetic-hash", false)
        .await
        .expect("viewer");
    let token = "synthetic-continue-login";
    state
        .store
        .create_token(&plurx_core::auth::hash_token(token), user.id, None)
        .await
        .expect("login");
    let local = state
        .store
        .sharing_identity(1000)
        .await
        .expect("B identity");
    let mut imports = Vec::new();
    for n in 1..=2 {
        let import = uuid::Uuid::new_v4();
        let secret = new_secret().expect("synthetic credential");
        let credential = crate::sharing::ImportCredential::encode(
            &secret,
            uuid::Uuid::new_v4(),
            1000,
            "Fixture B",
            2000,
            None,
        )
        .expect("closed credential");
        state
            .store
            .create_share_import(NewImport {
                id: import,
                source: SharingIdentity {
                    server_id: uuid::Uuid::new_v4(),
                    catalogue_epoch: uuid::Uuid::new_v4(),
                    created_at_ms: 1000,
                },
                source_name: format!("B configured Source {n}"),
                claim_id: uuid::Uuid::new_v4(),
                credential: state
                    .sharing
                    .key
                    .seal_sharing(
                        SharingSecretPurpose::Credential,
                        local.server_id,
                        import,
                        credential.expose(),
                    )
                    .expect("seal"),
                claim_secret: state
                    .sharing
                    .key
                    .seal_sharing(
                        SharingSecretPurpose::Claim,
                        local.server_id,
                        import,
                        "synthetic-claim",
                    )
                    .expect("claim"),
                endpoints: vec![Endpoint {
                    ipv4: "100.101.102.103".parse().expect("fixture IP"),
                    ipv6: None,
                    ts_fqdn: "offline.fixture.ts.net".into(),
                    port: 1,
                    spki_sha256: "a".repeat(64),
                }],
                now_ms: 1000,
            })
            .await
            .expect("import");
        state
            .store
            .settle_share_claim(import, uuid::Uuid::new_v4(), true, 1001)
            .await
            .expect("active import");
        state
            .store
            .assign_share_viewers(
                import,
                1,
                vec![Assignment {
                    library_id: source_id("12").expect("ID"),
                    user_id: user.id,
                }],
                1002,
            )
            .await
            .expect("assignment");
        state
            .store
            .save_remote_watch(RemoteWatchUpdate {
                import_id: import,
                library_id: source_id("12").expect("ID"),
                item_id: source_id("9007199254740993").expect("ID"),
                user_id: user.id,
                lifecycle_generation: 1,
                assignment_generation: 2,
                progress: RemoteWatch {
                    position_ms: n * 1000,
                    duration_ms: Some(60000),
                    watched: false,
                    sequence: n,
                    updated_at_ms: 1000 + n,
                },
            })
            .await
            .expect("B history");
        imports.push(import);
    }
    let app = super::super::router(state.clone());
    let call = |path: String| {
        let app = app.clone();
        let connection = crate::SharingConnectionCancellation::new();
        async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("authorization", format!("Bearer {token}"))
                        .extension(connection.clone())
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            let status = response.status();
            let bytes = response
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes();
            let value: Value = serde_json::from_slice(&bytes).expect("JSON");
            drop(connection);
            (status, value)
        }
    };
    let start = std::time::Instant::now();
    let (status, index) = call("/api/v1/shared/continue-watching".into()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(index["groups"].as_array().expect("groups").len(), 2);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(1),
        "local group index never waits for Source network"
    );
    assert_eq!(index["groups"][0]["count"], 1);
    assert!(index["groups"][0].get("items").is_none());
    for import in &imports {
        let (status, group) =
            call(format!("/api/v1/shared/imports/{import}/continue-watching")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(group["availability"], "unavailable");
        assert_eq!(group["count"], 1);
        assert_eq!(group["items"], json!([]));
        assert!(group["source_name"]
            .as_str()
            .expect("B label")
            .starts_with("B configured"));
        assert!(!group.to_string().contains("synthetic-continue-login"));
    }
    let (status, _) = call("/api/v1/shared/continue-watching?limit=201".into()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call("/api/v1/shared/continue-watching?path=injected".into()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    state
        .store
        .assign_share_viewers(imports[0], 2, Vec::new(), 2000)
        .await
        .expect("remove assignment");
    let (status, index) = call("/api/v1/shared/continue-watching".into()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(index["groups"].as_array().expect("groups").len(), 1);
    state
        .store
        .delete_token(&plurx_core::auth::hash_token(token))
        .await
        .expect("logout");
    let (status, _) = call("/api/v1/shared/continue-watching".into()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sharing_current_scope_http_checks_recipient_tuples_and_control_capacity() {
    let _serial = BODY_FIXTURES.lock().await;
    let fixture = body_fixture().await;
    let identity = fixture
        .state
        .store
        .sharing_identity(1000)
        .await
        .expect("identity");
    let grant = fixture
        .state
        .store
        .sharing_grant_status(&secret_hash(SecretDomain::Grant, &fixture.secret))
        .await
        .expect("status")
        .expect("grant")
        .grant;
    let scope = plurx_core::sharing_catalogue_details::SourceScopeRequest {
        server_id: identity.server_id,
        catalogue_epoch: identity.catalogue_epoch,
        grant_id: fixture.grant,
        recipient_server_id: grant.recipient_server_id,
        libraries: vec![source_id(&fixture.library.to_string()).expect("ID")],
        items: vec![plurx_core::sharing_catalogue_details::SourceScopeItem {
            library_id: source_id(&fixture.library.to_string()).expect("ID"),
            item_id: source_id(&fixture.item.to_string()).expect("ID"),
        }],
        files: Vec::new(),
    };
    let router = sharing::peer_router(fixture.state.clone());
    let request = |body: Vec<u8>| {
        Request::builder()
            .method("POST")
            .uri("/sharing/v1/current-scope")
            .header(
                "authorization",
                format!("CinemaShare {}", fixture.secret.expose()),
            )
            .body(Body::from(body))
            .expect("request")
    };
    for changed in 0..5 {
        let mut scope = scope.clone();
        match changed {
            1 => scope.recipient_server_id = uuid::Uuid::new_v4(),
            2 => scope.server_id = uuid::Uuid::new_v4(),
            3 => scope.grant_id = uuid::Uuid::new_v4(),
            4 => scope.items[0].library_id = source_id(&fixture.private.to_string()).expect("ID"),
            _ => {}
        }
        let response = router
            .clone()
            .oneshot(request(serde_json::to_vec(&scope).expect("scope")))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).expect("closed response"),
            json!({"authorized":changed==0})
        );
    }
    let full = SCOPE_CONTROL
        .clone()
        .try_acquire_many_owned(32)
        .expect("control cap");
    assert_eq!(
        router
            .clone()
            .oneshot(request(serde_json::to_vec(&scope).expect("scope")))
            .await
            .expect("response")
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(full);
    assert_eq!(
        router
            .clone()
            .oneshot(request(vec![b' '; 32 * 1024 + 1]))
            .await
            .expect("response")
            .status(),
        StatusCode::BAD_REQUEST
    );
    fixture
        .state
        .store
        .revoke_share(fixture.grant, 2000)
        .await
        .expect("revoke");
    assert_eq!(
        router
            .oneshot(request(serde_json::to_vec(&scope).expect("scope")))
            .await
            .expect("response")
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(SCOPE_CONTROL.available_permits(), 32);
}

#[tokio::test]
async fn sharing_catalogue_http_requires_live_grant_and_qualified_order_maintenance() {
    let (_, state) = super::super::tests::test_app_with_state();
    state
        .store
        .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
        .await
        .expect("enable candidate routes");
    let router = sharing::peer_router(state.clone());
    let response = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/sharing/v1/libraries")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .expect("private response"),
        "no-store"
    );
    let library = state
        .store
        .create_library(&NewLibrary {
            name: "Exported movies".into(),
            kind: LibraryKind::Movies,
            paths: vec!["/source/private/path".into()],
            anime: false,
        })
        .await
        .expect("library")
        .id;
    state
        .store
        .sharing_identity(clock_ms())
        .await
        .expect("identity");
    let secret = new_secret().expect("grant secret");
    let invitation = uuid::Uuid::new_v4();
    let grant = uuid::Uuid::new_v4();
    let now = clock_ms();
    state
        .store
        .create_share_invitation(InvitationRecord {
            id: invitation,
            token_hash: "a".repeat(64),
            library_ids: vec![library],
            created_at_ms: now,
            expires_at_ms: now + 60000,
        })
        .await
        .expect("invitation");
    state
        .store
        .claim_share(ShareClaim {
            invitation_id: invitation,
            invitation_hash: "a".repeat(64),
            claim_id: uuid::Uuid::new_v4(),
            grant_id: grant,
            recipient_server_id: uuid::Uuid::new_v4(),
            recipient_name: "Receiver".into(),
            credential_hash: secret_hash(SecretDomain::Grant, &secret),
            now_ms: now,
        })
        .await
        .expect("claim");
    state
        .store
        .approve_share(grant, 1, now)
        .await
        .expect("approve");
    for (method, path, body) in [
        ("GET", "/sharing/v1/libraries".to_owned(), ""),
        ("GET", format!("/sharing/v1/libraries/{library}/items"), ""),
        ("GET", "/sharing/v1/items/1".to_owned(), ""),
        ("GET", "/sharing/v1/items/1/children".to_owned(), ""),
        (
            "POST",
            "/sharing/v1/items:batch".to_owned(),
            "{\"item_ids\":[\"1\"]}",
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method(method)
                    .uri(path)
                    .header("authorization", format!("CinemaShare {}", secret.expose()))
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "missing candidate must fail closed"
        );
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("bounded body")
            .to_bytes();
        let text = std::str::from_utf8(&bytes).expect("error JSON");
        assert!(!text.contains("Exported movies"));
        assert!(!text.contains("/source/private/path"));
        assert!(text.contains("sharing_authority_unavailable"));
    }
}
#[tokio::test]
async fn sharing_catalogue_page_failures_keep_typed_reopen_and_absence_results() {
    use crate::sharing_client::PeerError;
    async fn typed(error: ApiError) -> (StatusCode, String) {
        let response = error.into_response();
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("typed body")
            .to_bytes();
        let value: Value = serde_json::from_slice(&body).expect("typed JSON");
        (
            status,
            value["code"].as_str().unwrap_or_default().to_owned(),
        )
    }
    for (error, cursor, status, code) in [
        (
            PeerError::Rejected(StatusCode::GONE),
            true,
            StatusCode::GONE,
            "sharing_cursor_expired",
        ),
        (
            PeerError::Rejected(StatusCode::CONFLICT),
            true,
            StatusCode::CONFLICT,
            "sharing_query_changed",
        ),
        (
            PeerError::Rejected(StatusCode::BAD_REQUEST),
            true,
            StatusCode::BAD_REQUEST,
            "sharing_cursor_invalid",
        ),
        (
            PeerError::Rejected(StatusCode::BAD_REQUEST),
            false,
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_source_unavailable",
        ),
        (
            PeerError::Rejected(StatusCode::NOT_FOUND),
            true,
            StatusCode::NOT_FOUND,
            "sharing_not_found",
        ),
        (
            PeerError::Authentication,
            false,
            StatusCode::NOT_FOUND,
            "sharing_not_found",
        ),
        (
            PeerError::Rejected(StatusCode::TOO_MANY_REQUESTS),
            true,
            StatusCode::TOO_MANY_REQUESTS,
            "sharing_metadata_capacity",
        ),
        (
            PeerError::Unavailable,
            true,
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_source_unavailable",
        ),
        (
            PeerError::IdentityMismatch,
            true,
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_source_unavailable",
        ),
    ] {
        assert_eq!(
            typed(page_failure(error, cursor)).await,
            (status, code.to_owned())
        );
    }
}
#[test]
fn sharing_catalogue_http_rejects_unknown_duplicate_and_oversized_queries() {
    for raw in [
        "offset=1",
        "limit=201",
        "limit=0",
        "q=%",
        "q=%FF",
        "q=%C3%28",
        "limit=1&limit=2",
        "cursor=x&cursor=y",
        "q=x&q=y",
    ] {
        assert!(query(Some(raw)).is_err(), "{raw}");
    }
    assert!(query(Some(&format!("q={}", "a".repeat(513)))).is_err());
    assert!(query(Some(&format!("cursor={}", "a".repeat(4097)))).is_err());
    assert!(query(Some("q=film&limit=60")).is_ok());
}
#[tokio::test]
async fn sharing_source_preparation_uses_complete_live_file_and_real_principal_engine() {
    use super::super::{
        hls::{prepare_source_playback, CreateSession, SourcePlaybackTarget},
        stream,
    };
    use plurx_core::{
        sharing_catalogue_details::CatalogueRevisionKey,
        store::sharing_catalogue_details::SourceDetailsRead,
    };
    let _serial = BODY_FIXTURES.lock().await;
    let fixture = body_fixture().await;
    let identity = fixture
        .state
        .store
        .sharing_identity(1000)
        .await
        .expect("identity");
    let envelope =
        CatalogueRevisionKey::generate_sealed(&fixture.state.sharing.key, identity.clone())
            .expect("fixture key");
    let key = CatalogueRevisionKey::open(&fixture.state.sharing.key, identity.clone(), &envelope)
        .expect("open");
    let source = fixture._directory.path().join("prepared.mp4");
    std::fs::write(&source, b"actual source presence").expect("actual file");
    let writer = rusqlite::Connection::open(&fixture.path).expect("writer");
    writer
        .execute_batch(plurx_core::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA)
        .expect("fixture purpose schema");
    writer
        .execute(
            "INSERT INTO sharing_catalogue_keys VALUES(1,?1,?2,?3)",
            rusqlite::params![
                identity.server_id.to_string(),
                identity.catalogue_epoch.to_string(),
                envelope.as_stored()
            ],
        )
        .expect("fixture purpose key");
    writer.execute("INSERT INTO files(id,item_id,path,size,mtime,duration_ms,container,video_codec,width,height,bit_depth,bitrate) VALUES(0,?1,?2,20,1000,60000,'mp4','h264',1920,1080,8,1000000)",rusqlite::params![fixture.item,source.to_str().expect("path")]).expect("actual Source file");
    let hash = secret_hash(SecretDomain::Grant, &fixture.secret);
    let SourceDetailsRead::Authorized(witness) = fixture
        .state
        .store
        .source_item_file_witness(
            &hash,
            fixture.grant,
            SourceId::parse(&fixture.item.to_string()).expect("valid Source fixture value"),
            SourceId::parse("0").expect("valid Source fixture value"),
        )
        .await
        .expect("witness")
    else {
        panic!("authorized witness")
    };
    let target = SourcePlaybackTarget {
        server_id: identity.server_id,
        catalogue_epoch: identity.catalogue_epoch,
        library_id: SourceId::parse(&fixture.library.to_string())
            .expect("valid Source fixture value"),
        item_id: SourceId::parse(&fixture.item.to_string()).expect("valid Source fixture value"),
        file_id: SourceId::parse("0").expect("valid Source fixture value"),
        revision: key.file_revision(&witness).expect("revision"),
    };
    let caps:plurx_core::playback::DeviceCaps=serde_json::from_value(json!({"v":2,"video":[{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"]})).expect("caps");
    let body = || {
        serde_json::from_value::<CreateSession>(json!({"playback_id":"shared-player","request_id":"shared-request","copy":true,"height":1080,"quality_auto":false,"presentation":"vod","caps":caps})).expect("body")
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("CinemaShare {}", fixture.secret.expose())
            .parse()
            .expect("valid Source fixture value"),
    );
    headers.insert(
        "cinemashare-viewer",
        "a".repeat(64).parse().expect("valid Source fixture value"),
    );
    let prepared = prepare_source_playback(&fixture.state, &headers, target.clone(), body())
        .await
        .expect("actual Source preparation");
    // Response projection is pure presentation evidence; this StartInfo
    // fixture does not assert physical admission or publication.
    let session = uuid::Uuid::new_v4().to_string();
    let incarnation = uuid::Uuid::new_v4().to_string();
    let info = crate::transcode::StartInfo {
        processed_dv_profile: None,
        processed_fel: None,
        degraded_processing: None,
        processing_note: None,
        audio_delivery: prepared.request().audio_delivery.clone(),
        retained_output: None,
        session_id: session.clone(),
        playlist_url: format!("/api/v1/hls/{session}/index.m3u8"),
        duration_ms: Some(60_000),
        start_seconds: 0.0,
        media_origin_seconds: 0.0,
        target_height: 1080,
        kind: prepared.request().kind,
        encoder: "vod",
        grade: plurx_core::transcode::OutputGrade::Sdr,
        vod: true,
        control_lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
    };
    let response = prepared
        .start_response(&fixture.state, &info, &incarnation, 7)
        .await
        .expect("complete Source response");
    assert_eq!(response.session_id, session);
    assert_eq!(response.playlist_url, info.playlist_url);
    assert_eq!(response.duration_ms, info.duration_ms);
    assert_eq!(response.height, info.target_height);
    assert_eq!(response.encoder, info.encoder);
    assert_eq!(response.media_origin_ms, Some(0));
    assert!(response.vod);
    assert_eq!(response.prior_kbps, None);
    assert_eq!(response.plan_notes, prepared.plan_notes());
    let control = response.control.as_ref().expect("private control");
    assert_eq!(control.generation, incarnation);
    assert_eq!(control.control_epoch, 7);
    assert_eq!(control.url, format!("/api/v1/hls/{session}/control"));
    assert!(prepared
        .start_response(&fixture.state, &info, &incarnation, 0)
        .await
        .is_err());
    assert!(prepared
        .start_response(&fixture.state, &info, "not-an-incarnation", 7)
        .await
        .is_err());
    let decision_input = || super::super::shared_playback::SourceDecisionRequest {
        reference: target.clone(),
        caps: caps.clone(),
        audio: None,
        subtitle: None,
        audio_offset_ms: None,
        force: None,
    };
    let peer_response =
        super::super::shared_playback::source_decision(&fixture.state, &headers, decision_input())
            .await
            .expect("current peer decision");
    let peer_payload: Value = serde_json::from_slice(
        &to_bytes(peer_response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("bounded body"),
    )
    .expect("peer payload");
    assert_eq!(peer_payload["protocol"], 1);
    assert_eq!(
        peer_payload["reference"],
        serde_json::to_value(&target).expect("reference")
    );
    assert_eq!(
        peer_payload["decision"],
        serde_json::to_value(prepared.decision()).expect("complete engine")
    );
    let mut wrong = decision_input();
    wrong.reference.server_id = uuid::Uuid::new_v4();
    assert!(
        super::super::shared_playback::source_decision(&fixture.state, &headers, wrong)
            .await
            .is_err()
    );
    let mut empty = decision_input();
    empty.caps = plurx_core::playback::DeviceCaps::default();
    assert!(
        super::super::shared_playback::source_decision(&fixture.state, &headers, empty)
            .await
            .is_err()
    );
    assert_eq!(prepared.file().id, 0);
    assert!(crate::media_sessions::source_session_request_is_valid(
        prepared.request(),
        prepared.principal()
    ));
    assert!(!crate::media_sessions::worker_session_request_is_valid(
        prepared.request()
    ));
    let mut negative = prepared.request().clone();
    negative.file_id = -1;
    assert!(!crate::media_sessions::source_session_request_is_valid(
        &negative,
        prepared.principal()
    ));
    assert!(!crate::media_sessions::source_session_request_is_valid(
        prepared.request(),
        &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 1 }
    ));
    assert_eq!(prepared.file().path, source);
    assert_eq!(prepared.principal().local_user_id(), None);
    assert_eq!(prepared.decision().prior_kbps, None);
    assert_eq!(
        prepared.request().request_id.as_deref(),
        Some("shared-request")
    );
    assert!(matches!(
        prepared.request().kind,
        crate::transcode::SessionKind::Copy { .. }
    ));
    assert_ne!(
        prepared.fingerprint(),
        prepared.request().durable_intent_fingerprint(
            &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 1 }
        )
    );
    let mut other_headers = headers.clone();
    other_headers.insert(
        "cinemashare-viewer",
        "b".repeat(64).parse().expect("valid Source fixture value"),
    );
    let other = prepare_source_playback(&fixture.state, &other_headers, target.clone(), body())
        .await
        .expect("distinct Source viewer");
    assert_ne!(
        prepared.fingerprint(),
        other.fingerprint(),
        "equal numeric Source files and request IDs do not collapse distinct sharing viewers"
    );
    let prior = plurx_core::domain::NetworkPrior {
        sustained_kbps: Some(1),
        ..Default::default()
    };
    assert!(
        super::super::hls::resolve_plan_for_principal(
            super::super::hls::FilePlanInputs {
                snapshot: None,
                state: &fixture.state,
                file_id: 0,
                source: Some(prepared.file()),
                network_prior: Some(&prior)
            },
            prepared.principal(),
            None,
            body()
        )
        .await
        .is_err(),
        "Source planner refuses Local network-prior injection"
    );
    let user = fixture
        .state
        .store
        .create_user("local-engine-fixture", "synthetic-hash", false)
        .await
        .expect("real local account");
    let Json(local) = stream::decision(
        super::super::extract::AuthUser(user),
        State(fixture.state.clone()),
        axum::extract::Path(0),
        axum::extract::Query(stream::Caps {
            caps_v2: Some(caps.clone()),
            ..Default::default()
        }),
        HeaderMap::new(),
        super::super::network::RemoteAddress(None),
    )
    .await
    .expect("actual Local decision");
    assert_eq!(serde_json::to_value(prepared.decision()).expect("valid Source fixture value"),serde_json::to_value(local).expect("valid Source fixture value"),"full actual engine decision parity, including source facts, tracks, delivery, quality candidates and ladder");
    for changed in [
        SourcePlaybackTarget {
            server_id: uuid::Uuid::new_v4(),
            ..target.clone()
        },
        SourcePlaybackTarget {
            catalogue_epoch: uuid::Uuid::new_v4(),
            ..target.clone()
        },
        SourcePlaybackTarget {
            library_id: SourceId::parse(&fixture.private.to_string())
                .expect("valid Source fixture value"),
            ..target.clone()
        },
        SourcePlaybackTarget {
            revision: plurx_core::sharing_catalogue_details::FileRevision::parse(&"0".repeat(64))
                .expect("valid Source fixture value"),
            ..target.clone()
        },
    ] {
        assert!(
            prepare_source_playback(&fixture.state, &headers, changed, body())
                .await
                .is_err()
        );
    }
    writer
        .execute(
            "UPDATE files SET mtime=2000 WHERE id=?1",
            [prepared.file().id],
        )
        .expect("real file revision changed");
    assert!(
        prepare_source_playback(&fixture.state, &headers, target.clone(), body())
            .await
            .is_err()
    );
    writer
        .execute(
            "UPDATE files SET mtime=1000 WHERE id=?1",
            [prepared.file().id],
        )
        .expect("restore snapshot");
    writer
        .execute(
            "UPDATE items SET library_id=?1 WHERE id=?2",
            rusqlite::params![fixture.private, fixture.item],
        )
        .expect("move outside grant");
    assert!(
        prepare_source_playback(&fixture.state, &headers, target, body())
            .await
            .is_err()
    );
    let sessions: i64 = writer
        .query_row("SELECT count(*) FROM media_sessions", [], |row| row.get(0))
        .expect("actual session rows");
    assert_eq!(
        sessions, 0,
        "preparation allocates no physical or durable Source session"
    );
}
