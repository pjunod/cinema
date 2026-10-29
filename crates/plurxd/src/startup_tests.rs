use super::*;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use plurx_core::domain::Library;
use plurx_core::store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn media_runtime_record_uses_the_fragment_cache_engine_identity() {
    let record = media_runtime_identity().await;
    assert_eq!(
        record.engine_digest,
        crate::ffmpeg::fragment_index_engine_digest().await
    );
    assert_eq!(record.architecture, std::env::consts::ARCH);
    assert_eq!(record.engine_digest.len(), 64);
}

async fn timeout_test_server(
    timeouts: HttpTimeouts,
) -> (
    SocketAddr,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<anyhow::Result<HttpDrain>>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let app = axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let served = tokio::spawn(serve_http(
        listener,
        app,
        async move {
            let _ = stopped.await;
        },
        timeouts,
    ));
    (address, stop, served)
}

/// How long a timeout test waits for a response before calling it a
/// hang.
const TIMEOUT_TEST_HANG_GUARD: Duration = Duration::from_secs(20);

/// How long a header-timer test waits for the server to close the socket.
/// The server under test closes it on the 80 ms header timer from
/// [`test_http_timeouts`]. This bound must separate that timer from two
/// failures: no timer at all (nothing else closes an idle HTTP/1
/// connection), and `serve_http` ignoring the timeouts it was given and
/// applying the production [`HEADER_READ_TIMEOUT`] of 15 s. So it stays
/// well below the production value (asserted at compile time below), and
/// the tests also check the elapsed time against it. It is still far above
/// 80 ms: a sub-second bound failed on loaded runners with the timer
/// working, because a current-thread test runtime that is not scheduled
/// cannot read the EOF.
const HEADER_TIMER_CLOSE_BOUND: Duration = Duration::from_secs(5);
const _: () = assert!(
    HEADER_TIMER_CLOSE_BOUND.as_millis() * 3 <= HEADER_READ_TIMEOUT.as_millis(),
    "the close bound must stay far below the production header timer"
);

/// Wait for the server to close `stream` (EOF) within
/// [`HEADER_TIMER_CLOSE_BOUND`].
async fn expect_header_timer_close(stream: &mut tokio::net::TcpStream, context: &str) {
    let started = tokio::time::Instant::now();
    let mut byte = [0u8; 1];
    let read = tokio::time::timeout(HEADER_TIMER_CLOSE_BOUND, stream.read(&mut byte))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{context}: the configured header timer must close the socket \
                     within {HEADER_TIMER_CLOSE_BOUND:?} (production's is {HEADER_READ_TIMEOUT:?})"
            )
        })
        .expect("read");
    assert_eq!(read, 0, "{context}: the socket must end at EOF");
    let elapsed = started.elapsed();
    assert!(
        elapsed < HEADER_TIMER_CLOSE_BOUND,
        "{context}: closed after {elapsed:?}, not by the configured header timer"
    );
}

fn test_http_timeouts() -> HttpTimeouts {
    HttpTimeouts {
        header_read: Duration::from_millis(80),
        h2_keepalive_interval: Duration::from_millis(30),
        h2_keepalive_timeout: Duration::from_millis(200),
        accept_error_backoff: Duration::from_millis(20),
        shutdown_drain: Duration::from_millis(200),
    }
}

/// Real pending listener boundary and in-place transfer, not proof that
/// a learner has completed clock observation or voter promotion.
#[tokio::test]
async fn k06_clock_only_socket_denies_application_and_transfers_existing_keepalive() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    async fn request(stream: &mut BufReader<tokio::net::TcpStream>, path: &str) -> String {
        stream
            .get_mut()
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .await
            .expect("request");
        let mut status = String::new();
        stream.read_line(&mut status).await.expect("status");
        let mut bytes = 0;
        loop {
            let mut line = String::new();
            stream.read_line(&mut line).await.expect("header");
            assert!(!line.is_empty(), "response must not close before headers");
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                bytes = value.trim().parse::<usize>().expect("length");
            }
        }
        let mut body = vec![0; bytes];
        stream.read_exact(&mut body).await.expect("body");
        status
    }
    let reservation = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve");
    let address = reservation.local_addr().expect("address");
    let owner = Arc::new(StartupObservationHttp::from_listener(reservation).expect("owned socket"));
    assert!(
        tokio::net::TcpListener::bind(address).await.is_err(),
        "the same listener stays reserved before startup consumes it"
    );
    owner
        .start(
            plurx_core::cluster::membership::MembershipManager::unavailable(),
            "pending-node".into(),
        )
        .await
        .expect("pending socket");
    assert!(
        tokio::net::TcpListener::bind(address).await.is_err(),
        "pending phase owns the one bound socket"
    );
    let mut stream = BufReader::new(
        tokio::net::TcpStream::connect(address)
            .await
            .expect("connect"),
    );
    assert!(
        request(&mut stream, "/_internal/v1/clock")
            .await
            .contains("401"),
        "unsigned observation refuses"
    );
    assert!(
        request(&mut stream, "/active").await.contains("503"),
        "no application route while pending, and it says the server is starting"
    );
    let (shutdown, stopped) = tokio::sync::oneshot::channel();
    let normal_owner = Arc::clone(&owner);
    let normal = tokio::spawn(async move {
        normal_owner
            .serve_normal(
                axum::Router::new().route("/active", axum::routing::get(|| async { "active" })),
                async move {
                    let _ = stopped.await;
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if request(&mut stream, "/active").await.contains("200") {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("same keepalive observes transferred router");
    assert!(
        tokio::net::TcpListener::bind(address).await.is_err(),
        "activation did not release/rebind the socket"
    );
    shutdown.send(()).expect("shutdown");
    drop(stream);
    tokio::time::timeout(Duration::from_secs(2), normal)
        .await
        .expect("drain budget")
        .expect("task")
        .expect("normal drain");
    assert!(
        owner.tasks.lock().await.is_none(),
        "server/probe ownership consumed exactly once"
    );
    assert!(
        tokio::net::TcpListener::bind(address).await.is_ok(),
        "drained listener releases the port"
    );
}

/// The 2026-10-10 report: a page opened during startup was blank and
/// `/readyz` answered 404. While pending, every non-clock route says the
/// server is starting — typed JSON for clients, a self-reloading page for
/// browsers — and never that the route does not exist.
#[tokio::test]
async fn pending_socket_answers_starting_not_not_found() {
    use axum::http::{header, Request, StatusCode};
    let reservation = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve");
    let address = reservation.local_addr().expect("address");
    let owner = Arc::new(StartupObservationHttp::from_listener(reservation).expect("owned socket"));
    assert!(
        tokio::net::TcpListener::bind(address).await.is_err(),
        "the same listener stays reserved before startup consumes it"
    );
    owner
        .start(
            plurx_core::cluster::membership::MembershipManager::unavailable(),
            "pending-node".into(),
        )
        .await
        .expect("pending socket");
    let app = owner.router.read().expect("router").clone();
    for path in ["/api/v1/server", "/healthz", "/web/index.html"] {
        let response = tower::ServiceExt::oneshot(
            app.clone(),
            Request::get(path)
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("infallible");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert_eq!(response.headers()[header::RETRY_AFTER], "2", "{path}");
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "no-store",
            "{path}"
        );
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("typed JSON");
        assert_eq!(json["code"], "server_starting", "{path}");
    }
    let ready = tower::ServiceExt::oneshot(
        app.clone(),
        Request::get("/readyz")
            .body(axum::body::Body::empty())
            .expect("request"),
    )
    .await
    .expect("infallible");
    assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(ready.headers()[header::RETRY_AFTER], "2");
    let body = axum::body::to_bytes(ready.into_body(), 1024)
        .await
        .expect("body");
    assert_eq!(&body[..], b"starting\n", "readyz keeps its one-word reason");
    let page = tower::ServiceExt::oneshot(
        app,
        Request::get("/")
            .header(header::ACCEPT, "text/html,application/xhtml+xml,*/*;q=0.8")
            .body(axum::body::Body::empty())
            .expect("request"),
    )
    .await
    .expect("infallible");
    assert_eq!(page.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(page.headers()[header::RETRY_AFTER], "2");
    assert_eq!(
        page.headers()[header::CACHE_CONTROL],
        "no-store",
        "the starting page must never be cached as the app shell"
    );
    assert!(page.headers()[header::CONTENT_TYPE]
        .to_str()
        .expect("type")
        .starts_with("text/html"));
    let body = axum::body::to_bytes(page.into_body(), 64 * 1024)
        .await
        .expect("body");
    let html = std::str::from_utf8(&body).expect("utf-8");
    assert!(
        html.contains("http-equiv=\"refresh\""),
        "the page reloads itself"
    );
    assert!(html.contains("The server is starting"));
    // The clock route keeps its own answer; k06 above pins it over a
    // real socket, where the connection info it needs exists.
    owner.stop_and_drain().await;
}

/// Real learner/catchup -> authenticated observation -> committed vote.
/// This is not a physical restart/qualification receipt.
#[cfg(feature = "cluster-integration-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn k06_actual_voter_join_observes_committed_learner_before_promotion() {
    use axum::{extract::State, response::IntoResponse, Json};
    use plurx_core::cluster::membership::{
        FinalizeJoinRequest, MembershipManager, RedeemJoinRequest,
    };
    use plurx_core::cluster::migration::select_daemon_store_observing;
    fn config(root: &std::path::Path) -> Config {
        let listeners: Vec<_> = (0..3)
            .map(|_| std::net::TcpListener::bind("127.0.0.1:0").expect("port"))
            .collect();
        let addresses: Vec<_> = listeners
            .iter()
            .map(|listener| listener.local_addr().expect("address"))
            .collect();
        let mut config = Config::default();
        config.storage.data_dir = root.into();
        config.server.bind = addresses[0];
        config.cluster.raft_bind = addresses[1];
        config.cluster.api_bind = addresses[2];
        config.cluster.advertise_host = "localhost".into();
        config.cluster.join_url = format!("http://{}", config.server.bind);
        config.cluster.artwork_url = config.cluster.join_url.clone();
        config
    }
    async fn redeem(
        State(manager): State<MembershipManager>,
        Json(request): Json<RedeemJoinRequest>,
    ) -> axum::response::Response {
        match manager.redeem(&request).await {
            Ok(()) => axum::http::StatusCode::NO_CONTENT.into_response(),
            Err(error) => (
                axum::http::StatusCode::CONFLICT,
                Json(serde_json::json!({"code":error.code(),"message":error.to_string()})),
            )
                .into_response(),
        }
    }
    async fn finalize(
        State(manager): State<MembershipManager>,
        Json(request): Json<FinalizeJoinRequest>,
    ) -> axum::response::Response {
        match manager.finalize(&request).await {
            Ok(()) => axum::http::StatusCode::NO_CONTENT.into_response(),
            Err(error) => (
                axum::http::StatusCode::CONFLICT,
                Json(serde_json::json!({"code":error.code(),"message":error.to_string()})),
            )
                .into_response(),
        }
    }
    struct AuditObserver {
        owner: Arc<StartupObservationHttp>,
        source: MembershipManager,
        raft_id: u64,
        observed: Arc<AtomicBool>,
        origin: String,
    }
    impl StartupClockObserver for AuditObserver {
        fn start(
            &self,
            membership: MembershipManager,
            node_id: String,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<(), plurx_core::error::StoreError>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async move {
                let roster = membership
                    .clock_peers()
                    .await
                    .map_err(|error| plurx_core::error::StoreError::Database(error.to_string()))?
                    .membership
                    .expect("actual applied startup roster");
                assert!(roster.members.contains(&self.raft_id));
                assert!(
                    !roster.voters.contains(&self.raft_id),
                    "desired voter must still be a committed learner at observation entry"
                );
                self.owner.start(membership, node_id.clone()).await?;
                let response = crate::http::peer_transport::PeerTransport::new(self.source.clone())
                    .clock_request(&node_id, &self.origin)
                    .await
                    .map_err(|error| {
                        plurx_core::error::StoreError::Database(format!("{error:?}"))
                    })?;
                assert_eq!(
                    response.status,
                    reqwest::StatusCode::OK,
                    "pending learner serves authenticated clock observation before promotion"
                );
                self.observed.store(true, Ordering::SeqCst);
                Ok(())
            })
        }
    }
    let source_root = crate::test_tempdir().expect("source");
    let source_config = config(source_root.path());
    drop(
        plurx_core::store::SqliteStore::open(&source_root.path().join("plurx.db"))
            .expect("SQLite source"),
    );
    let source_owner = Arc::new(StartupObservationHttp::new(source_config.server.bind));
    let source = select_daemon_store_observing(&source_config, Some(source_owner.as_ref()))
        .await
        .expect("source activation");
    let manager = source.membership_manager();
    let clock = axum::Router::new()
        .route(
            http::internal_clock::PATH,
            axum::routing::get(http::internal_clock::observation_snapshot),
        )
        .with_state(http::internal_clock::ObservationContext {
            membership: manager.clone(),
            node_id: source.identity.node_id.clone(),
            observer: source_owner.observer().await.expect("source clock owner"),
        });
    let app = axum::Router::new()
        .route("/api/v1/cluster/join/redeem", axum::routing::post(redeem))
        .route(
            "/api/v1/cluster/join/finalize",
            axum::routing::post(finalize),
        )
        .with_state(manager.clone())
        .merge(clock);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let source_http = Arc::clone(&source_owner);
    let server = tokio::spawn(async move {
        source_http
            .serve_normal(app, async move {
                let _ = stopped.await;
            })
            .await
    });
    let original = manager
        .clock_guard()
        .acquire_owned_for(plurx_core::cluster::clock::ClockDecision::MembershipChange)
        .expect("actual singleton clock proof");
    let token = manager
        .issue_token(Duration::from_secs(120))
        .await
        .expect("voter intent");
    let joining_root = crate::test_tempdir().expect("joiner");
    let mut joining_config = config(joining_root.path());
    let token_path = joining_root.path().join("join.token");
    std::fs::write(&token_path, format!("{}\n", token.token)).expect("test token");
    joining_config.cluster.join_token_file = token_path.clone();
    let joining_owner = Arc::new(StartupObservationHttp::new(joining_config.server.bind));
    let observed = Arc::new(AtomicBool::new(false));
    let observer = AuditObserver {
        owner: Arc::clone(&joining_owner),
        source: manager.clone(),
        raft_id: token.raft_id,
        observed: Arc::clone(&observed),
        origin: joining_config.cluster.artwork_url.clone(),
    };
    let joined = tokio::time::timeout(
        Duration::from_secs(90),
        select_daemon_store_observing(&joining_config, Some(&observer)),
    )
    .await
    .expect("bounded actual join")
    .expect("joined voter");
    assert!(observed.load(Ordering::SeqCst));
    let applied = joined
        .local_client()
        .expect("local client")
        .local_db_raft_metrics()
        .expect("watch")
        .membership_snapshot();
    assert!(applied.committed && applied.voters.contains(&token.raft_id));
    assert!(
        original.revalidate().is_err(),
        "actual member/promotion changes invalidate the original singleton proof"
    );
    assert!(
        !token_path.exists(),
        "finalization follows actual applied vote"
    );
    assert!(
        joining_root.path().join("hiqlite/activation.json").exists(),
        "activation must be published after vote"
    );
    joining_owner.stop_and_drain().await;
    let joined_shutdown = joined.shutdown().await;
    stop.send(()).expect("source stop");
    server.await.expect("HTTP task").expect("HTTP drain");
    source.shutdown().await.expect("source Raft drain");
    joined_shutdown.expect("joined Raft drain");
}

struct FailFirstAccept {
    listener: tokio::net::TcpListener,
    failed: Arc<AtomicBool>,
}

impl HttpAcceptor for FailFirstAccept {
    type Stream = tokio::net::TcpStream;

    async fn accept(&self) -> std::io::Result<(Self::Stream, SocketAddr)> {
        if !self.failed.swap(true, Ordering::SeqCst) {
            return Err(std::io::Error::other("simulated descriptor pressure"));
        }
        self.listener.accept().await
    }
}

/// The production acceptor hands `serve_http` sockets with Nagle's
/// algorithm off. Revert `disable_nagle` in the `TcpListener` acceptor and
/// the accepted stream reports `nodelay() == false`, which is the default
/// the kernel gives every accepted socket.
#[tokio::test]
async fn accepted_http_connections_have_nagle_disabled() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");

    let (client, accepted) = tokio::join!(
        tokio::net::TcpStream::connect(address),
        HttpAcceptor::accept(&listener),
    );
    let _client = client.expect("connect");
    let (accepted, _remote) = accepted.expect("accept through the production acceptor");

    assert!(
        accepted.nodelay().expect("read TCP_NODELAY"),
        "every accepted HTTP connection must have TCP_NODELAY set"
    );
}

#[tokio::test]
async fn accept_error_backs_off_then_serves_the_next_connection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let failed = Arc::new(AtomicBool::new(false));
    let acceptor = FailFirstAccept {
        listener,
        failed: Arc::clone(&failed),
    };
    let app = axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let started = tokio::time::Instant::now();
    let served = tokio::spawn(serve_http(
        acceptor,
        app,
        async move {
            let _ = stopped.await;
        },
        test_http_timeouts(),
    ));

    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect after the injected accept failure");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("request");
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), stream.read_to_end(&mut response))
        .await
        .expect("response timeout")
        .expect("response");
    assert!(response.windows(2).any(|window| window == b"ok"));
    assert!(
        started.elapsed() >= test_http_timeouts().accept_error_backoff,
        "the injected resource-pressure error must not spin"
    );
    assert!(failed.load(Ordering::SeqCst), "the failure was injected");
    stop_timeout_test_server(stop, served).await;
}

async fn stop_timeout_test_server(
    stop: tokio::sync::oneshot::Sender<()>,
    served: tokio::task::JoinHandle<anyhow::Result<HttpDrain>>,
) {
    stop.send(()).expect("stop server");
    tokio::time::timeout(Duration::from_secs(1), served)
        .await
        .expect("server stops")
        .expect("server task")
        .expect("clean server shutdown");
}

#[tokio::test]
async fn header_read_timeout_closes_a_silent_connection() {
    let (address, stop, served) = timeout_test_server(test_http_timeouts()).await;
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n")
        .await
        .expect("partial request head");

    expect_header_timer_close(&mut stream, "a partial request head").await;
    stop_timeout_test_server(stop, served).await;
}

#[tokio::test]
async fn keep_alive_idle_is_bounded_by_the_header_timer() {
    let (address, stop, served) = timeout_test_server(test_http_timeouts()).await;
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n")
        .await
        .expect("request");

    let mut response = Vec::new();
    while !response.ends_with(b"ok") {
        let mut chunk = [0u8; 256];
        let read = tokio::time::timeout(TIMEOUT_TEST_HANG_GUARD, stream.read(&mut chunk))
            .await
            .expect("response arrives")
            .expect("read response");
        assert!(read > 0, "connection closed before its response");
        response.extend_from_slice(&chunk[..read]);
    }

    expect_header_timer_close(&mut stream, "an idle keep-alive connection").await;
    stop_timeout_test_server(stop, served).await;
}

