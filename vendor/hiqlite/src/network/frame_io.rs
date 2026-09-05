use fastwebsockets::{Frame, WebSocket, WebSocketError, WebSocketWrite};
use std::io;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

/// Maximum time spent writing and flushing one complete WebSocket frame.
pub(crate) const FRAME_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Grace allowed for a best-effort close before connection teardown wins.
pub(crate) const CLOSE_WRITE_TIMEOUT: Duration = Duration::from_millis(250);

pub(crate) async fn write_frame_flushed<S>(
    write: &mut WebSocketWrite<S>,
    frame: Frame<'_>,
) -> Result<(), WebSocketError>
where
    S: AsyncWrite + Unpin,
{
    write_frame_flushed_with_timeout(write, frame, FRAME_WRITE_TIMEOUT).await
}

pub(crate) async fn write_close_frame_flushed<S>(
    write: &mut WebSocketWrite<S>,
    frame: Frame<'_>,
) -> Result<(), WebSocketError>
where
    S: AsyncWrite + Unpin,
{
    write_frame_flushed_with_timeout(write, frame, CLOSE_WRITE_TIMEOUT).await
}

async fn write_frame_flushed_with_timeout<S>(
    write: &mut WebSocketWrite<S>,
    frame: Frame<'_>,
    timeout: Duration,
) -> Result<(), WebSocketError>
where
    S: AsyncWrite + Unpin,
{
    tokio::time::timeout(timeout, async {
        write.write_frame(frame).await?;
        write.flush().await
    })
    .await
    .map_err(|_| frame_write_timeout(timeout))?
}

pub(crate) async fn write_socket_frame_flushed<S>(
    socket: &mut WebSocket<S>,
    frame: Frame<'_>,
) -> Result<(), WebSocketError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    write_socket_frame_flushed_with_timeout(socket, frame, FRAME_WRITE_TIMEOUT).await
}

pub(crate) async fn write_socket_close_frame_flushed<S>(
    socket: &mut WebSocket<S>,
    frame: Frame<'_>,
) -> Result<(), WebSocketError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    write_socket_frame_flushed_with_timeout(socket, frame, CLOSE_WRITE_TIMEOUT).await
}

async fn write_socket_frame_flushed_with_timeout<S>(
    socket: &mut WebSocket<S>,
    frame: Frame<'_>,
    timeout: Duration,
) -> Result<(), WebSocketError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::timeout(timeout, async {
        socket.write_frame(frame).await?;
        socket.flush().await
    })
    .await
    .map_err(|_| frame_write_timeout(timeout))?
}

