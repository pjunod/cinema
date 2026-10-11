//! Bound accepted connections independently of request/provider admission.
use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{Instant, Sleep},
};
pub struct BoundedListener {
    listener: TcpListener,
    permits: Arc<Semaphore>,
}
impl BoundedListener {
    pub fn new(listener: TcpListener) -> Self {
        Self {
            listener,
            permits: Arc::new(Semaphore::new(16)),
        }
    }
}
pub struct Stream {
    socket: TcpStream,
    _permit: OwnedSemaphorePermit,
    idle: Pin<Box<Sleep>>,
}
impl Stream {
    fn check(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        use std::future::Future;
        if self.idle.as_mut().poll(cx).is_ready() {
            Err(io::Error::new(io::ErrorKind::TimedOut, "idle connection"))
        } else {
            Ok(())
        }
    }
    fn progress(&mut self) {
        self.idle
            .as_mut()
            .reset(Instant::now() + Duration::from_secs(15));
    }
}
impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if let Err(error) = self.check(cx) {
            return Poll::Ready(Err(error));
        }
        let before = buffer.filled().len();
        let result = Pin::new(&mut self.socket).poll_read(cx, buffer);
        if result.is_ready() && buffer.filled().len() > before {
            self.progress();
        }
        result
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        if let Err(error) = self.check(cx) {
            return Poll::Ready(Err(error));
        }
        let result = Pin::new(&mut self.socket).poll_write(cx, buffer);
        if matches!(result,Poll::Ready(Ok(count)) if count>0) {
            self.progress();
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_shutdown(cx)
    }
}
impl axum::serve::Listener for BoundedListener {
    type Io = Stream;
    type Addr = std::net::SocketAddr;
    async fn accept(&mut self) -> (Stream, Self::Addr) {
        loop {
            match self.listener.accept().await {
                Ok((socket, address)) => {
                    let Ok(permit) = self.permits.clone().try_acquire_owned() else {
                        drop(socket);
                        continue;
                    };
                    return (
                        Stream {
                            socket,
                            _permit: permit,
                            idle: Box::pin(tokio::time::sleep(Duration::from_secs(15))),
                        },
                        address,
                    );
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }
    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}