#[tokio::test]
async fn sharing_h2_graceful_predecessor_drain_preserves_existing_successor_writer() {
    use http_body_util::BodyExt;
    async fn expect_empty_terminal_eof(body: &mut hyper::body::Incoming, label: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(frame) = body.frame().await {
                let frame = frame.expect("graceful terminal frame must not be a transport error");
                let data = frame
                    .into_data()
                    .expect("fixture emits no terminal trailers");
                eprintln!("{label} terminal DATA length={}", data.len());
                assert!(
                    data.is_empty(),
                    "{label} unexpected queued terminal DATA: {data:?}"
                );
            }
        })
        .await
        .expect("bounded terminal stream EOF");
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let (first_tx, first_rx) = tokio::sync::mpsc::channel::<axum::body::Bytes>(2);
    let (second_tx, second_rx) = tokio::sync::mpsc::channel::<axum::body::Bytes>(2);
    let (third_tx, third_rx) = tokio::sync::mpsc::channel::<axum::body::Bytes>(2);
    drop(third_tx);
    let streams = Arc::new(std::sync::Mutex::new([
        Some(first_rx),
        Some(second_rx),
        Some(third_rx),
    ]));
    let (accepted_tx, mut accepted_rx) = tokio::sync::mpsc::channel(2);
    let app = axum::Router::new().route(
        "/{index}",
        axum::routing::get({
            move |axum::extract::Path(index): axum::extract::Path<usize>,
                      axum::Extension(connection): axum::Extension<
                    SharingConnectionCancellation,
                >| {
                    let receiver = streams.lock().expect("streams")[index]
                        .take()
                        .expect("stream");
                    let accepted = accepted_tx.clone();
                    async move {
                        accepted.send(connection).await.expect("observer");
                        let stream =
                            futures_util::stream::unfold(receiver, |mut receiver| async move {
                                receiver
                                    .recv()
                                    .await
                                    .map(|bytes| (Ok::<_, std::io::Error>(bytes), receiver))
                            });
                        axum::body::Body::from_stream(stream)
                    }
                }
        }),
    );
    let (stop, stopped) = tokio::sync::oneshot::channel();
    // Test server owns accepted drivers; explicit stop and join close it.
    let served = tokio::spawn(serve_http(
        listener,
        app,
        async move {
            let _ = stopped.await;
        },
        HTTP_TIMEOUTS,
    ));
    let socket = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect");
    let (mut client, driver) = hyper::client::conn::http2::handshake::<_, _, axum::body::Body>(
        TokioExecutor::new(),
        TokioIo::new(socket),
    )
    .await
    .expect("h2 handshake");
    // Client driver is joined after GOAWAY and actual accepted closure.
    let driven = tokio::spawn(driver);
    let mut predecessor = client
        .send_request(
            Request::builder()
                .uri(format!("http://{address}/0"))
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("predecessor headers")
        .into_body();
    let connection = accepted_rx.recv().await.expect("accepted predecessor");
    let custody = crate::sharing_connection_custody::AcceptedDriverRegistry::default();
    let captured = custody
        .capture(&connection, "owner")
        .expect("physical capture");
    assert_eq!(captured.id().boot_id, custody.boot_id());
    let principal_incarnation = uuid::Uuid::new_v4();
    let owner_identity = "a".repeat(64);
    let mut registration = custody
        .registration_guard()
        .await
        .expect("registration gate");
    let obligation = captured
        .prepare_obligation(
            &mut registration,
            "receiver",
            principal_incarnation,
            &owner_identity,
        )
        .expect("independently retained registration obligation");
    // Model the lost durable-registration answer while retaining exact
    // node-local custody. Actual closure below comes only from Hyper.
    drop(registration);

    let mut successor = client
        .send_request(
            Request::builder()
                .uri(format!("http://{address}/1"))
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("successor headers")
        .into_body();
    let same = accepted_rx.recv().await.expect("accepted successor");
    assert!(std::sync::Weak::ptr_eq(
        &connection.ownership_key(),
        &same.ownership_key()
    ));
    assert_eq!(
        custody
            .capture(&same, "owner")
            .expect("same actual driver")
            .id(),
        captured.id()
    );
    connection.drain_token().cancel();
    second_tx
        .send(axum::body::Bytes::from_static(b"successor survives"))
        .await
        .expect("write");
    let frame = tokio::time::timeout(Duration::from_secs(5), successor.frame())
        .await
        .expect("successor read deadline")
        .expect("frame")
        .expect("successor DATA");
    assert_eq!(frame.into_data().expect("data"), "successor survives");
    assert!(
        !connection.0.is_cancelled(),
        "graceful drain never hard-cuts either stream"
    );
    assert!(
        !connection.closed().is_closed(),
        "queued predecessor ownership is unsettled"
    );
    drop(first_tx);
    // Hyper may expose its legal empty END_STREAM DATA frame before EOF.
    // Inspect every frame and reject errors/bytes; stream EOF still does
    // not prove the accepted driver closed while the successor owns it.
    expect_empty_terminal_eof(&mut predecessor, "predecessor").await;
    assert!(
        !connection.closed().is_closed(),
        "successor still owns its accepted writer"
    );
    drop(second_tx);
    expect_empty_terminal_eof(&mut successor, "successor").await;
    drop(client);
    tokio::time::timeout(Duration::from_secs(5), connection.closed().wait())
        .await
        .expect("actual accepted writer closure");
    let receipt = obligation.joined().await;
    assert!(receipt.matches(captured.id()));
    assert!(!receipt.confirmation().is_empty());
    let close = crate::sharing_connection_custody::DriverCloseRequest {
        driver: captured.id().clone(),
        registration_sequence: obligation.registration_sequence(),
        principal_kind: "receiver".into(),
        incarnation_id: principal_incarnation,
        owner_identity,
        expected_owner_epoch: 1,
        mode: crate::sharing_connection_custody::DriverCloseMode::Drain,
        deadline_unix_ms: crate::media_sessions::unix_ms() + 5_000,
    };
    assert!(custody.close("foreign-owner", &close).await.is_err());
    assert!(custody
        .close("owner", &close)
        .await
        .expect("exact lost-close replay")
        .matches(captured.id()));
    obligation
        .release_after_ack(&receipt)
        .expect("only actual closure allows durable ack release");
    let recovered = custody
        .existing_obligation(
            captured.id(),
            "receiver",
            principal_incarnation,
            &close.owner_identity,
        )
        .expect("exact reservation survives actual closure and ack");
    custody
        .reconcile_guard(&recovered)
        .await
        .expect("mutable ack does not change pending identity")
        .complete();
    let mut next_socket = tokio::net::TcpStream::connect(address)
        .await
        .expect("fresh actual transport");
    next_socket
        .write_all(b"GET /2 HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n")
        .await
        .expect("new writer request");
    let next_connection = accepted_rx.recv().await.expect("fresh accepted driver");
    let next_driver = custody
        .capture(&next_connection, "owner")
        .expect("fresh physical capture");
    let mut next_permit = custody
        .registration_guard()
        .await
        .expect("registration gate");
    let next = next_driver
        .prepare_obligation(
            &mut next_permit,
            "receiver",
            principal_incarnation,
            &close.owner_identity,
        )
        .expect("new driver admitted after exact unknown reconciliation");
    assert!(next.registration_sequence() > recovered.registration_sequence());
    next_permit.complete();
    drop(next_socket);
    tokio::time::timeout(Duration::from_secs(5), next_connection.closed().wait())
        .await
        .expect("fresh accepted driver actually joined");
    let restarted = crate::sharing_connection_custody::AcceptedDriverRegistry::default();
    assert!(
        restarted.close("owner", &close).await.is_err(),
        "restart cannot manufacture closure"
    );
    driven
        .await
        .expect("driver joined")
        .expect("graceful h2 result");
    stop.send(()).expect("stop");
    served.await.expect("server joined").expect("server result");
}

#[tokio::test]
async fn h2_keepalive_is_configured() {
    let (address, stop, served) = timeout_test_server(test_http_timeouts()).await;
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect");
    // HTTP/2 prior knowledge: the client preface followed by an empty
    // SETTINGS frame. Read raw frames because a high-level h2 client
    // answers keepalive PINGs internally and would hide the evidence.
    stream
        .write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\x00\x00\x00\x04\x00\x00\x00\x00\x00")
        .await
        .expect("h2 preface");

    let saw_ping = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let mut header = [0u8; 9];
            stream.read_exact(&mut header).await.expect("frame header");
            let length =
                ((header[0] as usize) << 16) | ((header[1] as usize) << 8) | header[2] as usize;
            let mut payload = vec![0u8; length];
            stream
                .read_exact(&mut payload)
                .await
                .expect("frame payload");
            if header[3] == 0x6 {
                assert_eq!(length, 8, "PING payload length");
                assert_eq!(&header[5..9], &[0, 0, 0, 0], "PING is stream zero");
                break true;
            }
        }
    })
    .await
    .expect("server must emit a PING inside the keepalive interval");
    assert!(saw_ping);
    stop_timeout_test_server(stop, served).await;
}

/// The operator-facing cluster refusal is a sentence, not a paragraph with
/// the indentation baked in.
///
/// It shipped reading `... active on              another cluster node`:
/// two continuation lines inside the literal had lost their trailing `\`,
/// so fourteen columns of Rust source indentation became fourteen spaces
/// in the message. Nothing asserted on it, so `cargo fmt` kept it aligned
/// and every test stayed green. A run of two spaces is the whole tell.
#[test]
fn the_cluster_refusals_read_as_sentences() {
    assert!(
        !ARTWORK_PASS_UNAVAILABLE.contains("  "),
        "source indentation leaked into an operator message: \
             {ARTWORK_PASS_UNAVAILABLE:?}"
    );
    // And it is still the whole sentence, so the assertion above cannot be
    // satisfied by shortening the message instead of fixing it.
    assert!(ARTWORK_PASS_UNAVAILABLE.starts_with("the artwork provider pass is not available"));
    assert!(ARTWORK_PASS_UNAVAILABLE.ends_with("never runs cluster-wide provider work"));
    assert!(ARTWORK_PASS_UNAVAILABLE.contains("admitted as a learner"));
}

fn config_in(dir: &std::path::Path) -> Config {
    let mut config = Config::default();
    config.storage.data_dir = dir.to_path_buf();
    config.server.bind = "127.0.0.1:0".parse().expect("bind");
    config
}

fn store_in(dir: &std::path::Path) -> Arc<dyn Store> {
    Arc::new(SqliteStore::open(&dir.join("plurx.db")).expect("store"))
}

/// The scratch directory is cleared at boot because a half-written segment
/// from a killed process is worse than no segment. The two caches are its
/// siblings precisely so that clearing does not reach them: a cache that
/// empties on restart is a warm-up cost with none of the benefit.
#[test]
fn boot_clears_the_session_scratch_and_nothing_else() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let data = tmp.path();

    let first = create_dirs(data).expect("first boot");
    std::fs::write(first.transcode.join("stale-session.m4s"), b"half").expect("write");
    std::fs::write(first.cache.join("finished.mp4"), b"kept").expect("write");
    std::fs::write(first.subs.join("extracted.srt"), b"kept").expect("write");
    std::fs::write(first.artwork.join("poster.jpg"), b"kept").expect("write");
    std::fs::write(first.runtime_cache.join("ffmpeg.capabilities"), b"kept").expect("write");
    std::fs::write(first.renditions.join("admitted.json"), b"kept").expect("write");

    let second = create_dirs(data).expect("second boot");
    assert_eq!(second.transcode, first.transcode);
    assert!(
        !second.transcode.join("stale-session.m4s").exists(),
        "a stale session must not survive a restart"
    );
    assert!(second.transcode.is_dir(), "and the directory is recreated");
    for kept in [
        second.cache.join("finished.mp4"),
        second.subs.join("extracted.srt"),
        second.artwork.join("poster.jpg"),
        second.runtime_cache.join("ffmpeg.capabilities"),
        second.renditions.join("admitted.json"),
    ] {
        assert!(kept.exists(), "{} must survive a restart", kept.display());
    }
    // The caches are siblings of the scratch, not children of it: nested,
    // the boot clear above would take them with it.
    assert!(!second.cache.starts_with(&second.transcode));
    assert!(!second.subs.starts_with(&second.transcode));
}

#[test]
fn split_storage_roots_preserve_cache_and_clear_only_scratch() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let storage = StorageConfig {
        data_dir: tmp.path().join("durable"),
        cache_dir: tmp.path().join("persistent"),
        transcode_dir: tmp.path().join("scratch"),
        ..Default::default()
    };
    std::fs::create_dir_all(&storage.data_dir).expect("durable root");
    let first = create_dirs_for_storage(&storage).expect("first split boot");
    std::fs::write(storage.data_dir.join("authority.marker"), b"durable").expect("authority");
    std::fs::write(first.cache.join("finished.mp4"), b"kept").expect("cache");
    let offline_generation = first.cache.join("offline/ready/media");
    std::fs::create_dir_all(&offline_generation).expect("offline generation");
    std::fs::write(offline_generation.join("segment-00001.m4s"), b"kept").expect("offline segment");
    std::fs::write(first.subs.join("extracted.srt"), b"kept").expect("subs");
    std::fs::write(first.artwork.join("poster.jpg"), b"kept").expect("artwork");
    std::fs::write(first.runtime_cache.join("ffmpeg.capabilities"), b"kept")
        .expect("runtime cache");
    std::fs::write(first.renditions.join("admitted.json"), b"kept").expect("VOD rendition");
    std::fs::write(first.transcode.join("partial.m4s"), b"discard").expect("scratch");

    let second = create_dirs_for_storage(&storage).expect("second split boot");
    let cache_root = std::fs::canonicalize(&storage.cache_dir).expect("cache identity");
    let scratch_root = std::fs::canonicalize(&storage.transcode_dir).expect("scratch identity");
    assert_eq!(second.artwork, cache_root.join("artwork"));
    assert_eq!(second.cache, cache_root.join("transcode"));
    assert_eq!(second.subs, cache_root.join("subs"));
    assert_eq!(second.runtime_cache, cache_root.join("runtime"));
    assert_eq!(second.renditions, cache_root.join("renditions"));
    assert_eq!(second.transcode, scratch_root);
    assert!(storage.data_dir.join("authority.marker").exists());
    assert!(second.cache.join("finished.mp4").exists());
    assert!(
        second
            .cache
            .join("offline/ready/media/segment-00001.m4s")
            .exists(),
        "nested offline-generation bytes must survive restart"
    );
    assert!(second.subs.join("extracted.srt").exists());
    assert!(second.artwork.join("poster.jpg").exists());
    assert!(second.runtime_cache.join("ffmpeg.capabilities").exists());
    assert!(second.renditions.join("admitted.json").exists());
    assert!(!second.transcode.join("partial.m4s").exists());
    assert!(
        second.transcode.join(TRANSCODE_SCRATCH_MARKER).exists(),
        "the exact scratch ownership marker must survive cleanup"
    );
}

#[test]
fn scratch_must_not_contain_authoritative_or_persistent_roots() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let mut config = Config::default();
    config.storage.data_dir = tmp.path().join("scratch").join("durable");
    config.storage.cache_dir = tmp.path().join("scratch").join("persistent");
    config.storage.transcode_dir = tmp.path().join("scratch");
    let error = format!(
        "{:#}",
        canonicalize_storage_roots(&mut config).expect_err("scratch ancestor must fail")
    );
    assert!(
        error.contains("contains authoritative data root"),
        "{error}"
    );
}

#[test]
fn shared_cache_root_is_never_scratch_or_a_local_cache_alias() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let scratch = tmp.path().join("scratch");
    let shared = scratch.join("shared-cache");
    std::fs::create_dir_all(shared.join("objects")).expect("shared cache root");
    let shared_object = shared.join("objects/immutable-segment");
    std::fs::write(&shared_object, b"must survive").expect("shared cache object");

    let mut nested = Config::default();
    nested.storage.data_dir = tmp.path().join("nested-durable");
    nested.storage.cache_dir = tmp.path().join("nested-local-cache");
    nested.storage.transcode_dir = scratch;
    nested.cluster.shared_cache_dir = shared;
    nested.cluster.shared_cache_id = "shared-a".to_string();
    let error = format!(
        "{:#}",
        canonicalize_storage_roots(&mut nested)
            .expect_err("scratch containing shared cache must fail")
    );
    assert!(
        error.contains("verified shared cache root") && error.contains("scratch"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(&shared_object).expect("shared object survives"),
        b"must survive"
    );
    assert!(!nested
        .storage
        .transcode_dir
        .join(TRANSCODE_SCRATCH_MARKER)
        .exists());

    let local_cache = tmp.path().join("aliased-local-cache");
    std::fs::create_dir_all(&local_cache).expect("local cache root");
    let mut aliased = Config::default();
    aliased.storage.data_dir = tmp.path().join("aliased-durable");
    aliased.storage.cache_dir = local_cache.clone();
    aliased.storage.transcode_dir = tmp.path().join("aliased-scratch");
    aliased.cluster.shared_cache_dir = local_cache;
    aliased.cluster.shared_cache_id = "shared-a".to_string();
    let error = format!(
        "{:#}",
        canonicalize_storage_roots(&mut aliased)
            .expect_err("shared and local cache roots must be disjoint")
    );
    assert!(
        error.contains("aliases another authoritative or persistent storage root"),
        "{error}"
    );
}

#[test]
fn missing_shared_cache_mount_preserves_node_local_fallback() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let missing_shared = tmp.path().join("offline-shared-mount");
    let mut config = Config::default();
    config.storage.data_dir = tmp.path().join("durable");
    config.storage.cache_dir = tmp.path().join("local-cache");
    config.storage.transcode_dir = tmp.path().join("scratch");
    config.cluster.shared_cache_dir = missing_shared.clone();
    config.cluster.shared_cache_id = "shared-a".to_string();

    canonicalize_storage_roots(&mut config)
        .expect("an unavailable shared cache retains node-local fallback");
    assert_eq!(config.cluster.shared_cache_dir, missing_shared);
    assert!(
        !config.cluster.shared_cache_dir.exists(),
        "P5 preflight must not create an unavailable shared mount"
    );
}

#[test]
fn populated_unowned_explicit_scratch_is_refused_and_untouched() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let scratch = tmp.path().join("broad-existing-directory");
    std::fs::create_dir_all(&scratch).expect("scratch root");
    let unrelated = scratch.join("unrelated-host-data");
    std::fs::write(&unrelated, b"must survive").expect("unrelated data");

    let mut config = Config::default();
    config.storage.data_dir = tmp.path().join("durable");
    config.storage.cache_dir = tmp.path().join("persistent");
    config.storage.transcode_dir = scratch;
    let error = format!(
        "{:#}",
        create_dirs_for_storage(&config.storage).expect_err("unowned scratch must fail")
    );
    assert!(
        error.contains("populated before ownership could be claimed"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(&unrelated).expect("unrelated data remains"),
        b"must survive"
    );
}

#[test]
fn prelock_storage_validation_never_clears_owned_live_scratch() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let storage = StorageConfig {
        data_dir: tmp.path().join("durable"),
        cache_dir: tmp.path().join("persistent"),
        transcode_dir: tmp.path().join("scratch"),
        ..Default::default()
    };
    let first = create_dirs_for_storage(&storage).expect("first daemon owns scratch");
    let live = first.transcode.join("live-session/segment-00001.m4s");
    std::fs::create_dir_all(live.parent().expect("live parent")).expect("live session");
    std::fs::write(&live, b"currently-serving").expect("live segment");

    let mut second = Config {
        storage,
        ..Default::default()
    };
    canonicalize_storage_roots(&mut second).expect("second daemon prelock validation");
    assert_eq!(
        std::fs::read(&live).expect("live segment remains"),
        b"currently-serving",
        "prelock startup work must not clear the active daemon's scratch"
    );
}

#[test]
fn scratch_must_not_be_nested_inside_a_managed_cache() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let mut config = Config::default();
    config.storage.data_dir = tmp.path().join("durable");
    config.storage.cache_dir = tmp.path().join("persistent");
    config.storage.transcode_dir = config
        .storage
        .cache_dir
        .join("transcode")
        .join("live-sessions");
    let error = format!(
        "{:#}",
        canonicalize_storage_roots(&mut config).expect_err("nested scratch must fail")
    );
    assert!(error.contains("is inside persistent cache path"), "{error}");
}

#[test]
fn authoritative_data_must_not_be_a_managed_cache_child() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let mut config = Config::default();
    config.storage.cache_dir = tmp.path().join("persistent");
    config.storage.data_dir = config.storage.cache_dir.join("transcode");
    let error = format!(
        "{:#}",
        canonicalize_storage_roots(&mut config)
            .expect_err("authority/cache identity collision must fail")
    );
    assert!(
        error.contains("authoritative data root") && error.contains("managed persistent cache"),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn metadata_refresh_resolves_the_daemon_storage_topology() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let cache = tmp.path().join("persistent");
    let artwork_target = tmp.path().join("artwork-on-another-disk");
    std::fs::create_dir_all(&cache).expect("cache root");
    std::fs::create_dir_all(&artwork_target).expect("artwork target");
    std::os::unix::fs::symlink(&artwork_target, cache.join("artwork"))
        .expect("supported artwork relocation");

    let mut config = Config::default();
    config.storage.data_dir = tmp.path().join("durable");
    config.storage.cache_dir = cache;
    config.storage.transcode_dir = tmp.path().join("scratch");
    let refresh_dirs = canonicalize_storage_roots(&mut config).expect("refresh topology preflight");
    let (daemon_dirs, _) =
        prepare_storage_dirs(&config.storage, false, &[], &[]).expect("daemon topology preflight");

    assert_eq!(refresh_dirs.artwork, daemon_dirs.artwork);
    assert_eq!(refresh_dirs.cache, daemon_dirs.cache);
    assert_eq!(refresh_dirs.subs, daemon_dirs.subs);
    assert_eq!(refresh_dirs.transcode, daemon_dirs.transcode);
    assert_eq!(
        refresh_dirs.artwork,
        std::fs::canonicalize(&artwork_target).expect("canonical artwork target")
    );
}