fn frame_write_timeout(timeout: Duration) -> WebSocketError {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "WebSocket frame write and flush exceeded {} ms",
            timeout.as_millis()
        ),
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastwebsockets::{FragmentCollectorRead, Payload, Role};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};
    use tokio::io::{AsyncRead, DuplexStream, ReadBuf};
    use tokio_rustls::{TlsAcceptor, TlsConnector};

    struct TestIo {
        flushes: Arc<AtomicUsize>,
        block_flush: bool,
        fail_flush: bool,
    }

    impl AsyncRead for TestIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    impl AsyncWrite for TestIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.flushes.fetch_add(1, Ordering::Relaxed);
            if self.fail_flush {
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "injected flush failure",
                )))
            } else if self.block_flush {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn split_writer(io: TestIo) -> WebSocketWrite<tokio::io::WriteHalf<TestIo>> {
        let socket = WebSocket::after_handshake(io, Role::Client);
        let (_read, write) = socket.split(tokio::io::split);
        write
    }

    #[derive(Default)]
    struct Gate {
        remaining: Option<usize>,
        waker: Option<Waker>,
    }

    struct GatedIo {
        inner: DuplexStream,
        gate: Arc<Mutex<Gate>>,
    }

    impl AsyncRead for GatedIo {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for GatedIo {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            {
                let mut gate = self.gate.lock().expect("gate lock");
                if gate.remaining == Some(0) {
                    gate.waker = Some(cx.waker().clone());
                    return Poll::Pending;
                }
            }
            let limit = self
                .gate
                .lock()
                .expect("gate lock")
                .remaining
                .unwrap_or(buf.len())
                .min(buf.len());
            let result = Pin::new(&mut self.inner).poll_write(cx, &buf[..limit]);
            if let Poll::Ready(Ok(written)) = result
                && let Some(remaining) = self.gate.lock().expect("gate lock").remaining.as_mut()
            {
                *remaining -= written;
            }
            result
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    #[tokio::test]
    async fn frame_completion_flushes_the_underlying_transport() {
        let flushes = Arc::new(AtomicUsize::new(0));
        let mut write = split_writer(TestIo {
            flushes: Arc::clone(&flushes),
            block_flush: false,
            fail_flush: false,
        });

        for len in [32, 3 * 1024 * 1024] {
            write_frame_flushed_with_timeout(
                &mut write,
                Frame::binary(Payload::Owned(vec![42; len])),
                Duration::from_millis(50),
            )
            .await
            .expect("write and flush must complete");
        }

        assert_eq!(flushes.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn flush_failure_is_a_failed_frame_write() {
        let flushes = Arc::new(AtomicUsize::new(0));
        let mut write = split_writer(TestIo {
            flushes: Arc::clone(&flushes),
            block_flush: false,
            fail_flush: true,
        });

        let err = write_frame_flushed_with_timeout(
            &mut write,
            Frame::binary(Payload::Borrowed(b"accepted plaintext")),
            Duration::from_millis(50),
        )
        .await
        .expect_err("flush failure must fail the frame");

        assert!(matches!(
            err,
            WebSocketError::IoError(ref err) if err.kind() == io::ErrorKind::BrokenPipe
        ));
        assert_eq!(flushes.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn permanently_blocked_flush_expires_the_combined_budget() {
        let flushes = Arc::new(AtomicUsize::new(0));
        let mut write = split_writer(TestIo {
            flushes: Arc::clone(&flushes),
            block_flush: true,
            fail_flush: false,
        });

        let err = write_frame_flushed_with_timeout(
            &mut write,
            Frame::binary(Payload::Borrowed(b"snapshot chunk")),
            Duration::from_millis(50),
        )
        .await
        .expect_err("a blocked flush must time out");

        assert!(matches!(
            err,
            WebSocketError::IoError(ref err) if err.kind() == io::ErrorKind::TimedOut
        ));
        assert!(flushes.load(Ordering::Relaxed) >= 1);
    }

    #[tokio::test]
    async fn real_tls_tail_backpressure_completes_for_every_payload_direction() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        for (surface, role, len) in [
            ("Raft request", Role::Client, 3 * 1024 * 1024),
            ("Raft response", Role::Server, 32),
            ("cluster API request", Role::Client, 32),
            ("cluster API response", Role::Server, 3 * 1024 * 1024),
        ] {
            tokio::time::timeout(
                Duration::from_secs(5),
                send_through_gated_tls(role, len, true),
            )
            .await
            .unwrap_or_else(|_| panic!("{surface} must recover within five seconds"));
        }
    }

    #[tokio::test]
    async fn real_tls_no_backpressure_control_completes_without_an_extra_frame() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        tokio::time::timeout(
            Duration::from_secs(5),
            send_through_gated_tls(Role::Server, 32, false),
        )
        .await
        .expect("control frame must complete without backpressure");
    }

    async fn send_through_gated_tls(role: Role, len: usize, inject_backpressure: bool) {
        let cert =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).expect("test certificate");
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.cert.der().clone()).expect("test root");
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
        let server = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert.der().clone()], key.into())
            .expect("server TLS config");
        let client = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let gate = Arc::new(Mutex::new(Gate::default()));
        let open_gate = Arc::new(Mutex::new(Gate::default()));
        let connector = TlsConnector::from(Arc::new(client));
        let acceptor = TlsAcceptor::from(Arc::new(server));
        let (client_tls, server_tls) = tokio::join!(
            connector.connect(
                "localhost".try_into().expect("server name"),
                GatedIo {
                    inner: client_io,
                    gate: if role == Role::Client {
                        Arc::clone(&gate)
                    } else {
                        Arc::clone(&open_gate)
                    },
                }
            ),
            acceptor.accept(GatedIo {
                inner: server_io,
                gate: if role == Role::Server {
                    Arc::clone(&gate)
                } else {
                    open_gate
                },
            })
        );
        let client_tls = tokio_rustls::TlsStream::Client(client_tls.expect("client TLS"));
        let server_tls = tokio_rustls::TlsStream::Server(server_tls.expect("server TLS"));
        let (sender_tls, receiver_tls, remote_role) = if role == Role::Server {
            (server_tls, client_tls, Role::Client)
        } else {
            (client_tls, server_tls, Role::Server)
        };
        let sender = WebSocket::after_handshake(sender_tls, role);
        let receiver = WebSocket::after_handshake(receiver_tls, remote_role);
        let (_sender_read, mut sender_write) = sender.split(tokio::io::split);
        let (receiver_read, _receiver_write) = receiver.split(tokio::io::split);
        let receive = tokio::spawn(async move {
            let mut read = FragmentCollectorRead::new(receiver_read);
            read.read_frame(&mut |_| async { Ok::<_, io::Error>(()) })
                .await
                .expect("receive frame")
                .payload
                .to_vec()
        });

        if inject_backpressure {
            gate.lock().expect("gate lock").remaining = Some(if len > 64 * 1024 { len } else { 0 });
        }
        let send = tokio::spawn(async move {
            write_frame_flushed(
                &mut sender_write,
                Frame::binary(Payload::Owned(vec![42; len])),
            )
            .await
        });

        if inject_backpressure {
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if gate.lock().expect("gate lock").waker.is_some() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("write must reach gated ciphertext tail");
            {
                let mut state = gate.lock().expect("gate lock");
                state.remaining = None;
                if let Some(waker) = state.waker.take() {
                    waker.wake();
                }
            }
        }

        send.await
            .expect("writer task")
            .expect("write and flush after transport resumes");
        let received = receive.await.expect("reader task");
        assert_eq!(received, vec![42; len]);
    }
}
