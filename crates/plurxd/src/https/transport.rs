//! Owned public TLS admission and serving lifecycle.
use super::Identity;
use crate::{HttpAcceptor, HttpDrain, HTTP_TIMEOUTS};
use anyhow::{Context, Result};
use futures_util::{future::BoxFuture, stream::FuturesUnordered, FutureExt, StreamExt};
use plurx_core::config::Config;
use std::{net::SocketAddr, panic::AssertUnwindSafe, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{Mutex, Semaphore},
    task::JoinSet,
};
use tokio_rustls::server::TlsStream;
use tokio_util::sync::CancellationToken;

const PENDING_HANDSHAKES: usize = 128;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
type Handshake = BoxFuture<'static, Result<(TlsStream<tokio::net::TcpStream>, SocketAddr)>>;

/// The queue owns every handshake future. Dropping the acceptor closes all
/// pending sockets immediately; it never leaves detached handshake tasks.
pub(super) struct TlsListener {
    socket: TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    pending: Mutex<FuturesUnordered<Handshake>>,
    slots: Arc<Semaphore>,
    limit: usize,
    deadline: Duration,
}

impl TlsListener {
    fn new(socket: TcpListener, identity: &Identity) -> Self {
        Self::with_limits(socket, identity, PENDING_HANDSHAKES, HANDSHAKE_TIMEOUT)
    }

    fn with_limits(
        socket: TcpListener,
        identity: &Identity,
        limit: usize,
        deadline: Duration,
    ) -> Self {
        Self {
            socket,
            acceptor: tokio_rustls::TlsAcceptor::from(identity.config.clone()),
            pending: Mutex::new(FuturesUnordered::new()),
            slots: Arc::new(Semaphore::new(limit)),
            limit,
            deadline,
        }
    }
}

impl HttpAcceptor for TlsListener {
    type Stream = TlsStream<tokio::net::TcpStream>;

    async fn accept(&self) -> std::io::Result<(Self::Stream, SocketAddr)> {
        let mut pending = self.pending.lock().await;
        loop {
            tokio::select! {
                completed = pending.next(), if !pending.is_empty() => {
                    if let Some(Ok(accepted)) = completed { return Ok(accepted); }
                    // Invalid, plaintext and timed-out peers are local failures,
                    // never TCP accept errors or reasons to back off admission.
                }
                accepted = self.socket.accept(), if pending.len() < self.limit => {
                    let (stream, remote) = accepted?;
                    crate::disable_nagle(&stream, remote);
                    let permit = self.slots.clone().try_acquire_owned()
                        .expect("pending handshake queue owns its exact permit count");
                    let acceptor = self.acceptor.clone();
                    let deadline = self.deadline;
                    pending.push(async move {
                        let _permit = permit;
                        let stream = tokio::time::timeout(deadline, acceptor.accept(stream))
                            .await.context("HTTPS handshake deadline")??;
                        Ok((stream, remote))
                    }.boxed());
                }
            }
        }
    }
}

pub(crate) struct Prepared {
    socket: TcpListener,
    pub(crate) identity: Arc<Identity>,
}

impl Prepared {
    /// Validate and bind before full readiness. Only startup provisions keys.
    pub(crate) async fn open(config: &Config) -> Result<Option<Self>> {
        let settings = config
            .server
            .https
            .clone()
            .normalize(config.server.bind.ip())?;
        settings.validate_ports(config)?;
        if !settings.enabled {
            return Ok(None);
        }
        let root = config.storage.data_dir.clone();
        let port = settings.port;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("HTTPS requires a valid system clock")?
            .as_secs();
        let now = i64::try_from(now).context("HTTPS system clock is out of range")?;
        let identity =
            tokio::task::spawn_blocking(move || Identity::load(&settings, &root, now, true))
                .await
                .context("HTTPS identity worker failed")??
                .ok_or_else(|| anyhow::anyhow!("enabled HTTPS has no identity"))?;
        let address = SocketAddr::new(config.server.bind.ip(), port);
        let socket = TcpListener::bind(address)
            .await
            .context("cannot bind enabled HTTPS listener")?;
        Ok(Some(Self {
            socket,
            identity: Arc::new(identity),
        }))
    }
}

/// The JoinSet owns the server even if startup or its caller is cancelled.
/// Fatal completion requests daemon shutdown and remains an error at drain,
/// so the final census cannot claim a clean stop.
#[derive(Default)]
pub(crate) struct Serving {
    stop: CancellationToken,
    tasks: JoinSet<Result<HttpDrain>>,
    _identity: Option<Arc<Identity>>,
}