#[cfg(unix)]
#[test]
fn a_legacy_transcode_cache_symlink_does_not_relocate_its_siblings() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let data = tmp.path().join("durable");
    let cache_root = data.join("cache");
    let transcode_target = tmp.path().join("transcodes-on-another-disk");
    std::fs::create_dir_all(&cache_root).expect("legacy cache root");
    std::fs::create_dir_all(&transcode_target).expect("transcode target");
    std::os::unix::fs::symlink(&transcode_target, cache_root.join("transcode"))
        .expect("legacy transcode relocation");

    let dirs = create_dirs(&data).expect("legacy layout boot");
    assert_eq!(
        dirs.cache,
        std::fs::canonicalize(&transcode_target).expect("canonical transcode target")
    );
    for (actual, expected) in [
        (&dirs.subs, cache_root.join("subs")),
        (&dirs.runtime_cache, cache_root.join("runtime")),
        (&dirs.renditions, cache_root.join("renditions")),
    ] {
        assert_eq!(
            actual,
            &std::fs::canonicalize(&expected).expect("canonical legacy sibling"),
            "resolving the transcode leaf must not move {}",
            expected.display()
        );
    }
}

#[test]
fn credential_key_inside_scratch_is_refused_without_deleting_it() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir_all(&scratch).expect("scratch root");
    let key = scratch.join("credentials.key");
    std::fs::write(&key, b"authoritative-key-material").expect("credential key");
    let mut config = Config::default();
    config.storage.data_dir = tmp.path().join("durable");
    config.storage.cache_dir = tmp.path().join("persistent");
    config.storage.transcode_dir = scratch;
    config.cluster.credential_key_file = key.clone();

    let error = format!(
        "{:#}",
        canonicalize_storage_roots(&mut config).expect_err("credential key in scratch must fail")
    );
    assert!(
        error.contains("credential key") && error.contains("transcode scratch"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(&key).expect("credential key survives"),
        b"authoritative-key-material"
    );
}

#[cfg(unix)]
#[test]
fn selected_credential_key_inode_is_protected_during_startup_scratch_cleanup() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let data = tmp.path().join("durable");
    let cache = tmp.path().join("persistent");
    let scratch = tmp.path().join("scratch");
    let storage = StorageConfig {
        data_dir: data.clone(),
        cache_dir: cache,
        transcode_dir: scratch,
        ..Default::default()
    };
    let initial = create_dirs_for_storage(&storage).expect("claim explicit scratch");
    let key = data.join("credentials.key");
    std::fs::write(&key, b"authoritative-key-material").expect("credential key");
    let alias = initial.transcode.join("credential-key-alias");
    std::fs::hard_link(&key, &alias).expect("same-inode scratch alias");

    let mut config = Config {
        storage,
        ..Default::default()
    };
    config.cluster.credential_key_file = key.clone();
    let error = format!(
        "{:#}",
        create_dirs_for_config(&config)
            .expect_err("the selected key identity must reach scratch cleanup")
    );
    assert!(error.contains("protected storage identity"), "{error}");
    assert_eq!(
        std::fs::read(&key).expect("credential key survives"),
        b"authoritative-key-material"
    );
    assert_eq!(
        std::fs::read(&alias).expect("scratch alias survives"),
        b"authoritative-key-material"
    );
}

#[cfg(unix)]
#[test]
fn filesystem_identity_alias_is_refused_even_without_ancestry() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let target = tmp.path().join("one-mounted-directory");
    let scratch_name = tmp.path().join("scratch-name");
    let cache_name = tmp.path().join("cache-name");
    std::fs::create_dir(&target).expect("target directory");
    std::os::unix::fs::symlink(&target, &scratch_name).expect("scratch alias");
    std::os::unix::fs::symlink(&target, &cache_name).expect("cache alias");
    let scratch = std::fs::canonicalize(&scratch_name).expect("scratch identity path");
    let cache = std::fs::canonicalize(&cache_name).expect("cache identity path");

    let error = format!(
        "{:#}",
        ensure_distinct_storage_identities(&scratch, &[("persistent cache", &cache)], false,)
            .expect_err("one inode under two names must fail")
    );
    assert!(error.contains("aliases persistent cache"), "{error}");
}

#[cfg(unix)]
#[test]
fn persistent_symlink_alias_to_scratch_is_refused_without_cleanup() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let cache = tmp.path().join("persistent");
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir_all(&cache).expect("cache root");
    std::fs::create_dir_all(&scratch).expect("scratch root");
    let persistent = scratch.join("offline-package.bin");
    std::fs::write(&persistent, b"must survive").expect("persistent bytes");
    std::os::unix::fs::symlink(&scratch, cache.join("transcode")).expect("cache/scratch alias");

    let mut config = Config::default();
    config.storage.data_dir = tmp.path().join("durable");
    config.storage.cache_dir = cache;
    config.storage.transcode_dir = scratch;
    let error = format!(
        "{:#}",
        canonicalize_storage_roots(&mut config).expect_err("alias must fail")
    );
    assert!(error.contains("contains persistent cache path"), "{error}");
    assert_eq!(
        std::fs::read(&persistent).expect("persistent alias target survives"),
        b"must survive"
    );
    assert!(!config
        .storage
        .transcode_dir
        .join(TRANSCODE_SCRATCH_MARKER)
        .exists());
}

#[cfg(unix)]
#[test]
fn vod_cache_children_cannot_alias_scratch_without_being_validated() {
    for child in ["runtime", "renditions"] {
        let tmp = crate::test_tempdir().expect("tempdir");
        let cache = tmp.path().join("persistent");
        let scratch = tmp.path().join("scratch");
        std::fs::create_dir_all(&cache).expect("cache root");
        std::fs::create_dir_all(&scratch).expect("scratch root");
        let bytes = scratch.join("must-survive.bin");
        std::fs::write(&bytes, b"must survive").expect("target bytes");
        std::os::unix::fs::symlink(&scratch, cache.join(child)).expect("cache/scratch alias");

        let mut config = Config::default();
        config.storage.data_dir = tmp.path().join("durable");
        config.storage.cache_dir = cache;
        config.storage.transcode_dir = scratch;
        let error = format!(
            "{:#}",
            canonicalize_storage_roots(&mut config)
                .expect_err("every derived cache child must be checked")
        );
        assert!(
            error.contains("transcode scratch") && error.contains("persistent cache path"),
            "{child}: {error}"
        );
        assert_eq!(
            std::fs::read(&bytes).expect("aliased bytes survive"),
            b"must survive"
        );
        assert!(!config
            .storage
            .transcode_dir
            .join(TRANSCODE_SCRATCH_MARKER)
            .exists());
    }
}

#[cfg(unix)]
#[test]
fn legacy_scratch_symlink_is_refused_without_touching_its_target() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let data = tmp.path().join("durable");
    let external = tmp.path().join("external-host-tree");
    std::fs::create_dir_all(&data).expect("data root");
    std::fs::create_dir_all(&external).expect("external target");
    let unrelated = external.join("unrelated-host-data");
    std::fs::write(&unrelated, b"must survive").expect("unrelated data");
    std::os::unix::fs::symlink(&external, data.join("transcode")).expect("legacy scratch symlink");

    let error = format!(
        "{:#}",
        create_dirs(&data).expect_err("legacy scratch symlink must fail")
    );
    // Assert on the guard's own words, not on the surrounding context
    // string: "legacy transcode scratch" is in the `with_context` of every
    // path through here, so it stays green with the guard deleted.
    assert!(error.contains("is a symlink"), "{error}");
    assert!(
        error.contains("configure an explicit empty scratch root"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(&unrelated).expect("external target survives"),
        b"must survive"
    );
}

/// A failed scratch clear must fail startup and leave the bytes it could
/// not remove observable.
///
/// The injection is deliberately not a permission change: `chmod 0500` is
/// ignored by uid 0, so as root the clear succeeded, this test failed, and
/// the bytes it claims to preserve were in fact deleted. The failure is
/// injected through plurx-core's test-only scratch failpoint instead, which
/// behaves identically for root and for an unprivileged uid.
#[cfg(unix)]
#[test]
fn incomplete_scratch_cleanup_fails_startup_and_preserves_the_error_target() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let storage = StorageConfig {
        data_dir: tmp.path().join("durable"),
        cache_dir: tmp.path().join("persistent"),
        transcode_dir: tmp.path().join("scratch"),
        ..Default::default()
    };
    let first = create_dirs_for_storage(&storage).expect("claim scratch");
    let stale = first.transcode.join("stale-session.m4s");
    std::fs::write(&stale, b"partial").expect("stale segment");

    plurx_core::fs_secure::scratch_fault::arm_cleanup_failure();
    let result = create_dirs_for_storage(&storage);
    plurx_core::fs_secure::scratch_fault::disarm();

    let error = format!(
        "{:#}",
        result.expect_err("partial cleanup must fail startup")
    );
    assert!(
        error.contains("securely clearing transcode scratch"),
        "{error}"
    );
    assert!(
        stale.exists(),
        "the failed cleanup target must remain observable"
    );

    // And with nothing injected the same boot clears it, so the assertion
    // above is about the failure and not about a clear that never runs.
    let second = create_dirs_for_storage(&storage).expect("uninjected boot clears scratch");
    assert!(!second.transcode.join("stale-session.m4s").exists());
}

/// The same property for the legacy `<data_dir>/transcode` root, which
/// carries no ownership marker and takes a different cleanup call.
#[cfg(unix)]
#[test]
fn incomplete_legacy_scratch_cleanup_fails_startup_and_preserves_its_target() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let data = tmp.path().join("durable");
    let first = create_dirs(&data).expect("first boot");
    let stale = first.transcode.join("stale-session.m4s");
    std::fs::write(&stale, b"partial").expect("stale segment");

    plurx_core::fs_secure::scratch_fault::arm_cleanup_failure();
    let result = create_dirs(&data);
    plurx_core::fs_secure::scratch_fault::disarm();

    let error = format!(
        "{:#}",
        result.expect_err("a failed legacy clear must fail startup")
    );
    assert!(
        error.contains("securely clearing legacy transcode scratch"),
        "{error}"
    );
    assert!(
        stale.exists(),
        "the failed cleanup target must remain observable"
    );
}

#[test]
fn cache_or_scratch_device_failure_leaves_authoritative_bytes_untouched() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let data = tmp.path().join("durable");
    std::fs::create_dir_all(&data).expect("durable root");
    let marker = data.join("authority.marker");
    std::fs::write(&marker, b"do-not-move").expect("authority marker");

    let blocked_cache = tmp.path().join("blocked-cache");
    std::fs::write(&blocked_cache, b"not a directory").expect("blocked cache");
    let cache_error = create_dirs_for_storage(&StorageConfig {
        data_dir: data.clone(),
        cache_dir: blocked_cache,
        ..Default::default()
    })
    .expect_err("cache device failure");
    assert!(cache_error.to_string().contains("persistent cache"));
    assert_eq!(
        std::fs::read(&marker).expect("authority survives cache failure"),
        b"do-not-move"
    );

    let blocked_scratch = tmp.path().join("blocked-scratch");
    std::fs::write(&blocked_scratch, b"not a directory").expect("blocked scratch");
    let scratch_error = create_dirs_for_storage(&StorageConfig {
        data_dir: data,
        transcode_dir: blocked_scratch,
        ..Default::default()
    })
    .expect_err("scratch device failure");
    assert!(scratch_error.to_string().contains("transcode"));
    assert_eq!(
        std::fs::read(&marker).expect("authority survives scratch failure"),
        b"do-not-move"
    );
}

/// The ENOTDIR test above simulates a dead device with a regular file
/// where a directory belongs, which is a different error class from a full
/// one. This is the real thing: a filesystem that returns `ENOSPC` for
/// every new inode, which is what a full cache or scratch device does.
///
/// Mounting a tmpfs needs privilege, so an unprivileged run says so and
/// skips rather than passing silently.
#[cfg(target_os = "linux")]
#[test]
fn a_full_cache_or_scratch_device_never_relocates_authoritative_bytes() {
    if std::env::var_os("PLURX_RUN_STORAGE_PRESSURE_TEST").is_none() {
        return;
    }
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "the opt-in ENOSPC storage-pressure test must run as root"
    );
    struct Mounted(PathBuf);
    impl Drop for Mounted {
        fn drop(&mut self) {
            let _ = std::process::Command::new("umount").arg(&self.0).status();
        }
    }

    let tmp = crate::test_tempdir().expect("tempdir");
    let full = tmp.path().join("full-device");
    std::fs::create_dir(&full).expect("mount point");
    let status = std::process::Command::new("mount")
        .args(["-t", "tmpfs", "-o", "size=64k,nr_inodes=8", "tmpfs"])
        .arg(&full)
        .status()
        .expect("run mount");
    assert!(
        status.success(),
        "opt-in ENOSPC tmpfs setup failed: {status}"
    );
    let _mounted = Mounted(full.clone());
    // Exhaust the filesystem so every later create returns ENOSPC,
    // whatever this kernel charges a directory.
    let mut exhausted = false;
    for index in 0..4_096 {
        if let Err(error) = std::fs::create_dir(full.join(format!("filler-{index}"))) {
            assert_eq!(
                error.raw_os_error(),
                Some(libc::ENOSPC),
                "the filler must stop on a full device, not on {error}"
            );
            exhausted = true;
            break;
        }
    }
    assert!(exhausted, "the tmpfs never filled up");

    let data = tmp.path().join("durable");
    std::fs::create_dir_all(&data).expect("durable root");
    let marker = data.join("authority.marker");
    std::fs::write(&marker, b"do-not-move").expect("authority marker");

    let cache_error = format!(
        "{:#}",
        create_dirs_for_storage(&StorageConfig {
            data_dir: data.clone(),
            cache_dir: full.join("cache"),
            ..Default::default()
        })
        .expect_err("a full cache device must fail startup")
    );
    assert!(cache_error.contains("persistent cache"), "{cache_error}");
    assert!(
        cache_error.contains("No space left on device"),
        "the operator must see why: {cache_error}"
    );

    let scratch_error = format!(
        "{:#}",
        create_dirs_for_storage(&StorageConfig {
            data_dir: data.clone(),
            transcode_dir: full.join("scratch"),
            ..Default::default()
        })
        .expect_err("a full scratch device must fail startup")
    );
    assert!(scratch_error.contains("transcode"), "{scratch_error}");
    assert!(
        scratch_error.contains("No space left on device"),
        "{scratch_error}"
    );

    assert_eq!(
        std::fs::read(&marker).expect("authority survives a full device"),
        b"do-not-move"
    );
    assert!(
        !data.join("plurx.db").exists(),
        "a failed cache or scratch layout must not create a database"
    );
    for entry in std::fs::read_dir(&full).expect("read the full device") {
        let entry = entry.expect("full device entry");
        assert!(
            entry.file_name().to_string_lossy().starts_with("filler-"),
            "nothing may be placed on the full device: {:?}",
            entry.path()
        );
    }
}

/// `storage.cache_dir` switches the node-local caches wholesale and
/// migrates nothing. Completed offline packages live under the old root,
/// so silence there is bytes the operator cannot find and cannot reclaim.
#[test]
fn setting_cache_dir_warns_about_the_legacy_cache_it_strands() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let data = tmp.path().join("durable");
    let legacy = create_dirs(&data).expect("legacy boot");
    std::fs::write(legacy.artwork.join("poster.jpg"), b"kept").expect("legacy artwork");
    let offline = legacy.cache.join("offline/ready/media");
    std::fs::create_dir_all(&offline).expect("offline generation");
    std::fs::write(offline.join("segment-00001.m4s"), b"kept").expect("offline segment");
    std::fs::write(
        legacy.runtime_cache.join("ffmpeg.capabilities"),
        b"regenerable",
    )
    .expect("legacy runtime cache");
    let rendition = legacy.renditions.join("title-1/manifest.json");
    std::fs::create_dir_all(rendition.parent().expect("rendition parent"))
        .expect("legacy rendition");
    std::fs::write(&rendition, b"admitted").expect("legacy rendition manifest");

    let storage = StorageConfig {
        data_dir: data.clone(),
        cache_dir: tmp.path().join("persistent"),
        ..Default::default()
    };
    let logs = captured(|| {
        create_dirs_for_storage(&storage).expect("split boot");
    });

    let warned = logs.tail("warn", 16);
    assert_eq!(warned.len(), 1, "{warned:?}");
    let message = format!("{warned:?}");
    assert!(message.contains("still holds entries"), "{message}");
    assert!(message.contains("offline"), "{message}");
    assert!(message.contains("admitted VOD renditions"), "{message}");
    assert!(message.contains("ffmpeg runtime cache"), "{message}");
    assert!(message.contains("delete or regenerate"), "{message}");
    assert!(
        message.contains(&data.display().to_string()),
        "both roots must be named: {message}"
    );
    assert!(
        message.contains(&tmp.path().join("persistent").display().to_string()),
        "both roots must be named: {message}"
    );
    // The legacy bytes are reported, never removed.
    assert!(legacy.artwork.join("poster.jpg").exists());
    assert!(offline.join("segment-00001.m4s").exists());
    assert!(legacy.runtime_cache.join("ffmpeg.capabilities").exists());
    assert!(rendition.exists());
}

/// The warning is about strandedness, not about `cache_dir` being set: an
/// empty legacy cache has nothing to report.
#[test]
fn an_empty_legacy_cache_produces_no_warning() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let storage = StorageConfig {
        data_dir: tmp.path().join("durable"),
        cache_dir: tmp.path().join("persistent"),
        ..Default::default()
    };
    std::fs::create_dir_all(&storage.data_dir).expect("durable root");
    let logs = captured(|| {
        create_dirs_for_storage(&storage).expect("split boot");
    });
    // Scoped to this warning rather than to "no warnings at all": under a
    // umask that makes the scratch root group-writable the boot also warns
    // about repairing it, which is a different and correct report.
    let warned = logs.tail("warn", 16);
    assert!(
        !format!("{warned:?}").contains("still holds entries"),
        "{warned:?}"
    );
}

#[test]
fn a_data_dir_that_cannot_be_created_is_reported_with_its_path() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let blocked = tmp.path().join("data");
    std::fs::write(&blocked, b"a file, not a directory").expect("write");
    let error = format!("{:#}", create_dirs(&blocked).expect_err("must fail"));
    assert!(error.contains("configured data"), "{error}");
}

/// An empty `PLURX_HWACCEL=` is what Compose produces for an unset
/// variable. Seeding the stored setting from it would overwrite the
/// operator's chosen encoder with "no preference" at every restart.
#[test]
fn an_empty_hwaccel_override_does_not_overwrite_the_stored_preference() {
    assert_eq!(hwaccel_override(None), None);
    assert_eq!(hwaccel_override(Some(String::new())), None);
    assert_eq!(
        hwaccel_override(Some("  NVENC  ".to_owned())),
        Some("nvenc".to_owned())
    );
}

/// "auto" is the *absence* of a preference. Passed to the chooser
/// literally it matches no encoder, so a node with a working GPU would
/// quietly fall back to software.
#[test]
fn auto_is_asked_as_no_preference_at_all() {
    assert_eq!(probe_preference("auto"), "");
    assert_eq!(probe_preference("nvenc"), "nvenc");
    assert_eq!(probe_preference(""), "");
}

/// Both the boot probe and the admin UI read the stored setting, so there
/// is one answer rather than two that can disagree.
#[tokio::test]
async fn the_stored_encoder_preference_defaults_to_auto_and_is_read_back() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let store = store_in(tmp.path());

    assert_eq!(resolve_hwaccel_pref(&store).await.expect("pref"), "auto");

    // PLURX_HWACCEL seeds the stored setting, so the admin UI shows what
    // the deployment asked for rather than disagreeing with it. Set and
    // cleared here because the variable is process-wide.
    std::env::set_var("PLURX_HWACCEL", "  NVENC  ");
    assert_eq!(resolve_hwaccel_pref(&store).await.expect("pref"), "nvenc");
    assert_eq!(
        store.get_setting(keys::HWACCEL).await.expect("setting"),
        Some("nvenc".to_owned()),
        "the env override must be durable, not just this boot's answer"
    );
    std::env::remove_var("PLURX_HWACCEL");

    store
        .put_setting(keys::HWACCEL, "vaapi")
        .await
        .expect("put");
    assert_eq!(resolve_hwaccel_pref(&store).await.expect("pref"), "vaapi");

    // A setting cleared through the UI is "no preference", not the empty
    // encoder name — which would select nothing at all.
    store.put_setting(keys::HWACCEL, "").await.expect("put");
    assert_eq!(resolve_hwaccel_pref(&store).await.expect("pref"), "auto");
}

/// The settings page renders this verbatim, so nothing at all must not
/// arrive as a blank version string.
#[test]
fn a_silent_binary_reports_no_version_rather_than_an_empty_one() {
    assert_eq!(
        first_version_line(b"ffmpeg version 7.1.1 Copyright (c)\nbuilt with clang\n"),
        Some("ffmpeg version 7.1.1 Copyright (c)".to_owned())
    );
    assert_eq!(first_version_line(b""), None);
    assert_eq!(first_version_line(b"   \nffmpeg version 7\n"), None);
}

/// A binary that is not there is not a startup failure — the settings page
/// simply has no version to show.
#[tokio::test]
async fn a_missing_ffmpeg_has_no_version() {
    assert_eq!(ffmpeg_version("plurxd-no-such-binary-anywhere").await, None);
}

/// And a binary that does answer is reported by its own first line. Run
/// against `/bin/echo` rather than ffmpeg so the spawn is real but the
/// output is this test's to choose: what is being pinned is that the
/// banner reaching the settings page is the process's actual stdout,
/// which a version read from anywhere else would not be.
#[tokio::test]
async fn a_version_banner_is_read_from_what_the_binary_printed() {
    assert_eq!(
        ffmpeg_version("/bin/echo").await,
        Some("-version".to_owned()),
        "echo repeats the flag it was passed, so that is the banner"
    );
}

/// A server that never appears has to leave a trail without filling the
/// log with one line every two seconds.
#[test]
fn waiting_for_a_server_logs_the_first_attempt_then_rations_itself() {
    assert!(should_log_wait(1), "the first failure is always reported");
    assert!(!should_log_wait(2));
    assert!(!should_log_wait(14));
    assert!(should_log_wait(15), "then one line every thirty seconds");
    assert!(should_log_wait(30));
    assert!(!should_log_wait(31));
}

/// The advertised port comes from the URL, because the whole point of the
/// companion is a server published on the *host* at whatever port the
/// operator mapped. Advertising the container's port would produce a
/// Bonjour record no client on the LAN can connect to.
#[test]
fn the_advertised_port_is_the_one_the_host_published() {
    let (base, port, info) = discovery_endpoints("http://10.42.1.9:8096").expect("endpoints");
    assert_eq!(port, 8096);
    assert_eq!(info.as_str(), "http://10.42.1.9:8096/api/v1/server");
    assert_eq!(base.as_str(), "http://10.42.1.9:8096/");

    // A scheme's default port is a real answer.
    let (_, port, info) = discovery_endpoints("https://plurx.example").expect("endpoints");
    assert_eq!(port, 443);
    assert_eq!(info.as_str(), "https://plurx.example/api/v1/server");

    // Anything that is not an HTTP URL is refused rather than advertised
    // as something clients cannot reach.
    for bad in ["ftp://host:21", "not a url", "file:///etc/plurx"] {
        assert!(discovery_endpoints(bad).is_err(), "{bad} must be refused");
    }
}

/// A local server standing in for the real `/api/v1/server`.
async fn identity_server(body: &'static str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = axum::Router::new().route(
        "/api/v1/server",
        axum::routing::get(move || async move { ([("content-type", "application/json")], body) }),
    );
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), handle)
}

const SERVER_IDENTITY: &str = r#"{"name":"Living Room","version":"0.2.0","instance_id":"abc","node_id":"node-a","cluster_advertisement":false}"#;

/// The same server as one node of a cluster: the companion reads
/// `cluster_advertisement` and must publish this node's own identity.
const CLUSTERED_IDENTITY: &str = r#"{"name":"Living Room","version":"0.2.0","instance_id":"logical-server","node_id":"node-b","cluster_advertisement":true}"#;

/// One registration attempt, as an advertiser was asked to make it.
#[derive(Clone)]
struct Advertised {
    instance_id: String,
    name: String,
    instance_name: String,
    node_id: Option<String>,
    port: u16,
    version: String,
}

/// What this machine publishes for `configured`, worked out without
/// calling the code under test: the hostname and the address a runner
/// happens to have decide the answer, and a test that asked
/// `instance_label` would only be asking the change to agree with itself.
fn expected_instance_name(configured: &str, node_id: Option<&str>) -> String {
    let machine = system_hostname();
    let label = match (configured, machine.as_deref()) {
        ("plurx", Some(host)) => host.to_owned(),
        (configured, _) => configured.to_owned(),
    };
    match (primary_lan_address(), machine.as_deref(), node_id) {
        // An address always tells one node from another.
        (Some(address), _, _) => format!("{label} · {address}"),
        // So does a hostname, but only when the label is the hostname.
        (None, Some(_), _) if label != configured || configured == "plurx" => label,
        // Nothing else does, so a cluster node falls back to its id.
        (None, _, Some(node_id)) => format!("{label} · {}", node_label_suffix(node_id)),
        (None, _, None) => label,
    }
}

#[tokio::test]
async fn the_companion_advertises_the_identity_the_server_reports() {
    let (base_url, server) = identity_server(SERVER_IDENTITY).await;
    let (base, _, info_url) = discovery_endpoints(&base_url).expect("endpoints");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .expect("client");

    let info = fetch_discovery_server_info(&client, info_url.clone())
        .await
        .expect("identity");
    assert_eq!(info.name, "Living Room");
    assert_eq!(info.instance_id, "abc");

    // And the retry loop returns that identity rather than waiting.
    let mut never = Box::pin(std::future::pending::<()>());
    let info = await_discovery_identity(
        &client,
        &base,
        info_url,
        Duration::from_millis(10),
        &mut never,
    )
    .await
    .expect("identity");
    assert_eq!(info.version, "0.2.0");
    server.abort();
}

/// The companion starts beside the server, not after it, so a refused
/// connection is the normal first few seconds. It must keep retrying — and
/// it must still stop when the container is asked to.
#[tokio::test]
async fn a_companion_waiting_on_a_dead_server_still_stops_on_shutdown() {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(200))
        .build()
        .expect("client");
    // Port 1 on loopback: nothing is listening, and connect fails fast.
    let (base, _, info_url) = discovery_endpoints("http://127.0.0.1:1").expect("endpoints");
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = tx.send(());
    });
    let mut shutdown = Box::pin(async move {
        let _ = rx.await;
    });

    let waited = await_discovery_identity(
        &client,
        &base,
        info_url,
        Duration::from_millis(20),
        &mut shutdown,
    )
    .await;
    assert!(
        waited.is_none(),
        "a shutdown while waiting must not advertise a server it never reached"
    );
}

/// The discovery record is withdrawn as part of the drain, and a
/// withdrawal that cannot be made is a warning rather than a failed
/// shutdown.
///
/// The daemon here is already shut down before `serve` is given it, which
/// is the awkward case: a node whose Bonjour daemon has gone must still
/// exit 0, because the alternative is `docker stop` reporting a failed
/// container for a server that drained perfectly well. Turning that warning
/// back into a `?` or an `expect` is exactly what this catches.
#[tokio::test]
async fn a_discovery_daemon_that_cannot_be_withdrawn_still_drains_cleanly() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let state = booted_state(tmp.path());
    let progress = Arc::clone(&state.progress);
    let app = http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");

    let daemon = daemon_off_the_mdns_port().expect("mDNS daemon");
    // Withdraw it first, and wait for the daemon to report that it has
    // actually stopped — otherwise the withdrawal inside `serve` races the
    // daemon thread and may still be accepted, which is not the case this
    // owns. Nothing was ever registered, so no record reaches the network.
    daemon
        .shutdown()
        .expect("the first withdrawal is the real one")
        .recv()
        .expect("the daemon reports when it has stopped");

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let logs = Arc::new(logbuf::LogBuffer::new(64));
    let _capture = capturing(&logs);
    let served = tokio::spawn(serve(
        listener,
        app,
        JellyfinStandardListener::default(),
        https::Serving::default(),
        progress,
        Some(daemon),
        async move {
            let _ = stopped.await;
        },
    ));
    stop.send(()).expect("stop");

    tokio::time::timeout(Duration::from_secs(10), served)
        .await
        .expect("serve must finish inside the drain window")
        .expect("join")
        .expect("a discovery daemon that cannot be withdrawn is not a failed shutdown");

    // Exiting 0 is only half of it. A withdrawal that silently did not
    // happen leaves a record on the LAN for a server that is gone, so the
    // operator has to be told which half of the drain failed.
    let warned = logs.tail("warn", 64);
    assert!(
        warned
            .iter()
            .any(|entry| entry.message.contains("Bonjour discovery shutdown failed")),
        "the failed withdrawal must reach the operator: {warned:?}"
    );
}

/// Playback progress is coalesced in memory, so the beats between one
/// durable commit and the next exist nowhere else. A `docker stop` in that
/// window must commit them rather than drop them — resuming a film minutes
/// behind where it was left is the failure an operator actually sees — and
/// the drain says how many it saved.
#[tokio::test]
async fn playback_progress_pending_at_shutdown_is_committed_not_lost() {
    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary};

    let tmp = crate::test_tempdir().expect("tempdir");
    let state = booted_state(tmp.path());
    let store = Arc::clone(&state.store);
    let progress = Arc::clone(&state.progress);

    let user = store
        .create_user("viewer", "hash", false)
        .await
        .expect("user");
    let library = store
        .create_library(&NewLibrary {
            name: "Films".to_owned(),
            kind: LibraryKind::Movies,
            paths: vec![std::path::PathBuf::from("/films")],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Interrupted".to_owned(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");

    // The first beat commits durably; the second lands inside the commit
    // window and so exists only in memory. That is the one at risk.
    progress
        .put(user.id, item, 1_000, Some(100_000))
        .await
        .expect("leading beat");
    progress
        .put(user.id, item, 7_000, Some(100_000))
        .await
        .expect("pending beat");

    let app = http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let logs = Arc::new(logbuf::LogBuffer::new(64));
    let _capture = capturing(&logs);
    let served = tokio::spawn(serve(
        listener,
        app,
        JellyfinStandardListener::default(),
        https::Serving::default(),
        progress,
        None,
        async move {
            let _ = stopped.await;
        },
    ));
    stop.send(()).expect("stop");

    tokio::time::timeout(Duration::from_secs(10), served)
        .await
        .expect("serve must finish inside the drain window")
        .expect("join")
        .expect("a clean drain");

    assert_eq!(
        store
            .watch_state(user.id, item)
            .await
            .expect("watch state")
            .expect("a watched item")
            .position_ms,
        7_000,
        "the beat that was only in memory has to survive the shutdown"
    );
    let flushed = logs.tail("info", 64);
    assert!(
        flushed.iter().any(|entry| entry
            .message
            .contains("flushed coalesced playback progress during shutdown")
            && entry.message.contains("flushed=1")),
        "the drain must report what it saved: {flushed:?}"
    );
}

/// A loopback bind is common behind a proxy. Advertising it would put a
/// setup button in every client on the LAN that can never connect.
///
/// Only the loopback refusal is asserted here, because that decision is
/// taken before `PLURX_GDM_PORT` is ever read and so cannot be perturbed by
/// a test running beside this one. What the LAN case resolves to depends on
/// that process-wide variable, so it belongs to the test that owns it.
#[test]
fn a_loopback_bind_is_never_advertised_over_gdm() {
    assert_eq!(
        gdm_responder_port(false, "127.0.0.1:32400".parse().expect("addr")),
        None
    );
}

/// Only a real `M-SEARCH` draws a reply. Answering anything that arrives
/// would make the server an amplification reflector on its own LAN.
#[tokio::test]
async fn gdm_answers_a_search_and_ignores_everything_else() {
    let server = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind server");
    let server_addr = server.local_addr().expect("addr");
    let responder = tokio::spawn(async move {
        let _ = gdm_serve(&server, "instance-id", None, "Living Room", 32400).await;
    });

    let client = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind client");
    client.connect(server_addr).await.expect("connect");

    // Noise first: it must draw nothing, and must not stop the responder.
    client.send(b"hello?").await.expect("send noise");
    let mut buf = [0u8; 2048];
    let quiet = tokio::time::timeout(Duration::from_millis(150), client.recv(&mut buf)).await;
    assert!(quiet.is_err(), "unsolicited traffic must not draw a reply");

    client
        .send(b"M-SEARCH * HTTP/1.0\r\n\r\n")
        .await
        .expect("send search");
    let n = tokio::time::timeout(Duration::from_secs(2), client.recv(&mut buf))
        .await
        .expect("a GDM reply")
        .expect("bytes");
    let reply = String::from_utf8_lossy(&buf[..n]);
    assert!(reply.contains("Living Room"), "{reply}");
    assert!(reply.contains("32400"), "{reply}");
    assert!(reply.contains(crate::version::SEMVER), "{reply}");
    responder.abort();
}

/// The container health check. Its output lands in `docker inspect`, and
/// its verdict decides whether an orchestrator restarts the server — so a
/// server answering anything but 200 has to fail rather than pass quietly.
#[test]
fn the_health_check_believes_only_a_200() {
    fn serve_once(
        status_line: &'static str,
    ) -> (
        u16,
        std::sync::mpsc::Receiver<Vec<u8>>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut request = [0u8; 512];
                let read = socket.read(&mut request).unwrap_or_default();
                let _ = request_tx.send(request[..read].to_vec());
                let _ = socket.write_all(status_line.as_bytes());
            }
        });
        (port, request_rx, handle)
    }

    let tmp = crate::test_tempdir().expect("tempdir");
    let mut config = config_in(tmp.path());

    let (port, request, server) = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
    config.server.bind = format!("127.0.0.1:{port}").parse().expect("addr");
    healthcheck(&config).expect("a 200 is healthy");
    assert!(
        request
            .recv()
            .expect("health request")
            .starts_with(b"GET /readyz HTTP/1.0\r\n"),
        "container health must measure serving readiness"
    );
    server.join().expect("server thread");

    let (port, _request, server) = serve_once("HTTP/1.1 503 Service Unavailable\r\n\r\n");
    config.server.bind = format!("127.0.0.1:{port}").parse().expect("addr");
    let error = format!(
        "{:#}",
        healthcheck(&config).expect_err("503 is not healthy")
    );
    assert!(error.contains("503"), "{error}");
    server.join().expect("server thread");

    // Nothing listening at all is unhealthy, and says which address it
    // could not reach.
    config.server.bind = "127.0.0.1:1".parse().expect("addr");
    let error = format!("{:#}", healthcheck(&config).expect_err("no server"));
    assert!(error.contains("127.0.0.1:1"), "{error}");
}

/// `plurxd healthcheck` is what Docker runs, so it has to reach the
/// running server through the same config the server was started with.
#[tokio::test]
async fn the_healthcheck_subcommand_probes_the_configured_port() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = std::thread::spawn(move || {
        if let Ok((mut socket, _)) = listener.accept() {
            let mut request = [0u8; 512];
            let _ = socket.read(&mut request);
            let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        }
    });

    let tmp = crate::test_tempdir().expect("tempdir");
    let mut config = config_in(tmp.path());
    config.server.bind = format!("127.0.0.1:{port}").parse().expect("addr");
    dispatch(Command::Healthcheck, config, None)
        .await
        .expect("healthy");
    server.join().expect("server thread");
}

/// A console process is not a daemon-local revocation authority. The
/// refusal must happen before either the password or existing sessions can
/// change, even when the target store is otherwise writable.
#[tokio::test]
async fn console_password_reset_fails_closed_before_any_store_mutation() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let config = config_in(tmp.path());
    let store = store_in(tmp.path());
    let hash = plurx_core::auth::hash_password("original-password").expect("hash");
    let user = store
        .create_user("owner", &hash, true)
        .await
        .expect("create user");
    store
        .create_token("session-token", user.id, None)
        .await
        .expect("token");
    let error = reset_password(&config, "owner", Some("a-new-password".to_owned()))
        .await
        .expect_err("direct Store reset must be refused");
    assert_eq!(error.to_string(), RESET_PASSWORD_REQUIRES_DAEMON_CONTROL);

    let after = store
        .get_user_by_username("owner")
        .await
        .expect("lookup")
        .expect("user");
    assert!(
        plurx_core::auth::verify_password("original-password", &after.password_hash),
        "the refusal must leave the password unchanged"
    );
    assert!(
        store
            .user_for_token("session-token")
            .await
            .expect("token lookup")
            .is_some(),
        "the refusal must not partially revoke sessions"
    );
}

#[tokio::test]
async fn a_password_reset_does_not_process_an_unusable_secret() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let config = config_in(tmp.path());
    let store = store_in(tmp.path());
    let hash = plurx_core::auth::hash_password("original-password").expect("hash");
    store
        .create_user("owner", &hash, true)
        .await
        .expect("create user");
    drop(store);

    let error = reset_password(&config, "owner", Some("short".to_owned()))
        .await
        .expect_err("direct reset must be refused");
    assert_eq!(error.to_string(), RESET_PASSWORD_REQUIRES_DAEMON_CONTROL);

    let store = store_in(tmp.path());
    // The real account's password is untouched by either refusal.
    let user = store
        .get_user_by_username("owner")
        .await
        .expect("lookup")
        .expect("user");
    assert!(plurx_core::auth::verify_password(
        "original-password",
        &user.password_hash
    ));
}

/// Maintenance commands must not become a second migration coordinator.
/// Reset refuses even earlier at its daemon-control boundary; metadata
/// reaches store selection but still cannot create an activated target.
#[tokio::test]
async fn maintenance_commands_refuse_an_unmigrated_data_directory() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let mut config = config_in(tmp.path());
    let store = store_in(tmp.path());
    let hash = plurx_core::auth::hash_password("original-password").expect("hash");
    store
        .create_user("owner", &hash, true)
        .await
        .expect("create user");
    drop(store);

    let reset = reset_password(&config, "owner", Some("a-new-password".to_owned()))
        .await
        .expect_err("reset must refuse a direct Store write");
    assert_eq!(reset.to_string(), RESET_PASSWORD_REQUIRES_DAEMON_CONTROL);
    let refresh = refresh_metadata(&mut config, None)
        .await
        .expect_err("refresh must refuse legacy SQLite");
    let message = format!("{refresh:#}");
    assert!(
        message.contains("only `plurxd run` may import"),
        "{message}"
    );
    assert!(!tmp.path().join("hiqlite.incoming").exists());
    assert!(!tmp.path().join("hiqlite").exists());

    let store = store_in(tmp.path());
    let user = store
        .get_user_by_username("owner")
        .await
        .expect("lookup")
        .expect("user");
    assert!(plurx_core::auth::verify_password(
        "original-password",
        &user.password_hash
    ));
}

async fn add_library(
    store: &Arc<dyn Store>,
    name: &str,
    kind: LibraryKind,
    anime: bool,
) -> Library {
    store
        .create_library(&plurx_core::domain::NewLibrary {
            name: name.to_owned(),
            kind,
            paths: vec![std::path::PathBuf::from("/media")],
            anime,
        })
        .await
        .expect("library")
}

/// Book and home-video libraries have no provider behind them, so a
/// refresh must skip them rather than fail the whole run on a missing
/// TMDB key it was never going to use.
#[tokio::test]
async fn a_refresh_skips_the_libraries_that_have_no_provider() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let store = store_in(tmp.path());
    add_library(&store, "Books", LibraryKind::Books, false).await;
    add_library(&store, "Home", LibraryKind::Home, false).await;
    let artwork = tmp.path().join("artwork");
    refresh_metadata_with_store(
        store,
        &artwork,
        None,
        "test-refresh",
        &plurx_core::cluster::coordination::UnclusteredJobAuthority,
    )
    .await
    .expect("a provider-less refresh must succeed");
}

/// Naming a library that does not exist is a mistake worth reporting: the
/// alternative is a run that silently refreshes nothing and exits 0.
#[tokio::test]
async fn a_refresh_of_an_unknown_library_fails_rather_than_doing_nothing() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let store = store_in(tmp.path());
    add_library(&store, "Movies", LibraryKind::Movies, false).await;
    let artwork = tmp.path().join("artwork");

    let error = format!(
        "{:#}",
        refresh_metadata_with_store(
            store,
            &artwork,
            Some(4242),
            "test-refresh",
            &plurx_core::cluster::coordination::UnclusteredJobAuthority,
        )
        .await
        .expect_err("no such library")
    );
    assert!(error.contains("no library with id 4242"), "{error}");
}