impl Serving {
    pub(crate) fn start(
        prepared: Option<Prepared>,
        app: axum::Router,
        listener_stop: CancellationToken,
        daemon_stop: CancellationToken,
    ) -> Self {
        let Some(prepared) = prepared else {
            return Self::default();
        };
        tracing::info!(address = %prepared.socket.local_addr().expect("bound HTTPS socket"),
            mode = prepared.identity.facts.mode, fingerprint = %prepared.identity.facts.sha256,
            expires_at = prepared.identity.facts.expires_at_seconds, "listening for HTTPS");
        let listener = TlsListener::new(prepared.socket, &prepared.identity);
        let stop = listener_stop.child_token();
        let shutdown = stop.clone();
        let future = crate::serve_http(
            listener,
            app,
            async move { shutdown.cancelled().await },
            HTTP_TIMEOUTS,
        );
        let mut serving = Self::supervise(future, stop, daemon_stop);
        serving._identity = Some(prepared.identity);
        serving
    }

    fn supervise(
        future: impl std::future::Future<Output = Result<HttpDrain>> + Send + 'static,
        stop: CancellationToken,
        daemon_stop: CancellationToken,
    ) -> Self {
        let expected_stop = stop.clone();
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            let result = AssertUnwindSafe(future)
                .catch_unwind()
                .await
                .map_err(|_| anyhow::anyhow!("HTTPS serving task panicked"))
                .and_then(|result| result);
            if result.is_err() || !expected_stop.is_cancelled() {
                daemon_stop.cancel();
                return Err(anyhow::anyhow!("HTTPS listener stopped unexpectedly"));
            }
            result
        });
        Self {
            stop,
            tasks,
            _identity: None,
        }
    }

    pub(crate) async fn drain(mut self) -> Result<HttpDrain> {
        self.stop.cancel();
        match self.tasks.join_next().await {
            None => Ok(HttpDrain::Complete),
            Some(result) => result.context("HTTPS serving task failed")?,
        }
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, Bytes},
        extract::ConnectInfo,
        routing::get,
        Router,
    };
    use std::{convert::Infallible, net::IpAddr};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn identity(root: &std::path::Path) -> Arc<Identity> {
        let settings = plurx_core::config::https::HttpsConfig {
            enabled: true,
            ..Default::default()
        }
        .normalize("127.0.0.1".parse().expect("loopback"))
        .expect("settings");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64;
        Arc::new(
            Identity::load(&settings, root, now, true)
                .expect("identity")
                .expect("enabled"),
        )
    }

    fn client(identity: &Identity, h2: bool) -> reqwest::Client {
        let certificate =
            reqwest::Certificate::from_pem(&identity.public_chain_pem()).expect("public cert");
        let builder = reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(certificate)
            .timeout(Duration::from_secs(3));
        if h2 {
            builder.http2_prior_knowledge()
        } else {
            builder.http1_only()
        }
        .build()
        .expect("trusted client")
    }

    async fn socket() -> TcpListener {
        TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener")
    }

    async fn wait_for_slots(slots: &Semaphore, count: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while slots.available_permits() != count {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("bounded handshake observation");
    }

    #[tokio::test]
    async fn malformed_and_silent_tls_peers_do_not_serialize_or_back_off_healthy_http() {
        let root = crate::test_tempdir().expect("tempdir");
        let identity = identity(root.path());
        let socket = socket().await;
        let address = socket.local_addr().expect("address");
        let listener = TlsListener::new(socket, &identity);
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let app = Router::new().route(
            "/peer",
            get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.ip().to_string() }),
        );
        let mut timeouts = HTTP_TIMEOUTS;
        timeouts.accept_error_backoff = Duration::from_secs(30);
        let server = tokio::spawn(crate::serve_http(
            listener,
            app,
            async move { stopped.cancelled().await },
            timeouts,
        ));
        let mut silent = tokio::net::TcpStream::connect(address)
            .await
            .expect("silent peer");
        let mut malformed = tokio::net::TcpStream::connect(address)
            .await
            .expect("plaintext peer");
        malformed
            .write_all(b"GET /peer HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("plaintext write");
        drop(malformed);
        for h2 in [false, true] {
            let response = client(&identity, h2)
                .get(format!("https://localhost:{}/peer", address.port()))
                .send()
                .await
                .expect("healthy admission while a peer is silent");
            assert_eq!(
                response.version(),
                if h2 {
                    reqwest::Version::HTTP_2
                } else {
                    reqwest::Version::HTTP_11
                }
            );
            assert_eq!(
                response
                    .text()
                    .await
                    .expect("peer response")
                    .parse::<IpAddr>()
                    .expect("peer IP"),
                address.ip()
            );
        }
        stop.cancel();
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), silent.read(&mut byte))
                .await
                .expect("pending peer closes promptly")
                .expect("EOF"),
            0
        );
        assert_eq!(
            server.await.expect("join").expect("serve"),
            HttpDrain::Complete
        );
        TcpListener::bind(address)
            .await
            .expect("TLS socket released");
    }

    #[tokio::test]
    async fn handshake_cap_deadline_releases_permits_and_admits_a_waiting_healthy_peer() {
        let root = crate::test_tempdir().expect("tempdir");
        let identity = identity(root.path());
        let socket = socket().await;
        let address = socket.local_addr().expect("address");
        let listener = TlsListener::with_limits(socket, &identity, 2, Duration::from_millis(300));
        let slots = listener.slots.clone();
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let server = tokio::spawn(crate::serve_http(
            listener,
            Router::new().route("/", get(|| async { "ok" })),
            async move { stopped.cancelled().await },
            HTTP_TIMEOUTS,
        ));
        let first = tokio::net::TcpStream::connect(address)
            .await
            .expect("silent first");
        let second = tokio::net::TcpStream::connect(address)
            .await
            .expect("silent second");
        wait_for_slots(&slots, 0).await;
        let response = client(&identity, false)
            .get(format!("https://localhost:{}/", address.port()))
            .send()
            .await
            .expect("deadline frees admission");
        assert_eq!(response.text().await.expect("body"), "ok");
        wait_for_slots(&slots, 2).await;
        stop.cancel();
        assert_eq!(
            server.await.expect("join").expect("serve"),
            HttpDrain::Complete
        );
        assert_eq!(slots.available_permits(), 2);
        drop((first, second));
    }

    #[tokio::test]
    async fn public_listener_stop_preserves_preparation_then_closes_tls_before_http_body_drain() {
        let root = crate::test_tempdir().expect("tempdir");
        let identity = identity(root.path());
        let tls_socket = socket().await;
        let tls_address = tls_socket.local_addr().expect("TLS address");
        let daemon_stop = CancellationToken::new();
        let listener_stop = CancellationToken::new();
        let tls = Serving::start(
            Some(Prepared {
                socket: tls_socket,
                identity: identity.clone(),
            }),
            Router::new().route("/close", get(|| async { "closed" })),
            listener_stop.clone(),
            daemon_stop.clone(),
        );
        let mut silent = tokio::net::TcpStream::connect(tls_address)
            .await
            .expect("silent TLS peer");
        let (release, released) = tokio::sync::oneshot::channel();
        let released = Arc::new(Mutex::new(Some(released)));
        let app = Router::new().route(
            "/stream",
            get(move || {
                let released = released.clone();
                async move {
                    let released = released.lock().await.take().expect("one stream");
                    let head = futures_util::stream::once(async {
                        Ok::<_, Infallible>(Bytes::from_static(b"head"))
                    });
                    let tail = futures_util::stream::once(async move {
                        let _ = released.await;
                        Ok::<_, Infallible>(Bytes::from_static(b"tail"))
                    });
                    Body::from_stream(head.chain(tail))
                }
            }),
        );
        let plain_socket = socket().await;
        let plain_address = plain_socket.local_addr().expect("HTTP address");
        let app = app.route("/close", get(|| async { "closed" }));
        let plain_stop = daemon_stop.clone();
        let (finish_preparation, preparation_finished) = tokio::sync::oneshot::channel();
        let plain = tokio::spawn(crate::serve_http(
            plain_socket,
            app,
            async move {
                plain_stop.cancelled().await;
                let _ = preparation_finished.await;
                listener_stop.cancel();
            },
            HTTP_TIMEOUTS,
        ));
        let mut response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("HTTP client")
            .get(format!("http://{plain_address}/stream"))
            .send()
            .await
            .expect("stream headers");
        assert_eq!(
            response.chunk().await.expect("head").expect("chunk"),
            "head"
        );
        daemon_stop.cancel();
        for base in [
            format!("http://{plain_address}"),
            format!("https://localhost:{}", tls_address.port()),
        ] {
            let response = client(&identity, false)
                .get(format!("{base}/close"))
                .send()
                .await
                .expect("close/control requests remain available during preparation");
            assert_eq!(response.text().await.expect("close response"), "closed");
        }
        finish_preparation.send(()).expect("preparation complete");
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), silent.read(&mut byte))
                .await
                .expect("public stop cancels pending TLS before HTTP drain")
                .expect("EOF"),
            0
        );
        assert!(
            !plain.is_finished(),
            "active HTTP response is still draining"
        );
        release.send(()).expect("release stream");
        assert_eq!(response.bytes().await.expect("tail"), "tail");
        assert_eq!(
            plain.await.expect("HTTP join").expect("HTTP serve"),
            HttpDrain::Complete
        );
        assert_eq!(tls.drain().await.expect("TLS drain"), HttpDrain::Complete);
        TcpListener::bind(tls_address)
            .await
            .expect("TLS port released");
        TcpListener::bind(plain_address)
            .await
            .expect("HTTP port released");
    }

    #[tokio::test]
    async fn stopping_tls_cancels_pending_handshakes_while_an_active_tls_body_drains() {
        let root = crate::test_tempdir().expect("tempdir");
        let identity = identity(root.path());
        let socket = socket().await;
        let address = socket.local_addr().expect("address");
        let listener = TlsListener::new(socket, &identity);
        let slots = listener.slots.clone();
        let (release, released) = tokio::sync::oneshot::channel();
        let receiver = Arc::new(Mutex::new(Some(released)));
        let app = Router::new().route(
            "/",
            get(move || {
                let receiver = receiver.clone();
                async move {
                    let released = receiver.lock().await.take().expect("one response");
                    Body::from_stream(
                        futures_util::stream::once(async {
                            Ok::<_, Infallible>(Bytes::from_static(b"head"))
                        })
                        .chain(futures_util::stream::once(async move {
                            let _ = released.await;
                            Ok::<_, Infallible>(Bytes::from_static(b"tail"))
                        })),
                    )
                }
            }),
        );
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let server = tokio::spawn(crate::serve_http(
            listener,
            app,
            async move { stopped.cancelled().await },
            HTTP_TIMEOUTS,
        ));
        let mut silent = tokio::net::TcpStream::connect(address)
            .await
            .expect("silent peer");
        let mut response = client(&identity, false)
            .get(format!("https://localhost:{}/", address.port()))
            .send()
            .await
            .expect("headers");
        assert_eq!(
            response.chunk().await.expect("head").expect("chunk"),
            "head"
        );
        stop.cancel();
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), silent.read(&mut byte))
                .await
                .expect("pending cancellation precedes body drain")
                .expect("EOF"),
            0
        );
        assert!(!server.is_finished());
        assert_eq!(slots.available_permits(), PENDING_HANDSHAKES);
        release.send(()).expect("release");
        assert_eq!(response.bytes().await.expect("tail"), "tail");
        assert_eq!(
            server.await.expect("join").expect("serve"),
            HttpDrain::Complete
        );
    }

    #[tokio::test]
    async fn unexpected_tls_completion_or_panic_requests_daemon_stop_and_stays_unclean() {
        for panic in [false, true] {
            let daemon = CancellationToken::new();
            let serving = Serving::supervise(
                async move {
                    if panic {
                        std::panic::resume_unwind(Box::new("synthetic TLS task panic"));
                    }
                    Ok(HttpDrain::Complete)
                },
                daemon.child_token(),
                daemon.clone(),
            );
            tokio::time::timeout(Duration::from_secs(1), daemon.cancelled())
                .await
                .expect("fatal task stops daemon");
            assert!(
                serving.drain().await.is_err(),
                "fatal TLS completion must not mark census clean"
            );
        }
    }

    #[tokio::test]
    async fn enabled_https_refuses_occupied_port_or_invalid_identity_while_disabled_ignores_material(
    ) {
        let root = crate::test_tempdir().expect("tempdir");
        let occupied = socket().await;
        let mut config = Config::default();
        config.storage.data_dir = root.path().to_path_buf();
        config.server.bind = "127.0.0.1:0".parse().expect("bind");
        config.server.https.enabled = true;
        config.server.https.port = occupied.local_addr().expect("address").port();
        assert!(
            Prepared::open(&config).await.is_err(),
            "enabled bind failure refuses startup"
        );
        drop(occupied);
        std::fs::write(
            root.path()
                .join("https")
                .join(super::super::MANAGED_IDENTITY_FILE),
            "damaged",
        )
        .expect("damage fixture");
        assert!(
            Prepared::open(&config).await.is_err(),
            "bad identity refuses startup"
        );
        config.server.https.enabled = false;
        assert!(Prepared::open(&config).await.expect("disabled").is_none());
    }
}