/// A TMDB-backed library with no key configured stops with the reason,
/// rather than reporting a successful refresh that fetched nothing.
#[tokio::test]
async fn a_provider_refresh_without_a_key_says_so() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let store = store_in(tmp.path());
    add_library(&store, "Movies", LibraryKind::Movies, false).await;
    let artwork = tmp.path().join("artwork");

    let error = format!(
        "{:#}",
        refresh_metadata_with_store(
            store,
            &artwork,
            None,
            "test-refresh",
            &plurx_core::cluster::coordination::UnclusteredJobAuthority
        )
        .await
        .expect_err("no TMDB key")
    );
    assert!(error.contains("TMDB API key is not configured"), "{error}");
}

/// With a key present and nothing to enrich, both provider paths run to
/// completion — an empty library is a no-op, not an error, and an anime
/// library routes to AniList rather than TMDB.
#[tokio::test]
async fn an_empty_provider_library_refreshes_to_an_empty_report() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let store = store_in(tmp.path());
    store
        .put_setting(keys::TMDB_API_KEY, "not-a-real-key")
        .await
        .expect("key");
    let movies = add_library(&store, "Movies", LibraryKind::Movies, false).await;
    let anime = add_library(&store, "Anime", LibraryKind::Shows, true).await;
    assert!(anime.anime, "the anime flag must be stored");
    let artwork = tmp.path().join("artwork");

    refresh_metadata_with_store(
        Arc::clone(&store),
        &artwork,
        Some(movies.id),
        "test-refresh",
        &plurx_core::cluster::coordination::UnclusteredJobAuthority,
    )
    .await
    .expect("empty TMDB refresh");
    refresh_metadata_with_store(
        store,
        &artwork,
        Some(anime.id),
        "test-refresh",
        &plurx_core::cluster::coordination::UnclusteredJobAuthority,
    )
    .await
    .expect("empty AniList refresh");
}

/// Everything a request needs, assembled the way `run` assembles it.
fn booted_state(dir: &std::path::Path) -> AppState {
    let config = config_in(dir);
    let store = store_in(dir);
    let catalogue = plurx_core::store::CatalogueReader::authority(Arc::clone(&store));
    build_state(
        &config,
        "test-node".to_owned(),
        "test-cluster".to_owned(),
        Arc::new(plurx_core::secrets::CredentialKey::generate()),
        None,
        plurx_core::cluster::migration::status::ReplicationMonitor::sqlite(),
        plurx_core::cluster::membership::MembershipManager::unavailable(),
        catalogue,
        store,
        create_dirs(dir).expect("dirs"),
        Default::default(),
        Default::default(),
        logbuf::LogBuffers {
            general: Arc::new(logbuf::LogBuffer::new(64)),
            cluster: Arc::new(logbuf::LogBuffer::new(64)),
        },
    )
}

#[tokio::test]
async fn https_prepared_socket_is_released_when_later_http_boot_binding_fails() {
    let root = crate::test_tempdir().expect("tempdir");
    let occupied_http = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("occupied HTTP");
    let tls_reservation = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve TLS");
    let tls_address = tls_reservation.local_addr().expect("TLS address");
    drop(tls_reservation);
    let mut config = config_in(root.path());
    config.server.bind = occupied_http.local_addr().expect("HTTP address");
    config.server.https.enabled = true;
    config.server.https.port = tls_address.port();
    let handle = plurx_core::cluster::open_store(&config)
        .await
        .expect("store");
    let catalogue = plurx_core::store::CatalogueReader::authority(handle.store.clone());
    let result = boot(
        config,
        Boot {
            source_activation_at_boot: None,
            store: handle.store,
            catalogue,
            replication: plurx_core::cluster::migration::status::ReplicationMonitor::sqlite(),
            membership: plurx_core::cluster::membership::MembershipManager::unavailable(),
            identity: handle.identity,
            credential_key: handle.credential_key,
            backup_client: None,
            dirs: create_dirs(root.path()).expect("dirs"),
            encoder_caps: Default::default(),
            system: Default::default(),
            logs: logbuf::LogBuffers {
                general: Arc::new(logbuf::LogBuffer::new(64)),
                cluster: Arc::new(logbuf::LogBuffer::new(64)),
            },
        },
        advertiser_that_fails,
        std::future::pending(),
    )
    .await;
    assert!(result.is_err(), "real HTTP boot bind must fail");
    assert!(
        root.path()
            .join("https")
            .join(https::MANAGED_IDENTITY_FILE)
            .exists(),
        "TLS preparation occurred before later startup failed"
    );
    tokio::net::TcpListener::bind(tls_address)
        .await
        .expect("failed boot must release prepared TLS socket");
}

#[tokio::test]
async fn https_and_http_share_full_router_auth_and_both_startup_owners_drain() {
    for observed in [false, true] {
        let root = crate::test_tempdir().expect("tempdir");
        let state = booted_state(root.path());
        let progress = state.progress.clone();
        let daemon_stop = state.shutdown.clone();
        let app = http::router(state);
        let plain_socket = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("HTTP");
        let plain_address = plain_socket.local_addr().expect("HTTP address");
        let reservation = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reserve TLS port");
        let tls_address = reservation.local_addr().expect("TLS address");
        drop(reservation);
        let mut plain_socket = Some(plain_socket);
        let observer = if observed {
            drop(plain_socket.take());
            let owner = Arc::new(StartupObservationHttp::new(plain_address));
            owner
                .start(
                    plurx_core::cluster::membership::MembershipManager::unavailable(),
                    "pending-node".into(),
                )
                .await
                .expect("pending owner");
            let pending = reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("client")
                .get(format!("http://{plain_address}/api/v1/server"))
                .send()
                .await
                .expect("pending response");
            assert_eq!(pending.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
            assert!(
                tokio::net::TcpStream::connect(tls_address).await.is_err(),
                "observation-only phase is HTTP"
            );
            Some(owner)
        } else {
            None
        };
        // Reacquire only for the ordinary owner; the observation owner keeps
        // its original socket and changes the router in place.
        let ordinary = if observer.is_none() {
            plain_socket.take()
        } else {
            None
        };
        let mut config = config_in(root.path());
        config.server.https.enabled = true;
        config.server.https.port = tls_address.port();
        let prepared = https::Prepared::open(&config)
            .await
            .expect("prepare TLS")
            .expect("enabled");
        let root_cert = reqwest::Certificate::from_pem(&prepared.identity.public_chain_pem())
            .expect("loaded public chain");
        let listener_stop = tokio_util::sync::CancellationToken::new();
        let tls = https::Serving::start(
            Some(prepared),
            app.clone(),
            listener_stop.clone(),
            daemon_stop,
        );
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let shutdown = async move {
            let _ = stopped.await;
            listener_stop.cancel();
        };
        let server = if let Some(owner) = observer {
            tokio::spawn(serve_observed(
                owner,
                app,
                JellyfinStandardListener::default(),
                tls,
                progress,
                None,
                shutdown,
            ))
        } else {
            tokio::spawn(serve(
                ordinary.expect("ordinary listener"),
                app,
                JellyfinStandardListener::default(),
                tls,
                progress,
                None,
                shutdown,
            ))
        };
        let client = reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(root_cert)
            .timeout(Duration::from_secs(5))
            .build()
            .expect("normally trusted client");
        for scheme in ["http", "https"] {
            let address = if scheme == "http" {
                plain_address.to_string()
            } else {
                format!("localhost:{}", tls_address.port())
            };
            let base = format!("{scheme}://{address}");
            assert!(client
                .get(format!("{base}/healthz"))
                .send()
                .await
                .expect("health")
                .status()
                .is_success());
            let info: serde_json::Value = client
                .get(format!("{base}/api/v1/server"))
                .send()
                .await
                .expect("bootstrap")
                .json()
                .await
                .expect("info");
            assert_eq!(info["node_id"], "test-node");
            let qr = client
                .get(format!("{base}/connect.svg?origin={base}"))
                .send()
                .await
                .expect("QR response")
                .error_for_status()
                .expect("QR preserves current origin");
            assert!(qr
                .headers()
                .get("content-type")
                .expect("SVG type")
                .to_str()
                .expect("type text")
                .starts_with("image/svg+xml"));
            assert!(qr.text().await.expect("QR SVG").contains("<svg"));
            for (path, expected) in [
                ("/api/v1/me", reqwest::StatusCode::UNAUTHORIZED),
                ("/api/v1/settings", reqwest::StatusCode::UNAUTHORIZED),
                ("/api/v1/files/1/direct", reqwest::StatusCode::UNAUTHORIZED),
                (
                    "/api/v1/images/missing.jpg",
                    reqwest::StatusCode::UNAUTHORIZED,
                ),
                // HLS uses an opaque capability; unknown sessions retain
                // their existing 404 behavior on both public transports.
                (
                    "/api/v1/hls/00000000-0000-0000-0000-000000000000/index.m3u8",
                    reqwest::StatusCode::NOT_FOUND,
                ),
            ] {
                assert_eq!(
                    client
                        .get(format!("{base}{path}"))
                        .send()
                        .await
                        .expect("guarded response")
                        .status(),
                    expected,
                    "{scheme} {path}"
                );
            }
        }
        stop.send(()).expect("stop");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(8), server)
                .await
                .expect("bounded drain")
                .expect("join")
                .expect("serve"),
            HttpDrain::Complete
        );
        tokio::net::TcpListener::bind(plain_address)
            .await
            .expect("HTTP port released");
        tokio::net::TcpListener::bind(tls_address)
            .await
            .expect("TLS port released");
    }
}

#[tokio::test]
async fn the_assembled_state_carries_the_configured_identity_and_directories() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let state = booted_state(tmp.path());
    let root = std::fs::canonicalize(tmp.path()).expect("canonical state root");
    assert_eq!(state.server_name, "plurx");
    assert_eq!(state.node_id, "test-node");
    assert_eq!(state.artwork_dir, root.join("artwork"));
    assert_eq!(state.cache_dir, root.join("cache").join("transcode"));
    assert_eq!(state.subs_dir, root.join("cache").join("subs"));
    // The scratch and the persistent cache both reached the transcode
    // manager: with the cache wired, ffmpeg's runtime cache is placed
    // beside it rather than inside the scratch that gets cleared at boot.
    assert!(
        tmp.path().join("cache").join("runtime").is_dir(),
        "the ffmpeg runtime cache must exist before any session starts"
    );
    crate::telemetry::drain_for_shutdown(&state.store).await;
}

/// The server answers requests, then stops when it is told to — and the
/// coalesced playback progress is flushed on the way out rather than lost.
#[tokio::test]
async fn the_server_answers_until_it_is_asked_to_stop_and_then_drains() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let state = booted_state(tmp.path());
    state
        .transcode
        .initialize_rate_control()
        .await
        .expect("rate control");
    let background_loops = BackgroundLoopGuard::new();
    spawn_background_loops(&state, background_loops.token());

    let progress = Arc::clone(&state.progress);
    let app = http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(serve(
        listener,
        app,
        JellyfinStandardListener::default(),
        https::Serving::default(),
        progress,
        None,
        async move {
            let _ = stopped.await;
        },
    ));

    let client = reqwest::Client::new();
    let health = client
        .get(format!("http://{addr}/healthz"))
        .send()
        .await
        .expect("healthz");
    assert!(health.status().is_success(), "{:?}", health.status());

    stop.send(()).expect("stop");
    let outcome = tokio::time::timeout(Duration::from_secs(10), served)
        .await
        .expect("serve must finish inside the drain window")
        .expect("join");
    assert_eq!(
        outcome.expect("an orderly shutdown is exit 0, not an error"),
        HttpDrain::Complete,
        "every connection finished, so the drain says so"
    );

    // And the listener really is gone: an orchestrator that restarts the
    // container must not race a socket that is still bound.
    assert!(
        reqwest::Client::new()
            .get(format!("http://{addr}/healthz"))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_err(),
        "the port must be released once serve returns"
    );
}

/// A served request carries its socket peer, because the network-priors
/// handlers read one.
///
/// This is the quietest way this file can break. `http::network`'s
/// `RemoteAddress` takes the peer from the request extensions as an
/// `Option` and its rejection is `Infallible`, so serving the bare router
/// instead of `into_make_service_with_connect_info` is not a compile error
/// and not a failed request — every prior simply reduces to `None` and gets
/// written under a key nothing reads. Nothing else in the crate would
/// notice, so this asserts the peer arrives at a handler served through
/// `serve` rather than trusting the call site to keep saying so.
#[tokio::test]
async fn a_served_request_arrives_with_its_socket_peer() {
    // Deliberately not `http::router`: `network` is private to `http`, and
    // this reads the extension exactly the way `RemoteAddress` does, so the
    // assertion tracks the mechanism the real extractor depends on.
    async fn peer(request: axum::extract::Request) -> String {
        request
            .extensions()
            .get::<axum::extract::ConnectInfo<SocketAddr>>()
            .map(|axum::extract::ConnectInfo(address)| address.to_string())
            .unwrap_or_default()
    }

    let tmp = crate::test_tempdir().expect("tempdir");
    let state = booted_state(tmp.path());
    let progress = Arc::clone(&state.progress);
    let app = axum::Router::new().route("/peer", axum::routing::get(peer));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(serve(
        listener,
        app,
        JellyfinStandardListener::default(),
        https::Serving::default(),
        progress,
        None,
        async move {
            let _ = stopped.await;
        },
    ));

    let body = reqwest::Client::new()
        .get(format!("http://{addr}/peer"))
        .send()
        .await
        .expect("peer")
        .text()
        .await
        .expect("body");

    stop.send(()).expect("stop");
    tokio::time::timeout(Duration::from_secs(10), served)
        .await
        .expect("serve must finish inside the drain window")
        .expect("join")
        .expect("an orderly shutdown is exit 0, not an error");

    // An empty body is the revert symptom exactly: a bare router serves the
    // request perfectly well and simply has no peer to hand over.
    let seen: SocketAddr = body.parse().expect("a served request must carry a peer");
    // The /24 reduction in `http::network` admits IPv4 only, so a peer that
    // arrives as something other than the loopback the client actually came
    // from would silently produce no identity at all.
    let loopback = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    assert_eq!(seen.ip(), loopback);
}

/// `PLURX_LOG_FORMAT` decides only the console sink's shape, and anything
/// that is not `json` is text — including a misspelling, which must not
/// refuse a boot.
#[test]
fn the_log_format_variable_is_json_or_text_and_never_an_error() {
    assert_eq!(LogFormat::parse(Some("json")), LogFormat::Json);
    assert_eq!(LogFormat::parse(Some("JSON")), LogFormat::Json);
    assert_eq!(LogFormat::parse(Some(" json ")), LogFormat::Json);
    assert_eq!(LogFormat::parse(Some("text")), LogFormat::Text);
    assert_eq!(LogFormat::parse(Some("jsonl")), LogFormat::Text);
    assert_eq!(LogFormat::parse(None), LogFormat::Text);
}

fn captured_console(format: LogFormat, ansi: bool) -> String {
    let logs = logbuf::LogBuffers::default();
    let captured = logbuf::testwriter::CapturedWriter::new();
    let subscriber = logging_subscriber(
        &logs,
        EnvFilter::new("trace"),
        format,
        ansi,
        captured.clone(),
    );
    tracing::subscriber::with_default(subscriber, || {
        tracing::warn!(detail = "value", "console line");
    });
    captured.text()
}

#[test]
fn json_format_emits_lines_a_parser_can_read() {
    let text = captured_console(LogFormat::Json, false);
    let lines = text.lines().filter(|line| !line.is_empty()).count();
    assert_eq!(lines, 1, "{text}");
    for line in text.lines().filter(|line| !line.is_empty()) {
        let parsed: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}: {line}"));
        assert_eq!(parsed["level"], "WARN");
        assert_eq!(parsed["fields"]["message"], "console line");
        assert_eq!(parsed["fields"]["detail"], "value");
    }
}

/// Split-child events name their parent or an owned dedicated channel.
///
/// The S-14 moves put code from `transcode.rs`, `http/hls.rs` and
/// `vodserve.rs` into `#[path]` child modules, and an event's default
/// target is its `module_path!()`: without an explicit target a moved line
/// would be labelled `plurxd::transcode::manager_start` instead of
/// `plurxd::transcode` in the console, in journald and in the log view.
/// The walk covers every file on disk, so a new child is scanned without
/// being listed. Test children (`tests.rs`, `tests/`) are skipped.
#[test]
fn split_children_log_under_their_parent_target() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut events = 0;
    for (directory, target) in [
        ("transcode", "plurxd::transcode"),
        ("http/hls", "plurxd::http::hls"),
        ("vod", "plurxd::vodserve"),
    ] {
        let mut files = Vec::new();
        let mut directories = vec![src.join(directory)];
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(&directory).expect("a split directory") {
                let path = entry.expect("a directory entry").path();
                if path.file_stem().and_then(|stem| stem.to_str()) == Some("tests") {
                    continue;
                }
                if path.is_dir() {
                    directories.push(path);
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                    files.push(path);
                }
            }
        }
        assert!(
            files.len() > 1,
            "{directory}: the walk reached no child modules"
        );
        let pinned = format!("target: \"{target}\",");
        for path in files {
            let source = std::fs::read_to_string(&path).expect("a readable child");
            for level in ["trace", "debug", "info", "warn", "error"] {
                let call = format!("tracing::{level}!(");
                for (at, _) in source.match_indices(&call) {
                    let arguments = source[at + call.len()..].trim_start();
                    // Three diagnostic channels are deliberately separate from
                    // their parent target so an operator can enable them
                    // alone: the hashed retained-output reuse facts
                    // (CONTINUOUS-QUALITY-BUILD) and the typed Auto
                    // preparation outcomes (2ed23494f), and source-fenced FEL
                    // qualification (681d628f6d). Each is admitted
                    // only in the files that own it, and still requires an
                    // explicit target, never a child default.
                    let relative = path.strip_prefix(&src).expect("split child under src");
                    let dedicated = [
                        ("plurxd::retained_reuse", "transcode/manager/create.rs"),
                        ("plurxd::retained_reuse", "vod/retained.rs"),
                        ("plurxd::dv", "transcode/manager/create.rs"),
                        ("plurxd::auto_prepare", "http/hls/create.rs"),
                        ("plurxd::auto_prepare", "http/hls/preparation.rs"),
                    ]
                    .iter()
                    .any(|(channel, owner)| {
                        relative == std::path::Path::new(owner)
                            && arguments.starts_with(&format!("target: \"{channel}\","))
                    });
                    assert!(
                        arguments.starts_with(&pinned) || dedicated,
                        "{}: the `{call}` at byte {at} logs under its own module path, \
                             not `{target}`",
                        path.display()
                    );
                    events += 1;
                }
            }
            // Other event and span forms take the same default; none is
            // used in a child today, so any one appearing is a new case.
            for form in ["tracing::event!(", "_span!(", "instrument"] {
                assert!(
                    !source.contains(form),
                    "{}: `{form}` in a split child needs its target pinned too",
                    path.display()
                );
            }
        }
    }
    assert!(
        events >= 299,
        "the scan found {events} events; the split moved 299"
    );
}

/// `tracing-subscriber` turns ANSI on whenever the `ansi` feature is
/// compiled and `NO_COLOR` is unset, with no TTY detection anywhere, so a
/// systemd unit or a container gets escape bytes in its journal unless
/// something sets the flag. This is that something.
#[test]
fn ansi_is_off_when_stdout_is_not_a_terminal_and_never_on_in_json() {
    const ESCAPE: &str = "\u{1b}[";
    assert!(
        !captured_console(LogFormat::Text, false).contains(ESCAPE),
        "escape bytes reached a non-terminal sink"
    );
    // The flag is load-bearing rather than decorative: with it on, the very
    // same line does carry escapes.
    assert!(
        captured_console(LogFormat::Text, true).contains(ESCAPE),
        "the ansi flag decided nothing"
    );
    // JSON forces it off whatever the terminal says, because an escape
    // sequence inside a JSON string field is a parse hazard, not a colour.
    assert!(!captured_console(LogFormat::Json, true).contains(ESCAPE));
}

#[test]
fn cluster_detail_stays_off_the_console_unless_it_is_a_warning() {
    // The console rule that predates the format switch, pinned so neither
    // format can quietly change which events reach stdout: cluster INFO
    // belongs on Settings -> Cluster, cluster WARN still reaches a service
    // supervisor, and everything else is unaffected.
    for format in [LogFormat::Text, LogFormat::Json] {
        let logs = logbuf::LogBuffers::default();
        let captured = logbuf::testwriter::CapturedWriter::new();
        let subscriber = logging_subscriber(
            &logs,
            EnvFilter::new("info"),
            format,
            false,
            captured.clone(),
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "plurx::cluster", "cluster detail");
            tracing::warn!(target: "plurx_core::cluster::migration::status", proof_attempt_sequence = 7, proof_in_flight = true, phase = "request_round_trip", "quorum proof expiry evidence");
            tracing::warn!(target: "plurx_core::cluster::migration::status", attempt_seq = 8, outage_ms = 900, suppressed = 3, "quorum watermark sampling recovered");
            tracing::info!(target: "plurxd::http", "ordinary detail");
        });
        let console = captured.text();
        assert!(!console.contains("cluster detail"), "{format:?}: {console}");
        assert!(
            console.contains("quorum proof expiry evidence"),
            "{format:?}: {console}"
        );
        assert!(console.contains("quorum watermark sampling recovered"));
        assert!(console.contains("ordinary detail"), "{format:?}: {console}");
        // The ring the product shows keeps the cluster detail the console
        // dropped, and keeps the two surfaces apart.
        assert_eq!(logs.cluster.tail("trace", 8).len(), 3);
        assert_eq!(logs.general.tail("trace", 8).len(), 1);
    }
}

/// The daemon installs exactly one subscriber, and losing that race is not
/// a reason to abort a boot — it can only mean logging already goes
/// somewhere. Both entry points have to survive being second.
#[test]
fn installing_logging_twice_does_not_abort_a_boot() {
    // A global subscriber outlives this test, so pin its filter to `off`
    // first: the only caller of either initialiser in this binary is right
    // here, and an `info` default would put daemon logging into every
    // other test's output.
    std::env::set_var("PLURX_LOG", "off");
    let logs = init_logging();
    assert!(
        logs.general.tail("trace", 8).is_empty() && logs.cluster.tail("trace", 8).is_empty(),
        "both fresh rings start empty"
    );
    // Second call, and the companion's own initialiser: neither may panic.
    let again = init_logging();
    assert!(!Arc::ptr_eq(&logs.general, &again.general));
    assert!(!Arc::ptr_eq(&logs.cluster, &again.cluster));
    init_companion_logging();
}

/// The standard port is best-effort: off and taken both leave the server
/// running and say so, and an open one answers over TCP, then stops
/// accepting when it drains.
#[tokio::test]
async fn jellyfin_standard_port_is_best_effort_and_drains_with_the_server() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let status = http::JellyfinStandardPortStatus::default();
    let app = axum::Router::new();

    let off = JellyfinStandardListener::open(None, &app, &status).await;
    assert_eq!(status.current(), http::JellyfinStandardPort::Off);
    assert_eq!(off.drain().await, HttpDrain::Complete);

    let held = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("hold a port");
    let taken = held.local_addr().expect("held address");
    let refused = JellyfinStandardListener::open(Some(taken), &app, &status).await;
    assert!(
        matches!(
            status.current(),
            http::JellyfinStandardPort::Unavailable { address, .. } if address == taken
        ),
        "{:?}",
        status.current()
    );
    assert_eq!(refused.drain().await, HttpDrain::Complete);

    let open =
        JellyfinStandardListener::open(Some("127.0.0.1:0".parse().expect("addr")), &app, &status)
            .await;
    let http::JellyfinStandardPort::Listening(bound) = status.current() else {
        panic!("not listening: {:?}", status.current());
    };
    assert_ne!(bound.port(), 0);
    let mut stream = tokio::net::TcpStream::connect(bound)
        .await
        .expect("connect");
    stream
        .write_all(b"GET /System/Info/Public HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("read");
    assert!(response.starts_with("HTTP/1.1 404"), "{response}");
    assert_eq!(open.drain().await, HttpDrain::Complete);
    assert!(
        tokio::net::TcpStream::connect(bound).await.is_err(),
        "a drained listener stops accepting"
    );
}

/// The common boot failure is a port already in use, and "Address already
/// in use" on its own does not say which one.
#[tokio::test]
async fn a_port_already_taken_is_reported_with_its_address() {
    let held = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = held.local_addr().expect("addr");

    let listener = bind_listener("127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("an ephemeral port is always available");
    assert_ne!(listener.local_addr().expect("addr").port(), 0);

    let error = format!("{:#}", bind_listener(addr).await.expect_err("taken"));
    assert!(error.contains(&addr.to_string()), "{error}");
}

/// One line naming both identities, which a cluster makes it possible to
/// confuse: the logical server clients bind to, and the process that owns
/// the local bytes.
#[test]
fn startup_names_the_server_and_the_node_separately() {
    let tmp = crate::test_tempdir().expect("tempdir");
    log_startup(
        &config_in(tmp.path()),
        &plurx_core::cluster::ClusterIdentity {
            cluster_id: "instance-1".to_owned(),
            node_id: "node-2".to_owned(),
            raft_id: 1,
        },
    );
}

/// An advertiser that never touches the network. Registering a real
/// Bonjour record puts a service on the LAN, which a test must not do to
/// check that discovery is best-effort.
fn advertiser_that_fails(
    _instance_id: &str,
    _name: &str,
    _instance_name: &str,
    _node_id: Option<&str>,
    _bind: SocketAddr,
    _version: &str,
) -> anyhow::Result<mdns_sd::ServiceDaemon> {
    anyhow::bail!("no mDNS daemon in this test")
}

/// The succeeding counterpart, for the callers that need a live daemon
/// handle rather than a fake. It publishes nothing: only `register` puts a
/// record on the network and this never calls it, and the daemon is bound
/// off the mDNS port so it cannot answer for the standard group either.
fn advertiser_without_a_record(
    _instance_id: &str,
    _name: &str,
    _instance_name: &str,
    _node_id: Option<&str>,
    _bind: SocketAddr,
    _version: &str,
) -> anyhow::Result<mdns_sd::ServiceDaemon> {
    daemon_off_the_mdns_port()
}

/// A `ServiceDaemon` on an ephemeral port instead of 5353. Tests need the
/// handle's shutdown behaviour, not its network reach, and the default port
/// would make the mandatory gate contend with the host's own responder.
fn daemon_off_the_mdns_port() -> anyhow::Result<mdns_sd::ServiceDaemon> {
    mdns_sd::ServiceDaemon::new_with_port(0).context("creating mDNS daemon")
}

/// A loopback bind advertises nothing at all — neither protocol — because
/// a discovery record pointing at 127.0.0.1 is a setup button in every
/// client on the LAN that can never connect.
#[tokio::test]
async fn a_loopback_server_starts_no_discovery_at_all() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let config = config_in(tmp.path());
    assert!(
        start_discovery(&config, "instance-1", None, advertiser_that_fails).is_none(),
        "a loopback bind must not be advertised"
    );
}

/// Discovery is best-effort past the loopback check: an advertiser that
/// cannot start leaves the server running and unadvertised rather than
/// failing the boot.
#[test]
fn an_advertiser_that_cannot_start_does_not_stop_the_server() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let mut config = config_in(tmp.path());
    config.server.bind = "0.0.0.0:32400".parse().expect("addr");
    assert!(
        start_bonjour(&config, "instance-1", None, advertiser_that_fails, true).is_none(),
        "a failed advertiser is a warning, not a boot failure"
    );
    // And a loopback server is not offered to the advertiser at all.
    assert!(start_bonjour(&config, "instance-1", None, advertiser_that_fails, false).is_none());
}

/// The whole boot below the ffmpeg probes: an empty data dir becomes a
/// server that answers, and a shutdown drains it back out again.
///
/// Boot is also the only thing that turns "Sign-ins expire" on for an
/// upgraded server: without `auth.token_expiry_since` no token can expire,
/// so the first boot must start that clock, and a later boot on the same
/// data dir must keep the first start rather than restart it.
#[tokio::test]
async fn a_measured_node_boots_serves_and_drains() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let config = config_in(tmp.path());

    let before = http::users::unix_now();
    let first = boot_serve_and_drain(&config, tmp.path()).await;
    let started = first
        .get_setting(keys::AUTH_TOKEN_EXPIRY_SINCE)
        .await
        .expect("read")
        .expect("boot must start the sign-in expiry clock, or nothing ever expires");
    let started: i64 = started.parse().expect("a unix time");
    assert!(
        (before..=http::users::unix_now()).contains(&started),
        "the clock starts at this boot, not {started}"
    );

    // A server that first started long ago: a restart must not move the
    // clock forward, or every restart would postpone every expiry.
    first
        .put_setting(keys::AUTH_TOKEN_EXPIRY_SINCE, "1000")
        .await
        .expect("an earlier first start");
    drop(first);
    let second = boot_serve_and_drain(&config, tmp.path()).await;
    assert_eq!(
        second
            .get_setting(keys::AUTH_TOKEN_EXPIRY_SINCE)
            .await
            .expect("read")
            .as_deref(),
        Some("1000"),
        "a second boot keeps the first start of the clock"
    );
}

/// The Plex façade census (C-07 §8.7) is wired into the daemon, not only
/// into its own module: boot restores it from the data directory before
/// serving, and the drained shutdown writes it once more, marked clean. A
/// boot that dropped the restore leaves no file; one that dropped the
/// final write leaves the startup write's `stopped_cleanly: false`, and the
/// second boot then counts an unclean stop.
#[tokio::test]
async fn a_drained_boot_records_a_clean_plex_census_stop() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let config = config_in(tmp.path());
    let census_path = tmp.path().join(http::plex_census::CENSUS_FILE);
    let read_census = || -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(&census_path).expect("the census file"))
            .expect("census json")
    };

    drop(boot_serve_and_drain(&config, tmp.path()).await);
    let first = read_census();
    assert_eq!(
        first["stopped_cleanly"],
        serde_json::Value::Bool(true),
        "a drained shutdown marks the census stop clean: {first}"
    );
    assert_eq!(first["unclean_stops"], 0);
    let started = first["started_unix_s"].as_u64().expect("a start time");
    assert!(started > 0);

    drop(boot_serve_and_drain(&config, tmp.path()).await);
    let second = read_census();
    assert_eq!(
        second["unclean_stops"], 0,
        "the second boot continued a cleanly stopped census: {second}"
    );
    assert_eq!(second["stopped_cleanly"], serde_json::Value::Bool(true));
    assert_eq!(
        second["started_unix_s"].as_u64(),
        Some(started),
        "one census across both boots"
    );
}

/// Boots `config` over `root`, checks it is serving, shuts it down, and
/// hands back the store it ran on.
async fn boot_serve_and_drain(
    config: &Config,
    root: &std::path::Path,
) -> Arc<dyn plurx_core::store::Store> {
    let config = config.clone();
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        // Each simulated process owns its runtime. Settle its detached
        // tasks and blocking writers before a later boot reopens the file.
        // The SQLite handle owns its connections independently; subsequent
        // inspection runs on the caller's current runtime.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("boot runtime");
        let store = runtime.block_on(boot_serve_and_drain_in_runtime(&config, &root));
        drop(runtime);
        store
    })
    .await
    .expect("boot runtime worker")
}

async fn boot_serve_and_drain_in_runtime(
    config: &Config,
    root: &std::path::Path,
) -> Arc<dyn plurx_core::store::Store> {
    let handle = plurx_core::cluster::open_store(config)
        .await
        .expect("store");
    let store = Arc::clone(&handle.store);
    let catalogue = plurx_core::store::CatalogueReader::authority(Arc::clone(&handle.store));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();

    let booted = tokio::spawn(boot(
        config.clone(),
        Boot {
            source_activation_at_boot: None,
            store: handle.store,
            catalogue,
            replication: plurx_core::cluster::migration::status::ReplicationMonitor::sqlite(),
            membership: plurx_core::cluster::membership::MembershipManager::unavailable(),
            identity: handle.identity,
            credential_key: handle.credential_key,
            backup_client: None,
            dirs: create_dirs(root).expect("dirs"),
            encoder_caps: Default::default(),
            system: Default::default(),
            logs: logbuf::LogBuffers {
                general: Arc::new(logbuf::LogBuffer::new(64)),
                cluster: Arc::new(logbuf::LogBuffer::new(64)),
            },
        },
        advertiser_that_fails,
        async move {
            let _ = stopped.await;
        },
    ));

    // The data dir is laid out and the store is open before anything is
    // served — a boot that answered before that would serve 500s.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(root.join("plurx.db").is_file());
    assert!(root.join("transcode").is_dir());
    assert!(!booted.is_finished(), "the server must still be running");

    stop.send(()).expect("stop");
    tokio::time::timeout(Duration::from_secs(10), booted)
        .await
        .expect("boot must return once shutdown fires")
        .expect("join")
        .expect("an orderly shutdown is exit 0");
    store
}

/// The Dolby Vision conversion is on unless an operator says otherwise,
/// and every spelling of "otherwise" that plurx accepts elsewhere works
/// here too.
///
/// Case matters because compose files are hand-written. `PLURX_DV_CONVERT=OFF`
/// is what someone types when they mean off, and a switch that answered
/// "on" to it would leave a node converting with nothing on the settings
/// page or in the boot log to explain why the variable did nothing.
#[test]
fn the_conversion_switch_reads_every_spelling_of_off() {
    // Default on: a Profile 7 title reaching a Dolby Vision client as
    // HDR10 is what this exists to stop, so it must not need enabling.
    assert!(dv_convert_enabled(None));

    for off in [
        "0", "false", "off", "no", "FALSE", "Off", "NO", " off ", "False",
    ] {
        assert!(!dv_convert_enabled(Some(off)), "{off:?} means off");
    }

    // Everything else is not a request to turn it off, and an unreadable
    // value must not silently disable a feature.
    for on in ["1", "true", "on", "yes", "", "maybe", "0.0"] {
        assert!(dv_convert_enabled(Some(on)), "{on:?} must leave it on");
    }
}

/// Seeding carries an operator's old variable across exactly once, and
/// never over an answer they have since given.
///
/// The second half is the part worth pinning. These settings are
/// replicated and the variables were per-node, so a node whose compose
/// file still says off must not be able to re-answer for the fleet on
/// every restart — that would make turning the switch on in Settings look
/// like it worked and then undo itself at the next deploy.
#[tokio::test]
async fn the_switch_seed_writes_once_and_never_over_an_operator() {
    let store: Arc<dyn plurx_core::store::Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store"));

    // Absent variables seed nothing: the defaults already answer.
    std::env::remove_var("PLURX_PGS_OVERLAY");
    std::env::remove_var("PLURX_DV_CONVERT");
    seed_switch_settings(&store)
        .await
        .expect("seed with no variables");
    assert_eq!(
        store.get_setting(keys::PGS_OVERLAY).await.expect("read"),
        None
    );
    assert_eq!(
        store.get_setting(keys::DV_CONVERT).await.expect("read"),
        None
    );

    // An explicit on carries across; so does an explicit off.
    std::env::set_var("PLURX_PGS_OVERLAY", "On");
    std::env::set_var("PLURX_DV_CONVERT", "off");
    seed_switch_settings(&store)
        .await
        .expect("seed from variables");
    assert_eq!(
        store
            .get_setting(keys::PGS_OVERLAY)
            .await
            .expect("read")
            .as_deref(),
        Some("1"),
        "a hand-written spelling has to carry across, not only `1`"
    );
    assert_eq!(
        store
            .get_setting(keys::DV_CONVERT)
            .await
            .expect("read")
            .as_deref(),
        Some("0")
    );

    // The operator now answers for themselves. A restart with the same
    // stale compose file must not take it back.
    store
        .put_setting(keys::PGS_OVERLAY, "0")
        .await
        .expect("operator turns it off");
    store
        .put_setting(keys::DV_CONVERT, "1")
        .await
        .expect("operator turns it on");
    seed_switch_settings(&store)
        .await
        .expect("seed after an operator answered");
    assert_eq!(
        store
            .get_setting(keys::PGS_OVERLAY)
            .await
            .expect("read")
            .as_deref(),
        Some("0"),
        "a stale variable reverted the operator on the next boot"
    );
    assert_eq!(
        store
            .get_setting(keys::DV_CONVERT)
            .await
            .expect("read")
            .as_deref(),
        Some("1"),
        "a stale variable reverted the operator on the next boot"
    );
    std::env::remove_var("PLURX_PGS_OVERLAY");
    std::env::remove_var("PLURX_DV_CONVERT");
}

/// A node that was never told either way must not claim the key for the
/// fleet. The conversion defaults on, so an unset or on variable already
/// answers the way the absent setting answers.
#[tokio::test]
async fn an_unconfigured_node_does_not_claim_the_conversion_key() {
    let store: Arc<dyn plurx_core::store::Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store"));
    std::env::remove_var("PLURX_PGS_OVERLAY");
    std::env::set_var("PLURX_DV_CONVERT", "1");
    seed_switch_settings(&store).await.expect("seed");
    assert_eq!(
        store.get_setting(keys::DV_CONVERT).await.expect("read"),
        None,
        "writing the default down makes a stored value the operator never chose, \
             and on a cluster lets a node that was never configured answer first"
    );
    std::env::remove_var("PLURX_DV_CONVERT");
}

/// The settings page and every session read this record, so the encoder it
/// names has to be the one the probe actually chose.
#[test]
fn the_system_record_reports_the_encoder_the_probe_selected() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let caps = plurx_core::transcode::EncoderCaps::default();
    let selected = caps.choose("").label().to_owned();
    let system = system_info(
        &config_in(tmp.path()),
        "/opt/jellyfin-ffmpeg/ffmpeg".to_owned(),
        "/opt/jellyfin-ffmpeg/ffprobe".to_owned(),
        "auto".to_owned(),
        caps.clone(),
        Measured {
            ffmpeg_version: Some("ffmpeg version 7.1.1".to_owned()),
            ffprobe_build_digest: Some("a".repeat(64)),
            decode_probe_identity: None,
            measured_decoders:
                plurx_core::transcode::decoder_inventory::MeasuredDecoders::from_measured(&[
                    (
                        "h264",
                        plurx_core::transcode::DecodeBackend::Software,
                        "h264",
                    ),
                    (
                        "h264",
                        plurx_core::transcode::DecodeBackend::VideoToolbox,
                        "h264",
                    ),
                ]),
            pacing: crate::ffmpeg::PacingCaps {
                readrate: true,
                initial_burst: true,
            },
            dovi_rpu: true,
            dovi_reshape: true,
            dovi_passthrough: true,
            dovi_passthrough_qsv: true,
            hdr10_passthrough: true,
            hdr10_passthrough_qsv: true,
            hdr10_passthrough_vaapi: true,
            encoder_selected: selected.clone(),
            decoders: vec!["h264".to_owned(), "hevc".to_owned()],
            tone_map: pipeprobe::PipelineReport::cpu_only("not probed"),
            dv_disk: crate::dv_disk::DvDiskCapabilities::default(),
        },
    );

    assert_eq!(system.data_dir, tmp.path().display().to_string());
    assert_eq!(system.ffmpeg, "/opt/jellyfin-ffmpeg/ffmpeg");
    assert_eq!(system.encoder_selected, selected);
    assert_eq!(system.hwaccel_pref, "auto");
    // The record has to carry the switch's answer, not a literal. A test
    // suite that only covers `dv_convert_enabled` leaves the wire between
    // it and this record uncovered: replacing the call with `true` would
    // pass everything else and quietly ignore the operator.
    assert_eq!(
        system.dolby_vision_convert,
        dv_convert_enabled(std::env::var("PLURX_DV_CONVERT").ok().as_deref()),
        "the reported conversion state is the one the switch decides"
    );
    assert!(system.dovi_rpu);
    assert!(system.dovi_reshape);
    assert!(system.pacing.readrate);
    // The tone-map answer a session consults is the one that was measured.
    assert_eq!(
        system.tone_map.selected(),
        plurx_core::transcode::Pipeline::Cpu
    );
    let wire = serde_json::to_value(&system).expect("serialize /api/v1/system info");
    assert_eq!(
        wire["measured_decoders"]["by_codec"]["h264"], "h264",
        "the v1 field keeps string values for existing API consumers"
    );
    assert_eq!(
        wire["measured_decoders"]["by_codec_and_backend_v2"]["h264"]["videotoolbox"], "h264",
        "the versioned sibling carries the backend-aware path"
    );
}

/// The companion publishes the identity the server reported, at the port
/// the host published it on — and a registration it cannot make is an
/// error rather than a silent success.
#[tokio::test]
async fn the_companion_advertises_at_the_published_port() {
    let (base_url, server) = identity_server(SERVER_IDENTITY).await;
    let port = base_url
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .expect("port");

    // A fn pointer cannot capture, so the record lands in a static the
    // test reads back.
    static SEEN: std::sync::Mutex<Vec<Advertised>> = std::sync::Mutex::new(Vec::new());
    fn record(
        instance_id: &str,
        name: &str,
        instance_name: &str,
        node_id: Option<&str>,
        bind: SocketAddr,
        version: &str,
    ) -> anyhow::Result<mdns_sd::ServiceDaemon> {
        SEEN.lock().expect("lock").push(Advertised {
            instance_id: instance_id.to_owned(),
            name: name.to_owned(),
            instance_name: instance_name.to_owned(),
            node_id: node_id.map(str::to_owned),
            port: bind.port(),
            version: version.to_owned(),
        });
        anyhow::bail!("no mDNS daemon in this test")
    }

    let error = format!(
        "{:#}",
        advertise_with(
            &base_url,
            record,
            Duration::from_millis(10),
            std::future::pending(),
        )
        .await
        .expect_err("the stub advertiser refuses")
    );
    assert!(error.contains("no mDNS daemon"), "{error}");

    let seen = SEEN.lock().expect("lock");
    assert_eq!(seen.len(), 1, "exactly one registration was attempted");
    let record = &seen[0];
    assert_eq!(
        record.instance_id, "abc",
        "the server's own identity, not a new one"
    );
    assert_eq!(record.version, "0.2.0");
    assert_eq!(
        record.port, port,
        "the host's published port must be advertised, not the container's"
    );
    assert!(record.name.contains("Living Room"), "{}", record.name);
    // The label a picker shows is the one this machine computes for
    // itself. Reverting the companion to publishing `info.name` for a
    // cluster node — the 2026-08-20 regression — fails here on any host
    // with a hostname or a LAN address.
    // This fixture is a standalone server, so no node id is published
    // and none may appear in the label.
    assert_eq!(record.node_id, None);
    assert_eq!(
        record.instance_name,
        expected_instance_name("Living Room", None)
    );
    drop(seen);
    server.abort();
}

/// The companion path for a cluster node — the path this whole naming
/// change exists for, and the one nothing exercised: `advertise_with`
/// must pass the node's own id through (it names the per-node host record
/// and rides in TXT) while publishing the label this machine computed for
/// itself, not the `server.name` every node of the cluster reports.
#[tokio::test]
async fn the_companion_advertises_a_cluster_node_as_its_own_machine() {
    static SEEN: std::sync::Mutex<Vec<Advertised>> = std::sync::Mutex::new(Vec::new());
    fn record(
        instance_id: &str,
        name: &str,
        instance_name: &str,
        node_id: Option<&str>,
        bind: SocketAddr,
        version: &str,
    ) -> anyhow::Result<mdns_sd::ServiceDaemon> {
        SEEN.lock().expect("lock").push(Advertised {
            instance_id: instance_id.to_owned(),
            name: name.to_owned(),
            instance_name: instance_name.to_owned(),
            node_id: node_id.map(str::to_owned),
            port: bind.port(),
            version: version.to_owned(),
        });
        anyhow::bail!("no mDNS daemon in this test")
    }

    let (base_url, server) = identity_server(CLUSTERED_IDENTITY).await;
    SEEN.lock().expect("lock").clear();
    let error = format!(
        "{:#}",
        advertise_with(
            &base_url,
            record,
            Duration::from_millis(10),
            std::future::pending(),
        )
        .await
        .expect_err("the stub advertiser refuses")
    );
    assert!(error.contains("no mDNS daemon"), "{error}");

    let seen = SEEN.lock().expect("lock");
    assert_eq!(seen.len(), 1, "exactly one registration was attempted");
    let record = &seen[0];
    assert_eq!(
        record.instance_id, "logical-server",
        "the logical server clients bind to"
    );
    assert_eq!(
        record.node_id.as_deref(),
        Some("node-b"),
        "a cluster node must still be addressed as itself: dropping this \
             collapses every node's host record onto the logical id"
    );
    assert_eq!(
        record.instance_name,
        expected_instance_name("Living Room", Some("node-b"))
    );
    drop(seen);
    server.abort();
}

/// A companion that did register withdraws its record when it is told to
/// stop, and reports that as a clean exit.
///
/// The whole tail past a successful registration: without it a `docker
/// stop` of the companion would leave a Bonjour record on the LAN pointing
/// at a server nothing is answering for, and every client on the network
/// would keep offering it until the record aged out. Stop is only sent
/// once the registration has actually happened — signalling earlier exits
/// through the "never reached a server" path instead, which is the case
/// the test below owns.
#[tokio::test]
async fn a_registered_companion_withdraws_its_record_on_shutdown() {
    static REGISTERED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    // The shared no-record advertiser, counted so the stop below is sent
    // only once the registration has actually happened.
    fn daemon_without_a_record(
        instance_id: &str,
        name: &str,
        instance_name: &str,
        node_id: Option<&str>,
        bind: SocketAddr,
        version: &str,
    ) -> anyhow::Result<mdns_sd::ServiceDaemon> {
        let daemon =
            advertiser_without_a_record(instance_id, name, instance_name, node_id, bind, version)?;
        REGISTERED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(daemon)
    }

    let (base_url, server) = identity_server(SERVER_IDENTITY).await;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let companion = tokio::spawn(async move {
        advertise_with(
            &base_url,
            daemon_without_a_record,
            Duration::from_millis(10),
            async {
                let _ = stopped.await;
            },
        )
        .await
    });

    // Only ask it to stop once it is past the registration, so this
    // exercises the shutdown tail rather than the early return.
    tokio::time::timeout(Duration::from_secs(5), async {
        while REGISTERED.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the companion must register before it is asked to stop");

    stop.send(()).expect("the companion is still listening");
    let outcome = tokio::time::timeout(Duration::from_secs(5), companion)
        .await
        .expect("a stop must end the companion")
        .expect("join");
    outcome.expect("withdrawing a record it registered is a clean exit");
    assert_eq!(
        REGISTERED.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the identity is fetched and advertised exactly once"
    );
    server.abort();
}

/// A companion asked to stop before the server ever appeared exits cleanly
/// rather than advertising something it never reached.
#[tokio::test]
async fn a_companion_that_never_reaches_a_server_exits_without_advertising() {
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = stop.send(());
    });
    advertise_with(
        "http://127.0.0.1:1",
        advertiser_that_fails,
        Duration::from_millis(20),
        async move {
            let _ = stopped.await;
        },
    )
    .await
    .expect("a shutdown while waiting is not a failure");
}

/// The responder can take the port and the group. Multicast is TTL-scoped
/// to the local network, which is what keeps GDM from becoming the
/// reflection amplifier the protocol has been abused as.
#[tokio::test]
async fn the_gdm_responder_binds_its_port_and_joins_the_search_group() {
    let socket = gdm_socket(0).await.expect("an ephemeral GDM socket");
    let addr = socket.local_addr().expect("addr");
    assert_ne!(addr.port(), 0, "the socket must really be bound");
    assert!(addr.ip().is_unspecified(), "GDM listens on every interface");

    // And a port already held is refused rather than silently shared.
    let taken = gdm_socket(addr.port()).await;
    assert!(
        taken.is_err(),
        "two responders on one port would answer each other's searches"
    );
}

/// A free UDP port, released before it is handed back. Racy in principle
/// and stable in practice; the alternative is hard-coding 32414 and
/// fighting whatever else on the machine wants it.
fn free_udp_port() -> u16 {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").expect("bind");
    probe.local_addr().expect("addr").port()
}

/// A LAN-bound server answers Plex clients' searches with its own name and
/// HTTP port. This is the wiring `plurxd` does at boot, driven end to end:
/// the responder is spawned, it binds, and it replies.
///
/// One test rather than several because `PLURX_GDM_PORT` is process-wide.
#[tokio::test]
async fn a_lan_server_answers_plex_searches_on_the_configured_port() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let mut config = config_in(tmp.path());
    config.server.name = "Loft".to_owned();
    config.server.bind = "0.0.0.0:32400".parse().expect("addr");

    // Unset, a LAN bind answers on the protocol's own port. Asserted here
    // rather than beside the loopback case because this test is the one
    // that owns the variable for the length of its run.
    std::env::remove_var("PLURX_GDM_PORT");
    assert_eq!(
        gdm_responder_port(true, config.server.bind),
        Some(plurx_compat_plex::gdm::GDM_PORT)
    );

    // An unparseable override disables the responder rather than falling
    // back to a port the operator did not ask for.
    std::env::set_var("PLURX_GDM_PORT", "not-a-port");
    assert_eq!(gdm_responder_port(true, config.server.bind), None);

    let port = free_udp_port();
    std::env::set_var("PLURX_GDM_PORT", port.to_string());
    assert_eq!(gdm_responder_port(true, config.server.bind), Some(port));

    // The full boot-time wiring: discovery starts the responder, and the
    // stub advertiser keeps Bonjour off the network.
    assert!(start_discovery(&config, "instance-1", None, advertiser_that_fails).is_none());

    let client = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("client");
    client
        .connect(("127.0.0.1", port))
        .await
        .expect("connect to the responder");

    let mut buf = [0u8; 2048];
    let mut reply = None;
    // The responder is spawned, so the first datagram can outrun its bind.
    for _ in 0..40 {
        client
            .send(b"M-SEARCH * HTTP/1.0\r\n\r\n")
            .await
            .expect("search");
        if let Ok(Ok(n)) =
            tokio::time::timeout(Duration::from_millis(100), client.recv(&mut buf)).await
        {
            reply = Some(String::from_utf8_lossy(&buf[..n]).into_owned());
            break;
        }
    }
    let reply = reply.expect("the GDM responder must answer a search");
    assert!(reply.contains("Name: Loft"), "{reply}");
    assert!(reply.contains("Port: 32400"), "{reply}");
    assert!(reply.contains("Resource-Identifier: instance-1"), "{reply}");
    std::env::remove_var("PLURX_GDM_PORT");
}

/// Compose deployments turn the in-process advertiser off and run
/// `plurxd advertise` in a host-network companion, because multicast
/// cannot cross a bridge. One test, because the variable is process-wide.
#[test]
fn compose_can_turn_the_in_process_advertiser_off() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let mut config = config_in(tmp.path());
    config.server.bind = "0.0.0.0:32400".parse().expect("addr");

    assert!(mdns_advertising_enabled(), "on unless told otherwise");
    // Enabled, the daemon a successful advertiser produced reaches the
    // caller: `serve` is what withdraws it on drain, so a `start_bonjour`
    // that dropped it here would leave the record on the LAN for as long
    // as it took to age out. Asserted in this test because it is the one
    // that owns `PLURX_MDNS_ADVERTISE` for the length of its run.
    let daemon = start_bonjour(
        &config,
        "instance-1",
        None,
        advertiser_without_a_record,
        true,
    )
    .expect("an enabled, successful advertiser must yield its daemon");
    daemon.shutdown().expect("the returned daemon must be live");

    std::env::set_var("PLURX_MDNS_ADVERTISE", "0");
    assert!(!mdns_advertising_enabled());
    assert!(
        start_bonjour(&config, "instance-1", None, advertiser_that_fails, true).is_none(),
        "a disabled advertiser is never even started"
    );
    std::env::remove_var("PLURX_MDNS_ADVERTISE");
    assert!(mdns_advertising_enabled());
}

/// Run a closure with every event it emits captured, and hand back the log.
///
/// A `tracing` macro only evaluates its fields when a subscriber wants
/// them, so a test with no subscriber is not exercising the log line it
/// thinks it is. Reuses the buffer layer the settings page reads.
fn captured(body: impl FnOnce()) -> Arc<logbuf::LogBuffer> {
    use tracing_subscriber::prelude::*;
    let logs = Arc::new(logbuf::LogBuffer::new(64));
    let subscriber = tracing_subscriber::registry().with(logbuf::BufferLayer(Arc::clone(&logs)));
    tracing::subscriber::with_default(subscriber, body);
    logs
}

/// Both numbers, or the line cannot answer the question it exists for.
///
/// After an `EMFILE` the only thing worth knowing is which limit the
/// daemon was actually running under, and "soft 1024" without the hard
/// limit beside it does not say whether raising the unit's `LimitNOFILE`
/// would have helped. Synthetic values: the raise itself is process-wide
/// and is proved in `plurx_core::process::rlimit`.
#[test]
fn the_open_file_limit_is_logged_with_both_values() {
    let logs = captured(|| {
        report_open_file_limit(Ok(Some(plurx_core::process::rlimit::OpenFileLimit {
            soft_before: 256,
            soft_after: 524_288,
            hard: 524_288,
        })));
    });
    let messages = logs
        .tail("info", 8)
        .into_iter()
        .map(|entry| entry.message)
        .collect::<Vec<_>>();
    assert_eq!(
        messages,
        vec!["open files: soft 256 -> 524288 (hard 524288)".to_owned()]
    );
}

/// A resource limit is not a reason to refuse to serve.
#[test]
fn an_open_file_limit_that_cannot_be_raised_is_only_a_warning() {
    let logs = captured(|| {
        report_open_file_limit(Err(std::io::Error::other("refused")));
    });
    let entries = logs.tail("trace", 8);
    let [entry] = &entries[..] else {
        panic!("expected exactly one event, got {}", entries.len());
    };
    assert_eq!(entry.level, "WARN");
    assert!(
        entry.message.contains("continuing on the inherited limit"),
        "unexpected message: {}",
        entry.message
    );
}

/// The async counterpart: capture on this thread until the guard drops.
///
/// `#[tokio::test]` runs a current-thread runtime, so the tasks the code
/// under test spawns are polled on this thread too and their events land in
/// the same buffer. Attaching the subscriber to one future would not reach
/// them, and a warning from a spawned task is exactly what a best-effort
/// subsystem reports itself with.
fn capturing(logs: &Arc<logbuf::LogBuffer>) -> tracing::subscriber::DefaultGuard {
    use tracing_subscriber::prelude::*;
    tracing::subscriber::set_default(
        tracing_subscriber::registry().with(logbuf::BufferLayer(Arc::clone(logs))),
    )
}

/// A daemon that records what it was asked to publish and can be told to
/// refuse either call. Nothing here opens a socket, which is what keeps
/// the mandatory unit and coverage gates off the local network.
#[derive(Default)]
struct FakeDaemon {
    monitor_refuses: bool,
    register_refuses: bool,
    events: std::sync::Mutex<Vec<mdns_sd::DaemonEvent>>,
    registered: std::sync::Mutex<Vec<mdns_sd::ServiceInfo>>,
}

impl MdnsRegistrar for FakeDaemon {
    fn monitor(&self) -> anyhow::Result<Box<dyn Iterator<Item = mdns_sd::DaemonEvent> + Send>> {
        if self.monitor_refuses {
            anyhow::bail!("monitoring mDNS daemon");
        }
        let events = std::mem::take(&mut *self.events.lock().expect("lock"));
        Ok(Box::new(events.into_iter()))
    }

    fn register(&self, service: mdns_sd::ServiceInfo) -> anyhow::Result<()> {
        if self.register_refuses {
            anyhow::bail!("registering plurx Bonjour service");
        }
        self.registered.lock().expect("lock").push(service);
        Ok(())
    }
}

/// The whole of what a successful registration publishes: the record every
/// client on the LAN discovers this server by. A wrong port or a missing
/// identity property here is a server nothing can connect to, and the log
/// line is how an operator confirms which port went out.
#[test]
fn a_registration_publishes_the_record_clients_discover() {
    let daemon = FakeDaemon::default();
    let logs = captured(|| {
        register_advertiser(
            &daemon,
            "plurxd-selftest-instance",
            "Living Room",
            "Living Room",
            None,
            "10.42.1.9:32400".parse().expect("addr"),
            "0.2.0",
        )
        .expect("a daemon that accepts both calls registers");
    });

    let registered = daemon.registered.lock().expect("lock");
    assert_eq!(registered.len(), 1, "exactly one record is published");
    let service = &registered[0];
    assert_eq!(service.get_type(), MDNS_SERVICE_TYPE);
    assert_eq!(service.get_port(), 32400, "the bind's port, not a default");
    assert!(
        service.get_fullname().contains("Living Room"),
        "{}",
        service.get_fullname()
    );

    let announced = logs.tail("info", 8);
    let line = announced
        .iter()
        .find(|entry| {
            entry
                .message
                .contains("Bonjour discovery advertiser registered")
        })
        .unwrap_or_else(|| panic!("a registration must be logged: {announced:?}"));
    assert!(
        line.message.contains("port=32400"),
        "the log must name the port that was published: {}",
        line.message
    );
}

/// A record that cannot be built is the third way a registration fails,
/// and the one an operator can cause by typing: a server name past the
/// 255-byte TXT limit. It has to surface as an error the boot carries on
/// from, never a panic, and nothing may be published for it.
#[test]
fn a_server_name_too_long_to_advertise_is_an_error_not_a_panic() {
    let daemon = FakeDaemon::default();
    let error = format!(
        "{:#}",
        register_advertiser(
            &daemon,
            "instance-1",
            &"Living Room ".repeat(30),
            &"Living Room ".repeat(30),
            None,
            "10.42.1.9:32400".parse().expect("addr"),
            "0.2.0",
        )
        .expect_err("a record that cannot be built is an error")
    );
    assert!(error.contains("255-byte limit"), "{error}");
    assert!(
        daemon.registered.lock().expect("lock").is_empty(),
        "nothing may be published for a record that was never built"
    );
}

/// The real daemon's monitor hands back a stream that ends when the daemon
/// does. That is what lets the monitor thread exit with the server instead
/// of outliving it; nothing is registered, so nothing reaches the network.
#[test]
fn a_real_daemons_monitor_ends_with_the_daemon() {
    let daemon = daemon_off_the_mdns_port().expect("mDNS daemon");
    let mut events = MdnsRegistrar::monitor(&daemon).expect("a live daemon can be monitored");
    daemon.shutdown().expect("withdrawal");

    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // Drains whatever the shutdown itself emitted, then ends.
        while events.next().is_some() {}
        let _ = done.send(());
    });
    ended
        .recv_timeout(Duration::from_secs(5))
        .expect("a daemon that is gone must end its event stream");
}

/// A GDM port the kernel will not give up is the ordinary case on a box
/// already running Plex. It must leave the server running and say so —
/// discovery is best-effort, and a warning is the only trace there is.
#[tokio::test]
async fn a_gdm_port_that_cannot_be_bound_is_a_warning_not_a_failure() {
    // Hold the port first, so the responder's bind is refused. Loopback
    // UDP only: this never joins a multicast group.
    let held = tokio::net::UdpSocket::bind("0.0.0.0:0")
        .await
        .expect("holding a port");
    let taken = held.local_addr().expect("addr").port();

    let tmp = crate::test_tempdir().expect("tempdir");
    let config = config_in(tmp.path());
    let logs = Arc::new(logbuf::LogBuffer::new(64));
    let _capture = capturing(&logs);
    // The responder is spawned, not awaited — a bind that fails must not be
    // something the caller waits on.
    spawn_gdm_responder(&config, "instance-1".to_owned(), None, taken);
    for _ in 0..50 {
        if !logs.tail("warn", 8).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let warned = logs.tail("warn", 8);
    let line = warned
        .iter()
        .find(|entry| {
            entry
                .message
                .contains("GDM discovery responder unavailable")
        })
        .unwrap_or_else(|| panic!("a refused GDM bind must be logged: {warned:?}"));
    assert!(
        line.message.contains("binding GDM port"),
        "the operator needs the reason, not just the symptom: {}",
        line.message
    );
}

/// The one line an operator reads to confirm which server, which node, and
/// which data directory this process actually came up on. A cluster makes
/// the two identities easy to confuse, so both have to be in it.
#[test]
fn the_startup_line_names_both_identities_and_the_data_dir() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let config = config_in(tmp.path());
    let identity = plurx_core::cluster::ClusterIdentity {
        cluster_id: "cluster-abc".to_owned(),
        node_id: "node-2".to_owned(),
        raft_id: 1,
    };

    let logs = captured(|| log_startup(&config, &identity));
    let started = logs.tail("info", 8);
    let line = started
        .iter()
        .find(|entry| entry.message.contains("plurxd starting"))
        .unwrap_or_else(|| panic!("startup must be logged: {started:?}"));
    for expected in [
        "instance_id=cluster-abc",
        "node_id=node-2",
        crate::version::SEMVER,
        &tmp.path().display().to_string(),
    ] {
        assert!(
            line.message.contains(expected),
            "the startup line must name {expected}: {}",
            line.message
        );
    }
}

/// Monitoring is established before the record exists. A daemon that
/// cannot be monitored publishes nothing at all rather than putting a
/// record on the network no failure would ever be reported for.
#[test]
fn a_daemon_that_cannot_be_monitored_publishes_nothing() {
    let daemon = FakeDaemon {
        monitor_refuses: true,
        ..Default::default()
    };
    let error = format!(
        "{:#}",
        register_advertiser(
            &daemon,
            "instance-1",
            "Living Room",
            "Living Room",
            None,
            "10.42.1.9:32400".parse().expect("addr"),
            "0.2.0",
        )
        .expect_err("an unmonitorable daemon is an error")
    );
    assert!(error.contains("monitoring mDNS daemon"), "{error}");
    assert!(
        daemon.registered.lock().expect("lock").is_empty(),
        "a record must not be published before its monitor exists"
    );
}

/// A refused registration reports why. `start_bonjour` logs this and
/// carries on unadvertised — discovery is best-effort, so it must be an
/// error the caller can read, never a panic or a silent success.
#[test]
fn a_refused_registration_reports_why() {
    let daemon = FakeDaemon {
        register_refuses: true,
        ..Default::default()
    };
    let error = format!(
        "{:#}",
        register_advertiser(
            &daemon,
            "instance-1",
            "Living Room",
            "Living Room",
            None,
            "10.42.1.9:32400".parse().expect("addr"),
            "0.2.0",
        )
        .expect_err("a daemon that refuses the record is an error")
    );
    assert!(
        error.contains("registering plurx Bonjour service"),
        "{error}"
    );
}

/// The monitor is the only place a discovery failure can surface, because
/// `mdns-sd` opens its sockets after `register` has returned. It logs the
/// errors, ignores the ordinary traffic, and ends when the daemon does.
#[test]
fn the_monitor_logs_daemon_errors_and_nothing_else() {
    let logs = captured(|| {
        log_mdns_events(
            vec![
                mdns_sd::DaemonEvent::Announce("plurx".to_owned(), "10.42.1.9:32400".to_owned()),
                mdns_sd::DaemonEvent::Error(mdns_sd::Error::Msg(
                    "no usable multicast interface".to_owned(),
                )),
            ]
            .into_iter(),
        );
    });

    let warned = logs.tail("warn", 16);
    assert_eq!(warned.len(), 1, "an announce is not a failure: {warned:?}");
    assert!(
        warned[0].message.contains("no usable multicast interface"),
        "the daemon's own reason must reach the operator: {}",
        warned[0].message
    );
}

/// A daemon whose events have run out ends the monitor thread rather than
/// leaving one parked for the life of the process.
#[test]
fn the_monitor_ends_when_the_daemon_does() {
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        log_mdns_events(std::iter::empty());
        let _ = done.send(());
    });
    ended
        .recv_timeout(Duration::from_secs(5))
        .expect("an exhausted event stream must end the monitor, not park it");
}

/// SIGTERM is what `docker stop` sends, and it has to start the drain
/// rather than kill the process mid-stream.
#[cfg(unix)]
#[tokio::test]
async fn sigterm_starts_the_drain() {
    // Registering our own stream first replaces the default terminate
    // action process-wide, so raising the signal below cannot kill the
    // test runner even if the task under test has not been polled yet.
    let mut guard = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM stream");
    let waiting = tokio::spawn(shutdown_signal());
    tokio::task::yield_now().await;

    // SAFETY: raising a signal this process has a handler installed for.
    assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
    guard.recv().await;

    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("SIGTERM must end the wait")
        .expect("join");
}

/// A drain is bounded. A paced remux holds its connection open for a
/// quarter of the film's runtime, so waiting for every request to finish
/// means never finishing — `docker stop` would spend its grace period
/// achieving nothing and then SIGKILL, which surfaces as exit 137.
#[tokio::test]
async fn a_connection_that_never_finishes_does_not_hold_the_drain_open() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let state = booted_state(tmp.path());
    let progress = Arc::clone(&state.progress);
    // A route that never responds, standing in for a paced remux.
    let app = axum::Router::new().route(
        "/forever",
        axum::routing::get(std::future::pending::<&'static str>),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(serve(
        listener,
        app,
        JellyfinStandardListener::default(),
        https::Serving::default(),
        progress,
        None,
        async move {
            let _ = stopped.await;
        },
    ));

    // Hold a request open, then ask the server to stop.
    let hanging = tokio::spawn(async move {
        let _ = reqwest::Client::new()
            .get(format!("http://{addr}/forever"))
            .send()
            .await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop.send(()).expect("stop");

    let outcome = tokio::time::timeout(
        SHUTDOWN_DRAIN_TIMEOUT + PROGRESS_DRAIN_TIMEOUT + Duration::from_secs(5),
        served,
    )
    .await
    .expect("the drain must be bounded, not indefinite")
    .expect("join");
    // Still exit 0, but reported as timed out: the Plex census must not
    // mark a stop clean while this connection could still be counted.
    assert_eq!(
        outcome.expect("a timed-out drain is still an orderly exit 0"),
        HttpDrain::TimedOut
    );
    hanging.abort();
}

/// A service instance with no name is still a service. An empty label
/// would be a DNS-SD registration failure rather than an unnamed server.
#[test]
fn an_unnamed_server_still_registers_under_something() {
    let info = mdns_service_info(
        "",
        "   ",
        "   ",
        None,
        "0.0.0.0:32400".parse().expect("socket address"),
        "0.2.0",
    )
    .expect("service info");
    assert_eq!(info.get_property_val_str("name"), Some("plurx"));
    // With no instance id to derive from, the host label falls back too.
    assert_eq!(info.get_hostname(), "plurx-server.local.");
}

/// A DNS-SD service instance is one DNS label of 63 bytes. A custom name
/// long enough to overflow it keeps the name and drops the address —
/// a registration failure would be worse than a less specific label.
#[test]
fn a_name_too_long_for_dns_sd_keeps_the_name_and_drops_the_address() {
    let address: IpAddr = "10.42.100.200".parse().expect("IP");
    let long = "L".repeat(60);
    assert_eq!(
        display_name(&long, Some("lab6"), Some(&address)),
        long,
        "the address must be dropped rather than overflow the label"
    );
    // One that still fits keeps both.
    assert_eq!(
        display_name("Loft", Some("lab6"), Some(&address)),
        "Loft · 10.42.100.200"
    );
}

/// The companion's whole loop is "ask again in two seconds". A request
/// with no timeout against a half-open connection would hang that loop
/// forever and the server would never be advertised.
#[tokio::test]
async fn the_companion_client_gives_up_on_a_silent_server() {
    let client = discovery_client().expect("client");
    // A socket that accepts and then says nothing: without a timeout this
    // request never returns.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let held = std::thread::spawn(move || {
        let accepted = listener.accept();
        std::thread::sleep(Duration::from_secs(8));
        drop(accepted);
    });

    let started = std::time::Instant::now();
    let outcome = client
        .get(format!("http://{addr}/api/v1/server"))
        .send()
        .await;
    assert!(outcome.is_err(), "a silent server must not hang the loop");
    let waited = started.elapsed();
    assert!(waited < Duration::from_secs(7), "it waited {waited:?}");
    held.join().expect("thread");
}

/// `plurxd advertise` refuses a URL it could never reach, rather than
/// publishing a Bonjour record pointing at nothing.
#[tokio::test]
async fn the_advertise_subcommand_refuses_an_unreachable_url() {
    let tmp = crate::test_tempdir().expect("tempdir");
    let error = format!(
        "{:#}",
        dispatch(
            Command::Advertise {
                server: "ftp://10.42.1.9:21".to_owned(),
            },
            config_in(tmp.path()),
            None,
        )
        .await
        .expect_err("not an HTTP server")
    );
    assert!(error.contains("http or https"), "{error}");
}

/// These read the machine rather than a fixture, so what is asserted is the
/// contract every caller depends on: a usable label or nothing at all.
/// A blank or loopback answer here becomes a discovery record no client can
/// use.
#[test]
fn the_machine_identity_is_usable_or_absent_but_never_blank() {
    if let Some(host) = system_hostname() {
        assert!(!host.trim().is_empty());
        assert!(!host.eq_ignore_ascii_case("localhost"));
        assert!(!host.ends_with(".local"));
    }
    if let Some(address) = primary_lan_address() {
        assert!(!address.is_loopback(), "{address}");
        assert!(!address.is_unspecified(), "{address}");
    }
}

#[test]
fn refresh_metadata_can_target_one_library() {
    let cli = Cli::try_parse_from(["plurxd", "refresh-metadata", "--library", "7"])
        .expect("refresh command");
    assert!(matches!(
        cli.command,
        Some(Command::RefreshMetadata { library: Some(7) })
    ));
}

#[tokio::test]
async fn sharing_tls_command_refuses_to_replace_an_existing_node_key() {
    let dir = tempfile::tempdir().expect("synthetic node key directory");
    // NodeTls refuses a group- or world-writable key directory, and a
    // temporary directory inherits the runner umask (002 on some hosts).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("private synthetic node key directory");
    }
    let command = || SharingCommand::InitTls {
        key_directory: dir.path().to_path_buf(),
    };
    let cli = Cli::try_parse_from([
        "plurxd",
        "sharing",
        "init-tls",
        "--key-directory",
        dir.path().to_str().expect("synthetic UTF-8 path"),
    ])
    .expect("sharing provisioning command");
    assert!(matches!(cli.command, Some(Command::Sharing { .. })));
    dispatch(
        Command::Sharing { command: command() },
        Config::default(),
        None,
    )
    .await
    .expect("provision synthetic TLS identity");
    let original =
        std::fs::read(dir.path().join("sharing-tls.der")).expect("synthetic certificate");
    assert!(dispatch(
        Command::Sharing { command: command() },
        Config::default(),
        None
    )
    .await
    .is_err());
    assert_eq!(
        std::fs::read(dir.path().join("sharing-tls.der")).expect("retained certificate"),
        original
    );
}

#[test]
fn cluster_and_wal_commands_keep_credentials_and_mutation_out_of_argv() {
    let cluster = Cli::try_parse_from([
        "plurxd",
        "cluster",
        "status",
        "--server",
        "https://node-a.example",
        "--token-file",
        "/run/secrets/plurx-admin",
        "--json",
    ])
    .expect("cluster status command");
    assert!(matches!(
        cluster.command,
        Some(Command::Cluster {
            command: ClusterCommand::Status { json: true, .. }
        })
    ));
    assert!(Cli::try_parse_from(["plurxd", "cluster", "status", "--token", "secret"]).is_err());

    let wal = Cli::try_parse_from([
        "plurxd",
        "wal",
        "recovery-plan",
        "--data-dir",
        "/srv/plurx",
        "--json",
    ])
    .expect("WAL recovery plan command");
    assert!(matches!(wal.command, Some(Command::Wal { .. })));
    assert!(Cli::try_parse_from(["plurxd", "wal", "apply-plan"]).is_err());
}

#[cfg(unix)]
#[test]
fn cluster_token_reader_requires_the_opened_owner_only_regular_file() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let root = crate::test_tempdir().expect("token fixture");
    let token = root.path().join("admin-token");
    std::fs::write(&token, "fixture-token\n").expect("seed token");
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600))
        .expect("make token owner-only");
    assert_eq!(
        read_owner_only_token(Some(&token)).expect("read owner-only token"),
        "fixture-token"
    );

    let link = root.path().join("token-link");
    symlink(&token, &link).expect("seed symlink");
    assert!(read_owner_only_token(Some(&link)).is_err());

    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o640))
        .expect("make token group-readable");
    assert!(read_owner_only_token(Some(&token)).is_err());
}

#[test]
fn cluster_node_ids_are_encoded_as_one_path_segment() {
    assert_eq!(percent_encode_path_segment("node-a"), "node-a");
    assert_eq!(percent_encode_path_segment("node/a b"), "node%2Fa%20b");
}

#[test]
fn gdm_port_defaults_and_accepts_an_override() {
    assert_eq!(
        gdm_bind_port(None).expect("default GDM port"),
        plurx_compat_plex::gdm::GDM_PORT
    );
    assert_eq!(
        gdm_bind_port(Some("32415")).expect("GDM port override"),
        32415
    );
    assert!(gdm_bind_port(Some("not-a-port")).is_err());
}

#[test]
fn compose_can_disable_only_the_in_process_advertiser() {
    for value in ["0", "false", "FALSE", " no ", "off"] {
        assert!(!mdns_advertising_value_enabled(value), "{value:?}");
    }
    for value in ["1", "true", "yes", "on", "anything-else"] {
        assert!(mdns_advertising_value_enabled(value), "{value:?}");
    }
}

#[test]
fn discovery_companion_decodes_the_public_server_identity() {
    let info: DiscoveryServerInfo = serde_json::from_str(
        r#"{
                "name": "Living Room",
                "version": "0.2.0",
                "build": "v0.2.0-4-gabc",
                "instance_id": "server-id",
                "node_id": "node-b",
                "cluster_advertisement": true,
                "uptime_seconds": 12,
                "setup_required": false,
                "android_app": true
            }"#,
    )
    .expect("server identity");

    assert_eq!(info.name, "Living Room");
    assert_eq!(info.version, "0.2.0");
    assert_eq!(info.instance_id, "server-id");
    assert_eq!(info.node_id, "node-b");
    assert!(info.cluster_advertisement);
}

#[test]
fn default_discovery_name_uses_the_machine_but_custom_names_win() {
    let address: IpAddr = "10.42.1.20".parse().expect("IP");
    assert_eq!(
        display_name("plurx", Some("lab6"), Some(&address)),
        "lab6 · 10.42.1.20"
    );
    assert_eq!(display_name("PLURX", Some("lab4.local."), None), "lab4");
    assert_eq!(
        display_name("Living Room", Some("lab6"), Some(&address)),
        "Living Room · 10.42.1.20"
    );
    assert_eq!(display_name("plurx", Some("localhost"), None), "plurx");
    assert_eq!(display_name("plurx", None, None), "plurx");
}

/// The label alone, which is all a name test is asking about.
fn display_name(configured: &str, hostname: Option<&str>, address: Option<&IpAddr>) -> String {
    discovery_name(configured, hostname, address).label
}

/// A cluster node is named for its machine, not for its UUID.
///
/// Every node of one logical server reports the same `server.name`, so
/// from 2026-08-20 a clustered node published that bare name plus twelve
/// characters of its node id — three rows of `plurx · 5deeeebc8f39` in the
/// Apple TV picker, on a fleet whose machines have perfectly good names.
/// The hostname and the LAN address are what tell nodes apart, and unlike
/// a UUID prefix a person can read them.
#[test]
fn a_clustered_node_is_named_for_its_machine_not_its_uuid() {
    let address: IpAddr = "10.42.4.8".parse().expect("IP");
    let node = "6b98c6cb-8388-48ba-8c2b-69c9dc6fc8a9";
    assert_eq!(
        instance_label(
            &discovery_name("plurx", Some("lab4"), Some(&address)),
            Some(node),
        ),
        "lab4 · 10.42.4.8"
    );
    // A hostname is already unique on one LAN, address or no address.
    assert_eq!(
        instance_label(&discovery_name("plurx", Some("lab4"), None), Some(node)),
        "lab4"
    );
}

/// The uniqueness the node id was protecting is still protected: a node
/// with nothing of its own to show falls back to it rather than becoming
/// the second identical row DNS-SD would have to rename.
#[test]
fn a_clustered_node_with_nothing_to_tell_it_apart_keeps_its_node_id() {
    let address: IpAddr = "10.42.4.8".parse().expect("IP");
    let node = "6b98c6cb-8388-48ba-8c2b-69c9dc6fc8a9";
    assert_eq!(
        instance_label(&discovery_name("plurx", None, None), Some(node)),
        "plurx · 6b98c6cb8388"
    );
    // An operator's custom name is configured identically on every node
    // of the cluster, so it does not distinguish one either.
    assert_eq!(
        instance_label(
            &discovery_name("Living Room", Some("lab4"), None),
            Some(node),
        ),
        "Living Room · 6b98c6cb8388"
    );
    // With this node's own address it does.
    assert_eq!(
        instance_label(
            &discovery_name("Living Room", Some("lab4"), Some(&address)),
            Some(node),
        ),
        "Living Room · 10.42.4.8"
    );
    // A standalone server has no peers to be confused with.
    assert_eq!(
        instance_label(&discovery_name("plurx", None, None), None),
        "plurx"
    );
}

/// The call site, not just the helper: `start_bonjour` must publish the
/// name this machine computed for itself. Restoring the branch that
/// handed a clustered node its bare `server.name` fails here on any host
/// that has either a hostname or a LAN address.
#[test]
fn a_clustered_server_advertises_the_name_it_computed_for_itself() {
    static SEEN: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());
    fn record(
        _instance_id: &str,
        name: &str,
        instance_name: &str,
        _node_id: Option<&str>,
        _bind: SocketAddr,
        _version: &str,
    ) -> anyhow::Result<mdns_sd::ServiceDaemon> {
        SEEN.lock()
            .expect("lock")
            .push((name.to_owned(), instance_name.to_owned()));
        anyhow::bail!("no mDNS daemon in this test")
    }

    const NODE: &str = "6b98c6cb-8388-48ba-8c2b-69c9dc6fc8a9";
    let tmp = crate::test_tempdir().expect("tempdir");
    let mut config = config_in(tmp.path());
    config.server.bind = "0.0.0.0:32400".parse().expect("addr");
    config.server.name = "plurx".to_owned();
    SEEN.lock().expect("lock").clear();

    assert!(
        start_bonjour(&config, "logical-server", Some(NODE), record, true).is_none(),
        "the stub advertiser refuses, and discovery is best-effort"
    );

    let seen = SEEN.lock().expect("lock").clone();
    assert_eq!(seen.len(), 1, "exactly one registration was attempted");
    let (name, instance_name) = seen[0].clone();
    assert_eq!(
        instance_name,
        expected_instance_name("plurx", Some(NODE)),
        "the label a picker shows is the one this machine computed"
    );
    assert_eq!(
        name,
        expected_instance_name("plurx", None),
        "the TXT name is the display label, without the node fallback"
    );
    if system_hostname().is_some() || primary_lan_address().is_some() {
        assert!(
            !instance_name.contains("6b98c6cb8388"),
            "a machine that can name itself must not be published as a UUID: {instance_name}"
        );
    }
}

#[test]
fn wildcard_bind_advertises_the_native_plurx_contract() {
    let info = mdns_service_info(
        "550e8400-e29b-41d4-a716-446655440000",
        "Living Room",
        "Living Room",
        None,
        "0.0.0.0:32400".parse().expect("socket address"),
        "0.2.0",
    )
    .expect("service info");

    assert_eq!(info.get_type(), MDNS_SERVICE_TYPE);
    assert_eq!(info.get_port(), 32400);
    assert_eq!(info.get_hostname(), "plurx-550e8400e29b.local.");
    assert_eq!(info.get_property_val_str("name"), Some("Living Room"));
    assert_eq!(info.get_property_val_str("version"), Some("0.2.0"));
    assert_eq!(info.get_property_val_str("api"), Some("/api/v1"));
    assert!(info.is_addr_auto());
}

#[test]
fn clustered_bonjour_records_keep_the_logical_id_but_address_the_node() {
    let instance_name = instance_label(&discovery_name("Living Room", None, None), Some("node-b"));
    let info = mdns_service_info(
        "logical-server",
        "Living Room",
        &instance_name,
        Some("node-b"),
        "0.0.0.0:32400".parse().expect("socket address"),
        "0.2.0",
    )
    .expect("service info");

    assert_eq!(info.get_hostname(), "plurx-nodeb.local.");
    assert!(info.get_fullname().contains("Living Room · nodeb"));
    assert_eq!(info.get_property_val_str("id"), Some("logical-server"));
    assert_eq!(info.get_property_val_str("node_id"), Some("node-b"));
    assert_eq!(info.get_property_val_str("name"), Some("Living Room"));
}

#[test]
fn clustered_bonjour_name_truncation_preserves_unicode_boundaries() {
    let name = clustered_instance_name(&"é".repeat(40), "node1");
    assert!(
        name.len() <= 63,
        "DNS-SD labels are bounded in bytes: {name}"
    );
    assert!(name.ends_with(" · node1"));
    assert!(std::str::from_utf8(name.as_bytes()).is_ok());
}

#[test]
fn explicit_bind_advertises_only_that_address() {
    let address = "10.42.1.20".parse().expect("IP address");
    let info = mdns_service_info(
        "server-id",
        "plurx",
        "plurx",
        None,
        "10.42.1.20:32400".parse().expect("socket address"),
        "0.2.0",
    )
    .expect("service info");

    assert!(!info.is_addr_auto());
    assert!(info.get_addresses().contains(&address));
}
