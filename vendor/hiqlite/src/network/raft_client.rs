use crate::Node;
use crate::NodeId;
use crate::app_state::RaftType;
use crate::helpers::{deserialize, serialize};
use crate::network::frame_io::{write_close_frame_flushed, write_frame_flushed};
use crate::network::raft_server::{
    RaftStreamRequest, RaftStreamResponse, RaftStreamResponsePayload,
};
use crate::network::web_socket_connect;
use fastwebsockets::{FragmentCollectorRead, Frame, OpCode, Payload, WebSocketWrite};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use openraft::error::RPCError;
use openraft::error::RemoteError;
use openraft::error::Unreachable;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;
use tokio::{select, task, time};
use tracing::{debug, error, info};

#[cfg(feature = "validation-test-helpers")]
static VALIDATION_RAFT_PARTITIONED: AtomicBool = AtomicBool::new(false);

/// Isolate this validation process from every Raft peer without changing its
/// authenticated client API. The feature is absent from production builds.
#[cfg(feature = "validation-test-helpers")]
pub fn validation_set_raft_partitioned(partitioned: bool) {
    VALIDATION_RAFT_PARTITIONED.store(partitioned, Ordering::Release);
}

#[cfg(feature = "validation-test-helpers")]
pub(crate) fn validation_raft_partitioned() -> bool {
    VALIDATION_RAFT_PARTITIONED.load(Ordering::Acquire)
}

#[cfg(feature = "cache")]
use crate::store::state_machine::memory::TypeConfigKV;

#[cfg(feature = "sqlite")]
use crate::store::state_machine::sqlite::TypeConfigSqlite;

#[cfg(any(feature = "cache", feature = "sqlite"))]
use crate::Error;
#[cfg(any(feature = "cache", feature = "sqlite"))]
use openraft::{
    error::{InstallSnapshotError, RaftError},
    network::{RPCOption, RaftNetwork, RaftNetworkFactory},
    raft::{
        AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest,
        InstallSnapshotResponse, VoteRequest, VoteResponse,
    },
};

pub struct NetworkStreaming {
    pub node_id: NodeId,
    pub tls_config: Option<Arc<rustls::ClientConfig>>,
    pub secret_raft: Vec<u8>,
    pub raft_type: RaftType,
    pub heartbeat_interval: u64,
    pub is_raft_stopped: Arc<AtomicBool>,
    pub is_startup_finished: Arc<AtomicBool>,
    // pub sender: flume::Sender<RaftRequest>,
}

#[cfg(feature = "cache")]
impl RaftNetworkFactory<TypeConfigKV> for NetworkStreaming {
    type Network = NetworkConnectionStreaming;

    #[tracing::instrument(level = "debug", skip_all)]
    async fn new_client(&mut self, _target: NodeId, node: &Node) -> Self::Network {
        debug!("Building new Raft Cache client with target {}", node);

        let (sender, rx) = flume::bounded(1);
        let reset = Arc::new(ConnectionResetState::default());

        let task = tokio::task::spawn(Box::pin(Self::ws_handler(
            self.node_id,
            self.raft_type.clone(),
            node.clone(),
            self.tls_config.clone(),
            self.secret_raft.clone(),
            rx,
            self.heartbeat_interval,
            self.is_raft_stopped.clone(),
            self.is_startup_finished.clone(),
            Arc::clone(&reset),
        )));

        NetworkConnectionStreaming {
            node: node.clone(),
            sender,
            reset,
            task: Some(task),
        }
    }
}

#[cfg(feature = "sqlite")]
impl RaftNetworkFactory<TypeConfigSqlite> for NetworkStreaming {
    type Network = NetworkConnectionStreaming;

    #[tracing::instrument(level = "debug", skip_all)]
    async fn new_client(&mut self, _target: NodeId, node: &Node) -> Self::Network {
        debug!("Building new Raft DB client with target {}", node);

        let (sender, rx) = flume::bounded(1);
        let reset = Arc::new(ConnectionResetState::default());

        let task = tokio::task::spawn(Box::pin(Self::ws_handler(
            self.node_id,
            self.raft_type.clone(),
            node.clone(),
            self.tls_config.clone(),
            self.secret_raft.clone(),
            rx,
            self.heartbeat_interval,
            self.is_raft_stopped.clone(),
            self.is_startup_finished.clone(),
            Arc::clone(&reset),
        )));

        NetworkConnectionStreaming {
            node: node.clone(),
            sender,
            reset,
            task: Some(task),
        }
    }
}

#[derive(Debug)]
enum RaftRequest {
    #[cfg(feature = "sqlite")]
    AppendDB(
        (
            oneshot::Sender<Result<RaftStreamResponsePayload, Error>>,
            AppendEntriesRequest<TypeConfigSqlite>,
        ),
    ),
    #[cfg(feature = "sqlite")]
    VoteDB(
        (
            oneshot::Sender<Result<RaftStreamResponsePayload, Error>>,
            VoteRequest<u64>,
        ),
    ),
    #[cfg(feature = "sqlite")]
    SnapshotDB(
        (
            oneshot::Sender<Result<RaftStreamResponsePayload, Error>>,
            InstallSnapshotRequest<TypeConfigSqlite>,
        ),
    ),

    #[cfg(feature = "cache")]
    AppendCache(
        (
            oneshot::Sender<Result<RaftStreamResponsePayload, Error>>,
            AppendEntriesRequest<TypeConfigKV>,
        ),
    ),
    #[cfg(feature = "cache")]
    VoteCache(
        (
            oneshot::Sender<Result<RaftStreamResponsePayload, Error>>,
            VoteRequest<u64>,
        ),
    ),
    #[cfg(feature = "cache")]
    SnapshotCache(
        (
            oneshot::Sender<Result<RaftStreamResponsePayload, Error>>,
            InstallSnapshotRequest<TypeConfigKV>,
        ),
    ),

    StreamResponse(RaftStreamResponse),

    ReaderExit,
    Shutdown,
}

impl RaftRequest {
    fn outbound_disposition(
        &self,
        socket_epoch: u64,
        reset_epoch: u64,
    ) -> Option<OutboundDisposition> {
        let response_is_closed = match self {
            #[cfg(feature = "sqlite")]
            Self::AppendDB((ack, _)) => ack.is_closed(),
            #[cfg(feature = "sqlite")]
            Self::VoteDB((ack, _)) => ack.is_closed(),
            #[cfg(feature = "sqlite")]
            Self::SnapshotDB((ack, _)) => ack.is_closed(),
            #[cfg(feature = "cache")]
            Self::AppendCache((ack, _)) => ack.is_closed(),
            #[cfg(feature = "cache")]
            Self::VoteCache((ack, _)) => ack.is_closed(),
            #[cfg(feature = "cache")]
            Self::SnapshotCache((ack, _)) => ack.is_closed(),
            Self::StreamResponse(_) | Self::ReaderExit | Self::Shutdown => return None,
        };

        if response_is_closed {
            Some(OutboundDisposition::DropCancelled)
        } else if reset_epoch != socket_epoch {
            Some(OutboundDisposition::Reconnect)
        } else {
            Some(OutboundDisposition::Send)
        }
    }

    fn fail_outbound(self, error: Error) {
        let ack = match self {
            #[cfg(feature = "sqlite")]
            Self::AppendDB((ack, _)) => Some(ack),
            #[cfg(feature = "sqlite")]
            Self::VoteDB((ack, _)) => Some(ack),
            #[cfg(feature = "sqlite")]
            Self::SnapshotDB((ack, _)) => Some(ack),
            #[cfg(feature = "cache")]
            Self::AppendCache((ack, _)) => Some(ack),
            #[cfg(feature = "cache")]
            Self::VoteCache((ack, _)) => Some(ack),
            #[cfg(feature = "cache")]
            Self::SnapshotCache((ack, _)) => Some(ack),
            Self::StreamResponse(_) | Self::ReaderExit | Self::Shutdown => None,
        };
        if let Some(ack) = ack {
            let _ = ack.send(Err(error));
        }
    }
}

#[derive(Debug)]
enum WritePayload {
    Payload(Vec<u8>),
    Close,
}

#[derive(Default)]
struct ConnectionResetState {
    epoch: AtomicU64,
    notify: Notify,
}

impl ConnectionResetState {
    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    fn request_reset(&self, observed_epoch: u64) {
        if self
            .epoch
            .compare_exchange(
                observed_epoch,
                observed_epoch.wrapping_add(1),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.notify.notify_one();
        }
    }

    async fn changed_since(&self, observed_epoch: u64) {
        while self.epoch() == observed_epoch {
            self.notify.notified().await;
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum OutboundDisposition {
    Send,
    DropCancelled,
    Reconnect,
}

#[derive(Debug)]
enum WriteEnqueueError {
    Reset,
    Disconnected(flume::SendError<WritePayload>),
}

// Boxing the request variant would add an allocation to every Raft RPC only to
// reduce this short-lived supervisor value's stack size.
#[allow(clippy::large_enum_variant)]
enum ConnectedEvent {
    Reset,
    WriterFinished(Result<(), String>),
    Request(Result<RaftRequest, flume::RecvError>),
}

async fn next_connected_event(
    reset: &ConnectionResetState,
    socket_epoch: u64,
    rx_read: &flume::Receiver<RaftRequest>,
    rx: &flume::Receiver<RaftRequest>,
    writer_finished: &mut oneshot::Receiver<Result<(), String>>,
) -> ConnectedEvent {
    select! {
        biased;
        _ = reset.changed_since(socket_epoch) => ConnectedEvent::Reset,
        result = writer_finished => ConnectedEvent::WriterFinished(
            result.unwrap_or_else(|_| Err("Raft writer task exited without reporting an outcome".into()))
        ),
        result = rx_read.recv_async() => ConnectedEvent::Request(result),
        result = rx.recv_async() => ConnectedEvent::Request(result),
    }
}

async fn enqueue_write_or_reset(
    tx_write: &flume::Sender<WritePayload>,
    payload: WritePayload,
    reset: &ConnectionResetState,
    socket_epoch: u64,
) -> Result<(), WriteEnqueueError> {
    select! {
        biased;
        _ = reset.changed_since(socket_epoch) => Err(WriteEnqueueError::Reset),
        result = tx_write.send_async(payload) => result.map_err(WriteEnqueueError::Disconnected),
    }
}

async fn stop_stream_tasks(
    tx_write: &flume::Sender<WritePayload>,
    handle_write: JoinHandle<()>,
    handle_read: JoinHandle<()>,
    forced_reset: bool,
) {
    // Cleanup must never queue behind a blocked socket write. A reset is a
    // forced transport boundary, so abort both split tasks immediately after
    // a best-effort close. Other reconnects retain the short graceful window.
    let _ = tx_write.try_send(WritePayload::Close);
    if !forced_reset {
        time::sleep(Duration::from_millis(250)).await;
    }

    handle_write.abort();
    handle_read.abort();
    let _ = handle_write.await;
    let _ = handle_read.await;
}

#[allow(clippy::type_complexity)]
impl NetworkStreaming {
    #[allow(clippy::too_many_arguments)]
    async fn ws_handler(
        this_node: NodeId,
        raft_type: RaftType,
        node: Node,
        tls_config: Option<Arc<rustls::ClientConfig>>,
        secret: Vec<u8>,
        rx: flume::Receiver<RaftRequest>,
        heartbeat_interval: u64,
        is_raft_stopped: Arc<AtomicBool>,
        is_startup_finished: Arc<AtomicBool>,
        reset: Arc<ConnectionResetState>,
    ) {
        let mut request_id = 0usize;
        // TODO probably, a Vec<_> is faster here since we would never have too many in flight reqs
        // for raft internal replication and voting? -> check
        // maybe feature-gate an alternative impl, even though it might not make the biggest difference
        let mut in_flight: HashMap<
            usize,
            oneshot::Sender<Result<RaftStreamResponsePayload, Error>>,
        > = HashMap::with_capacity(4);
        let mut shutdown = false;

        'outer: loop {
            if is_raft_stopped.load(Ordering::Relaxed) {
                if !is_startup_finished.load(Ordering::Relaxed) {
                    debug!("Raft is still starting up - skipping initial connection");
                    time::sleep(Duration::from_secs(1)).await;
                    continue;
                }

                debug!("Raft is stopped - exiting NetworkStreaming::ws_handler()");
                break;
            }

            info!("Trying to open WebSocket stream");
            let socket = {
                match web_socket_connect::try_connect(
                    this_node,
                    &node.addr_raft,
                    &raft_type,
                    tls_config.clone(),
                    &secret,
                )
                .await
                {
                    Ok(socket) => {
                        info!("WebSocket connected successfully");
                        socket
                    }
                    Err(err) => {
                        error!("Socket connect error to node {}: {:?}", node.id, err);

                        for _ in 0..3 {
                            // if there is a network error, no reason to try too hard to connect
                            time::sleep(Duration::from_millis(heartbeat_interval)).await;

                            // make sure channel is always free
                            match rx.try_recv() {
                                Ok(req) => {
                                    let ack = match req {
                                        #[cfg(feature = "sqlite")]
                                        RaftRequest::AppendDB((ack, _)) => Some(ack),
                                        #[cfg(feature = "sqlite")]
                                        RaftRequest::VoteDB((ack, _)) => Some(ack),
                                        #[cfg(feature = "sqlite")]
                                        RaftRequest::SnapshotDB((ack, _)) => Some(ack),
                                        #[cfg(feature = "cache")]
                                        RaftRequest::AppendCache((ack, _)) => Some(ack),
                                        #[cfg(feature = "cache")]
                                        RaftRequest::VoteCache((ack, _)) => Some(ack),
                                        #[cfg(feature = "cache")]
                                        RaftRequest::SnapshotCache((ack, _)) => Some(ack),
                                        RaftRequest::StreamResponse(_) => None,
                                        RaftRequest::ReaderExit => {
                                            continue;
                                        }
                                        RaftRequest::Shutdown => {
                                            break 'outer;
                                        }
                                    };

                                    if let Some(ack) = ack {
                                        let _ = ack.send(Err(Error::Connect(err.to_string())));
                                    }
                                }
                                // `Drop` normally queues `Shutdown`, but a request may already
                                // occupy the bounded channel. Once the last sender disappears,
                                // disconnection is the equally authoritative shutdown signal.
                                Err(flume::TryRecvError::Disconnected) => break 'outer,
                                Err(flume::TryRecvError::Empty) => {}
                            }
                        }

                        continue;
                    }
                }
            };
            assert!(
                in_flight.is_empty(),
                "raft in flight buffer should always be empty when restoring a connection"
            );
            let socket_epoch = reset.epoch();

            let (tx_write, rx_write) = flume::bounded(1);
            let (tx_read, rx_read) = flume::bounded(1);

            // TODO splitting needs `unstable-split` feature right now but is about to be stabilized soon
            let (read, write) = socket.split(tokio::io::split);
            // IMPORTANT: the reader is NOT CANCEL SAFE in v0.8!
            let read = FragmentCollectorRead::new(read);

            let handle_read = task::spawn(Box::pin(Self::stream_reader(read, tx_read.clone())));
            let (tx_writer_finished, mut rx_writer_finished) = oneshot::channel();
            let handle_write = task::spawn(Box::pin(Self::stream_writer(
                write,
                rx_write,
                tx_writer_finished,
            )));

            let mut forced_reset = false;
            'connected: loop {
                let res = match next_connected_event(
                    &reset,
                    socket_epoch,
                    &rx_read,
                    &rx,
                    &mut rx_writer_finished,
                )
                .await
                {
                    ConnectedEvent::Reset => {
                        debug!("RPC future was cancelled - reconnecting Raft stream");
                        forced_reset = true;
                        break;
                    }
                    ConnectedEvent::WriterFinished(outcome) => {
                        match outcome {
                            Ok(()) => error!("Raft WebSocket writer exited while connected"),
                            Err(err) => error!("Raft WebSocket writer failed: {err}"),
                        }
                        forced_reset = true;
                        break;
                    }
                    ConnectedEvent::Request(res) => res,
                };

                let req = match res {
                    Ok(r) => r,
                    Err(err) => {
                        error!("Client stream reader error: {}", err,);

                        if rx.is_disconnected() {
                            debug!("Raft tx dropped - exiting Stream Reader");
                            shutdown = true;
                        }
                        if rx_read.is_disconnected() {
                            debug!("Client Stream reader exited - initiating shutdown + reconnect");
                        }

                        break;
                    }
                };

                match req.outbound_disposition(socket_epoch, reset.epoch()) {
                    Some(OutboundDisposition::DropCancelled) => {
                        debug!("Dropping cancelled Raft request before transport write");
                        continue;
                    }
                    Some(OutboundDisposition::Reconnect) => {
                        req.fail_outbound(Error::Connect(
                            "Raft transport reset before request write".into(),
                        ));
                        forced_reset = true;
                        break;
                    }
                    Some(OutboundDisposition::Send) | None => {}
                }

                let stream_req = match req {
                    #[cfg(feature = "sqlite")]
                    RaftRequest::AppendDB((ack, req)) => {
                        Some((ack, RaftStreamRequest::AppendDB((request_id, req))))
                    }
                    #[cfg(feature = "sqlite")]
                    RaftRequest::VoteDB((ack, req)) => {
                        Some((ack, RaftStreamRequest::VoteDB((request_id, req))))
                    }
                    #[cfg(feature = "sqlite")]
                    RaftRequest::SnapshotDB((ack, req)) => {
                        Some((ack, RaftStreamRequest::SnapshotDB((request_id, req))))
                    }

                    #[cfg(feature = "cache")]
                    RaftRequest::AppendCache((ack, req)) => {
                        Some((ack, RaftStreamRequest::AppendCache((request_id, req))))
                    }
                    #[cfg(feature = "cache")]
                    RaftRequest::VoteCache((ack, req)) => {
                        Some((ack, RaftStreamRequest::VoteCache((request_id, req))))
                    }
                    #[cfg(feature = "cache")]
                    RaftRequest::SnapshotCache((ack, req)) => {
                        Some((ack, RaftStreamRequest::SnapshotCache((request_id, req))))
                    }

                    RaftRequest::StreamResponse(resp) => {
                        match in_flight.remove(&resp.request_id) {
                            None => {
                                error!("client ack for RaftStreamResponse missing");
                            }
                            Some(ack) => {
                                if ack.send(Ok(resp.payload)).is_err() {
                                    error!("sending back stream response from raft server");
                                }
                            }
                        }
                        None
                    }

                    RaftRequest::ReaderExit => {
                        debug!(
                            "ReaderExit - Client Stream reader exited - initiating shutdown + reconnect"
                        );
                        break;
                    }
                    RaftRequest::Shutdown => {
                        debug!("RaftRequest::Shutdown");
                        shutdown = true;
                        break;
                    }
                };

                if let Some((ack, payload)) = stream_req {
                    let bytes = serialize(&payload).unwrap();

                    match enqueue_write_or_reset(
                        &tx_write,
                        WritePayload::Payload(bytes),
                        &reset,
                        socket_epoch,
                    )
                    .await
                    {
                        Ok(()) => {}
                        Err(WriteEnqueueError::Reset) => {
                            let _ = ack.send(Err(Error::Connect(
                                "Raft transport reset during request write".into(),
                            )));
                            forced_reset = true;
                            break 'connected;
                        }
                        Err(WriteEnqueueError::Disconnected(err)) => {
                            let _ = ack.send(Err(Error::Connect(format!(
                                "Error sending Write Request to WebSocket writer: {err}"
                            ))));
                            break 'connected;
                        }
                    }

                    in_flight.insert(request_id, ack);
                    request_id += 1;
                }
            }

            stop_stream_tasks(&tx_write, handle_write, handle_read, forced_reset).await;

            for (_, ack) in in_flight.drain() {
                let _ = ack.send(Err(Error::Connect("Raft WebSocket stream ended".into())));
            }
            // reset to a reasonable size for the next start to keep memory usage under control
            in_flight = HashMap::with_capacity(4);

            if shutdown {
                break;
            }
        }

        debug!("Raft Client shut down, tx closed, exiting WsHandler");
    }

    async fn stream_reader(
        mut read: FragmentCollectorRead<ReadHalf<TokioIo<Upgraded>>>,
        tx: flume::Sender<RaftRequest>,
    ) {
        while let Ok(frame) = read
            .read_frame(&mut |frame| async move {
                // TODO obligated sends should be auto ping / pong / close ? -> verify!
                debug!(
                    "Received obligated send in stream client: OpCode: {:?}: {:?}",
                    frame.opcode.clone(),
                    frame.payload
                );
                Ok::<(), Error>(())
            })
            .await
        {
            match frame.opcode {
                OpCode::Continuation => {}
                OpCode::Text => {}
                OpCode::Binary => {
                    let bytes = frame.payload.deref();
                    let payload = deserialize::<RaftStreamResponse>(bytes).unwrap();
                    if let Err(err) = tx.send_async(RaftRequest::StreamResponse(payload)).await {
                        error!(
                            "Error sending Response to Raft client stream manager: {:?}",
                            err
                        );
                    }
                }
                OpCode::Close => break,
                OpCode::Ping => {}
                OpCode::Pong => {}
            }
        }

        let _ = tx.send_async(RaftRequest::ReaderExit).await;
        debug!("Exiting Client Stream Reader");
    }

    async fn stream_writer(
        mut write: WebSocketWrite<WriteHalf<TokioIo<Upgraded>>>,
        rx: flume::Receiver<WritePayload>,
        finished: oneshot::Sender<Result<(), String>>,
    ) {
        let outcome = loop {
            let payload = match rx.recv_async().await {
                Ok(payload) => payload,
                Err(_) => break Ok(()),
            };
            match payload {
                WritePayload::Payload(bytes) => {
                    let frame = Frame::binary(Payload::from(bytes));
                    if let Err(err) = write_frame_flushed(&mut write, frame).await {
                        error!("Client Stream error: {:?}", err);
                        break Err(err.to_string());
                    }
                }
                WritePayload::Close => {
                    debug!("Received Close request in Client Stream Writer");
                    let _ = write_close_frame_flushed(
                        &mut write,
                        Frame::close(1000, b"go away"),
                    )
                    .await;
                    break Ok(());
                }
            }
        };

        let _ = finished.send(outcome);
        debug!("Exiting Client Stream Writer");
    }
}

#[allow(clippy::type_complexity)]
pub struct NetworkConnectionStreaming {
    node: Node,
    sender: flume::Sender<RaftRequest>,
    reset: Arc<ConnectionResetState>,
    task: Option<JoinHandle<()>>,
}

struct ConnectionResetGuard {
    reset: Option<(Arc<ConnectionResetState>, u64)>,
}

impl ConnectionResetGuard {
    fn new(reset: Arc<ConnectionResetState>) -> Self {
        let epoch = reset.epoch();
        Self {
            reset: Some((reset, epoch)),
        }
    }

    fn disarm(&mut self) {
        self.reset = None;
    }
}

impl Drop for ConnectionResetGuard {
    fn drop(&mut self) {
        if let Some((reset, epoch)) = self.reset.take() {
            // A reset must not share the bounded request queue. The queue may
            // still contain the RPC whose future OpenRaft just dropped; a
            // best-effort `try_send` can then lose the only instruction that
            // tears down its stale WebSocket. The epoch is durable state, so
            // cancellation cannot be missed or applied to a replacement
            // socket created after this request began.
            reset.request_reset(epoch);
        }
    }
}

impl Drop for NetworkConnectionStreaming {
    fn drop(&mut self) {
        let _ = self.sender.try_send(RaftRequest::Shutdown);
        // Dropping a Tokio JoinHandle detaches its task. That is intentional:
        // `ws_handler` must receive the shutdown request, close the WebSocket,
        // and abort its split reader and writer. Aborting the handler here
        // instead detached those child tasks while the reader still owned its
        // socket, leaking one accepted Raft connection per OpenRaft RPC.
        drop(self.task.take());
    }
}

impl NetworkConnectionStreaming {
    #[inline(always)]
    async fn send<Err>(
        &mut self,
        req: RaftRequest,
        rx: oneshot::Receiver<Result<RaftStreamResponsePayload, Error>>,
        soft_ttl: Duration,
    ) -> Result<RaftStreamResponsePayload, RPCError<NodeId, Node, Err>>
    where
        Err: std::error::Error + 'static + Clone,
    {
        #[cfg(feature = "validation-test-helpers")]
        if validation_raft_partitioned() {
            let error = std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "validation Raft partition",
            );
            return Err(RPCError::Unreachable(Unreachable::new(&error)));
        }
        tracing::debug!(
            req = debug(&req),
            "sending rpc request to {}",
            self.node.addr_raft
        );

        // OpenRaft enforces `RPCOption::hard_ttl()` by dropping this future.
        // Keep a cancellation guard alive across both enqueue and response so
        // that drop also tears down a half-open WebSocket and lets the next
        // snapshot/append attempt establish a clean stream.
        let mut reset = ConnectionResetGuard::new(Arc::clone(&self.reset));
        let result = tokio::time::timeout(soft_ttl, async {
            self.sender.send_async(req).await.map_err(|err| {
                error!(
                    "NetworkConnectionStreaming::send to node {}: {}",
                    self.node.id,
                    err.to_string()
                );
                RPCError::Unreachable(Unreachable::new(&err))
            })?;

            rx.await
                .map_err(|err| RPCError::Unreachable(Unreachable::new(&err)))?
                .map_err(|err| RPCError::Unreachable(Unreachable::new(&err)))
        })
        .await;
        match result {
            Ok(result) => {
                reset.disarm();
                result
            }
            Err(error) => Err(RPCError::Unreachable(Unreachable::new(&error))),
        }
    }
}

/// Preserve errors returned by the peer as remote Raft errors.
///
/// In particular, OpenRaft's chunked snapshot transport recognizes a remote
/// `SnapshotMismatch` and restarts the transfer at offset zero. Flattening the
/// peer response into `Unreachable` hides that recovery signal and makes every
/// later retry resume at the rejected nonzero offset.
#[cfg(any(feature = "cache", feature = "sqlite"))]
fn remote_raft_error<Err>(node: &Node, error: Err) -> RPCError<NodeId, Node, Err>
where
    Err: std::error::Error,
{
    RPCError::RemoteError(RemoteError::new_with_node(node.id, node.clone(), error))
}

/// AppendEntries performs the durable follower write Raft is waiting for.
///
/// OpenRaft already drops the network future at `hard_ttl`; cancelling the
/// same RPC at its 3/4 soft deadline turns a response that completes inside
/// the caller's accepted bound into a false outage and resets its stream.
#[cfg(any(feature = "cache", feature = "sqlite"))]
fn append_response_ttl(option: &RPCOption) -> Duration {
    option.hard_ttl()
}

#[cfg(feature = "sqlite")]
impl RaftNetwork<TypeConfigSqlite> for NetworkConnectionStreaming {
    #[tracing::instrument(level = "debug", skip_all, err(Debug))]
    async fn append_entries(
        &mut self,
        req: AppendEntriesRequest<TypeConfigSqlite>,
        option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, Node, RaftError<NodeId>>> {
        let (ack, rx) = oneshot::channel();
        match self
            .send(
                RaftRequest::AppendDB((ack, req)),
                rx,
                append_response_ttl(&option),
            )
            .await?
        {
            RaftStreamResponsePayload::AppendDB(resp) => {
                resp.map_err(|err| RPCError::Unreachable(Unreachable::new(&err)))
            }
            _ => unreachable!(),
        }
    }

    #[tracing::instrument(level = "debug", skip_all, err(Debug))]
    async fn install_snapshot(
        &mut self,
        req: InstallSnapshotRequest<TypeConfigSqlite>,
        option: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<NodeId>,
        RPCError<NodeId, Node, RaftError<NodeId, InstallSnapshotError>>,
    > {
        let (ack, rx) = oneshot::channel();
        match self
            .send(RaftRequest::SnapshotDB((ack, req)), rx, option.soft_ttl())
            .await?
        {
            RaftStreamResponsePayload::SnapshotDB(resp) => {
                resp.map_err(|err| remote_raft_error(&self.node, err))
            }
            _ => unreachable!(),
        }
    }

    #[tracing::instrument(level = "debug", skip_all, err(Debug))]
    async fn vote(
        &mut self,
        req: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, Node, RaftError<NodeId>>> {
        let (ack, rx) = oneshot::channel();
        match self
            .send(RaftRequest::VoteDB((ack, req)), rx, option.soft_ttl())
            .await?
        {
            RaftStreamResponsePayload::VoteDB(resp) => {
                resp.map_err(|err| RPCError::Unreachable(Unreachable::new(&err)))
            }
            _ => unreachable!(),
        }
    }
}

#[cfg(feature = "cache")]
impl RaftNetwork<TypeConfigKV> for NetworkConnectionStreaming {
    #[tracing::instrument(level = "debug", skip_all, err(Debug))]
    async fn append_entries(
        &mut self,
        req: AppendEntriesRequest<TypeConfigKV>,
        option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, Node, RaftError<NodeId>>> {
        let (ack, rx) = oneshot::channel();
        match self
            .send(
                RaftRequest::AppendCache((ack, req)),
                rx,
                append_response_ttl(&option),
            )
            .await?
        {
            RaftStreamResponsePayload::AppendCache(resp) => {
                resp.map_err(|err| RPCError::Unreachable(Unreachable::new(&err)))
            }
            _ => unreachable!(),
        }
    }

    #[tracing::instrument(level = "debug", skip_all, err(Debug))]
    async fn install_snapshot(
        &mut self,
        req: InstallSnapshotRequest<TypeConfigKV>,
        option: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<NodeId>,
        RPCError<NodeId, Node, RaftError<NodeId, InstallSnapshotError>>,
    > {
        let (ack, rx) = oneshot::channel();
        match self
            .send(
                RaftRequest::SnapshotCache((ack, req)),
                rx,
                option.soft_ttl(),
            )
            .await?
        {
            RaftStreamResponsePayload::SnapshotCache(resp) => {
                resp.map_err(|err| remote_raft_error(&self.node, err))
            }
            _ => unreachable!(),
        }
    }

    #[tracing::instrument(level = "debug", skip_all, err(Debug))]
    async fn vote(
        &mut self,
        req: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, Node, RaftError<NodeId>>> {
        let (ack, rx) = oneshot::channel();
        match self
            .send(RaftRequest::VoteCache((ack, req)), rx, option.soft_ttl())
            .await?
        {
            RaftStreamResponsePayload::VoteCache(resp) => {
                resp.map_err(|err| RPCError::Unreachable(Unreachable::new(&err)))
            }
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use openraft::error::{InstallSnapshotError, RaftError, SnapshotMismatch};
    use openraft::{SnapshotMeta, SnapshotSegmentId, Vote};

    use super::*;
    fn test_node() -> Node {
        Node {
            id: 7,
            addr_raft: "127.0.0.1:32401".to_owned(),
            addr_api: "127.0.0.1:32402".to_owned(),
        }
    }

    fn test_snapshot_meta() -> SnapshotMeta<NodeId, Node> {
        SnapshotMeta {
            last_log_id: None,
            last_membership: Default::default(),
            snapshot_id: "snapshot".to_owned(),
        }
    }

    fn mismatch_at(offset: u64) -> RaftError<NodeId, InstallSnapshotError> {
        RaftError::APIError(InstallSnapshotError::SnapshotMismatch(SnapshotMismatch {
            expect: SnapshotSegmentId::from(("snapshot", 0)),
            got: SnapshotSegmentId::from(("snapshot", offset)),
        }))
    }

    #[tokio::test]
    async fn dropping_connection_allows_handler_to_process_shutdown() {
        let (sender, receiver) = flume::bounded(1);
        let release = Arc::new(Notify::new());
        let stopped = Arc::new(Notify::new());
        let task = tokio::spawn({
            let release = Arc::clone(&release);
            let stopped = Arc::clone(&stopped);
            async move {
                release.notified().await;
                assert!(matches!(
                    receiver.recv_async().await,
                    Ok(RaftRequest::Shutdown)
                ));
                stopped.notify_one();
            }
        });
        let connection = NetworkConnectionStreaming {
            node: Node {
                id: 2,
                addr_raft: "127.0.0.1:32401".to_owned(),
                addr_api: "127.0.0.1:32402".to_owned(),
            },
            sender,
            reset: Arc::new(ConnectionResetState::default()),
            task: Some(task),
        };

        drop(connection);
        release.notify_one();

        tokio::time::timeout(Duration::from_secs(1), stopped.notified())
            .await
            .expect("the detached handler must consume shutdown and exit cleanly");
    }

    #[tokio::test]
    async fn handler_coordinator_consumes_retained_reset_when_request_queue_is_full() {
        let (sender, receiver) = flume::bounded(1);
        let (_reader_sender, reader_receiver) = flume::bounded(1);
        sender
            .try_send(RaftRequest::Shutdown)
            .expect("fill the bounded request queue");
        let reset = Arc::new(ConnectionResetState::default());
        let socket_epoch = reset.epoch();
        let guard = ConnectionResetGuard::new(Arc::clone(&reset));
        let (_writer_finished, mut writer_finished) = oneshot::channel();

        drop(guard);

        let event = tokio::time::timeout(
            Duration::from_secs(1),
            next_connected_event(
                &reset,
                socket_epoch,
                &reader_receiver,
                &receiver,
                &mut writer_finished,
            ),
        )
        .await
        .expect("the handler's reconnect consumer must observe the retained reset");
        assert!(matches!(event, ConnectedEvent::Reset));
        assert!(matches!(
            receiver.recv_async().await,
            Ok(RaftRequest::Shutdown)
        ));
    }

    #[tokio::test]
    async fn handler_coordinator_observes_writer_failure_while_reader_is_pending() {
        let (_request_sender, request_receiver) = flume::bounded(1);
        let (_reader_sender, reader_receiver) = flume::bounded(1);
        let reset = ConnectionResetState::default();
        let socket_epoch = reset.epoch();
        let (writer_finished, mut writer_result) = oneshot::channel();

        writer_finished
            .send(Err("injected flush failure".to_owned()))
            .expect("writer outcome receiver must remain open");

        let event = tokio::time::timeout(
            Duration::from_secs(1),
            next_connected_event(
                &reset,
                socket_epoch,
                &reader_receiver,
                &request_receiver,
                &mut writer_result,
            ),
        )
        .await
        .expect("writer failure must wake the connection supervisor");
        assert!(matches!(
            event,
            ConnectedEvent::WriterFinished(Err(ref err)) if err == "injected flush failure"
        ));
    }

    #[tokio::test]
    async fn handler_coordinator_treats_writer_panic_as_terminal() {
        let (_request_sender, request_receiver) = flume::bounded(1);
        let (_reader_sender, reader_receiver) = flume::bounded(1);
        let reset = ConnectionResetState::default();
        let socket_epoch = reset.epoch();
        let (writer_finished, mut writer_result) = oneshot::channel::<Result<(), String>>();
        drop(writer_finished);

        let event = tokio::time::timeout(
            Duration::from_secs(1),
            next_connected_event(
                &reset,
                socket_epoch,
                &reader_receiver,
                &request_receiver,
                &mut writer_result,
            ),
        )
        .await
        .expect("dropped writer outcome must wake the connection supervisor");
        assert!(matches!(
            event,
            ConnectedEvent::WriterFinished(Err(ref err))
                if err.contains("without reporting an outcome")
        ));
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn replacement_socket_drops_cancelled_request_after_consuming_reset() {
        let reset = Arc::new(ConnectionResetState::default());
        let stale_socket_epoch = reset.epoch();
        let (cancelled_ack, cancelled_rx) = oneshot::channel();
        let cancelled = RaftRequest::SnapshotDB((
            cancelled_ack,
            InstallSnapshotRequest {
                vote: Vote::new_committed(1, 1),
                meta: test_snapshot_meta(),
                offset: 3,
                data: b"old".to_vec(),
                done: false,
            },
        ));
        drop(cancelled_rx);

        let guard = ConnectionResetGuard::new(Arc::clone(&reset));
        drop(guard);
        reset.changed_since(stale_socket_epoch).await;

        let replacement_socket_epoch = reset.epoch();
        assert_eq!(
            cancelled.outbound_disposition(replacement_socket_epoch, reset.epoch()),
            Some(OutboundDisposition::DropCancelled)
        );

        let (live_ack, _live_rx) = oneshot::channel();
        let live = RaftRequest::SnapshotDB((
            live_ack,
            InstallSnapshotRequest {
                vote: Vote::new_committed(1, 1),
                meta: test_snapshot_meta(),
                offset: 0,
                data: b"new".to_vec(),
                done: false,
            },
        ));
        assert_eq!(
            live.outbound_disposition(replacement_socket_epoch, reset.epoch()),
            Some(OutboundDisposition::Send)
        );
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn live_request_on_stale_socket_requires_reconnect() {
        let reset = ConnectionResetState::default();
        let socket_epoch = reset.epoch();
        reset.request_reset(socket_epoch);
        let (ack, _rx) = oneshot::channel();
        let request = RaftRequest::SnapshotDB((
            ack,
            InstallSnapshotRequest {
                vote: Vote::new_committed(1, 1),
                meta: test_snapshot_meta(),
                offset: 0,
                data: Vec::new(),
                done: true,
            },
        ));

        assert_eq!(
            request.outbound_disposition(socket_epoch, reset.epoch()),
            Some(OutboundDisposition::Reconnect)
        );
    }

    #[tokio::test]
    async fn reset_interrupts_write_enqueue_under_backpressure() {
        let (tx_write, _rx_write) = flume::bounded(1);
        tx_write
            .try_send(WritePayload::Payload(b"blocked".to_vec()))
            .expect("fill writer queue");
        let reset = ConnectionResetState::default();
        let socket_epoch = reset.epoch();
        let enqueue = enqueue_write_or_reset(
            &tx_write,
            WritePayload::Payload(b"waiting".to_vec()),
            &reset,
            socket_epoch,
        );
        tokio::pin!(enqueue);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(
            std::future::Future::poll(enqueue.as_mut(), &mut context).is_pending(),
            "the write enqueue must be blocked before reset"
        );
        reset.request_reset(socket_epoch);

        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), &mut enqueue).await,
            Ok(Err(WriteEnqueueError::Reset))
        ));
    }

    #[tokio::test]
    async fn forced_reset_cleanup_does_not_wait_for_full_writer_queue() {
        let (tx_write, _rx_write) = flume::bounded(1);
        tx_write
            .try_send(WritePayload::Payload(b"blocked".to_vec()))
            .expect("fill writer queue");
        let handle_write = tokio::spawn(std::future::pending());
        let handle_read = tokio::spawn(std::future::pending());

        tokio::time::timeout(
            Duration::from_secs(1),
            stop_stream_tasks(&tx_write, handle_write, handle_read, true),
        )
        .await
        .expect("forced reset cleanup must not queue behind the writer");
    }

    #[test]
    fn append_response_uses_the_callers_whole_accepted_deadline() {
        let option = RPCOption::new(Duration::from_millis(800));

        assert_eq!(option.soft_ttl(), Duration::from_millis(600));
        assert_eq!(append_response_ttl(&option), Duration::from_millis(800));
    }

    #[test]
    fn snapshot_mismatch_remains_a_remote_api_error() {
        let node = test_node();
        let mismatch = SnapshotMismatch {
            expect: SnapshotSegmentId::from(("snapshot", 0)),
            got: SnapshotSegmentId::from(("snapshot", 6_291_456)),
        };

        let error: RPCError<NodeId, Node, RaftError<NodeId, InstallSnapshotError>> =
            remote_raft_error(
                &node,
                RaftError::APIError(InstallSnapshotError::SnapshotMismatch(mismatch.clone())),
            );

        assert!(matches!(
            error,
            RPCError::RemoteError(RemoteError {
                target: 7,
                target_node: Some(target_node),
                source: RaftError::APIError(InstallSnapshotError::SnapshotMismatch(actual)),
            }) if target_node == node && actual == mismatch
        ));
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_install_snapshot_preserves_mismatch_for_offset_reset() {
        let (sender, receiver) = flume::bounded(1);
        let mut network = NetworkConnectionStreaming {
            node: test_node(),
            sender,
            reset: Arc::new(ConnectionResetState::default()),
            task: None,
        };
        let responder = tokio::spawn(async move {
            let (ack, request) = match receiver
                .recv_async()
                .await
                .expect("receive SQLite snapshot request")
            {
                RaftRequest::SnapshotDB(request) => request,
                request => panic!("unexpected SQLite Raft request: {request:?}"),
            };
            assert_eq!(request.offset, 4);
            ack.send(Ok(RaftStreamResponsePayload::SnapshotDB(Err(
                mismatch_at(request.offset),
            ))))
            .expect("return SQLite snapshot response");
        });

        let error = <NetworkConnectionStreaming as RaftNetwork<TypeConfigSqlite>>::install_snapshot(
            &mut network,
            InstallSnapshotRequest {
                vote: Vote::new_committed(1, 1),
                meta: test_snapshot_meta(),
                offset: 4,
                data: b"efgh".to_vec(),
                done: true,
            },
            RPCOption::new(Duration::from_millis(500)),
        )
        .await;
        responder.await.expect("join SQLite snapshot responder");

        assert!(matches!(
            error,
            Err(RPCError::RemoteError(RemoteError {
                target: 7,
                target_node: Some(target_node),
                source: RaftError::APIError(InstallSnapshotError::SnapshotMismatch(
                    SnapshotMismatch { expect, got },
                )),
            })) if target_node == test_node()
                && expect == SnapshotSegmentId::from(("snapshot", 0))
                && got == SnapshotSegmentId::from(("snapshot", 4))
        ));
    }

    #[cfg(feature = "cache")]
    #[tokio::test]
    async fn cache_install_snapshot_preserves_mismatch_for_offset_reset() {
        let (sender, receiver) = flume::bounded(1);
        let mut network = NetworkConnectionStreaming {
            node: test_node(),
            sender,
            reset: Arc::new(ConnectionResetState::default()),
            task: None,
        };
        let responder = tokio::spawn(async move {
            let (ack, request) = match receiver
                .recv_async()
                .await
                .expect("receive cache snapshot request")
            {
                RaftRequest::SnapshotCache(request) => request,
                request => panic!("unexpected cache Raft request: {request:?}"),
            };
            assert_eq!(request.offset, 4);
            ack.send(Ok(RaftStreamResponsePayload::SnapshotCache(Err(
                mismatch_at(request.offset),
            ))))
            .expect("return cache snapshot response");
        });

        let error = <NetworkConnectionStreaming as RaftNetwork<TypeConfigKV>>::install_snapshot(
            &mut network,
            InstallSnapshotRequest {
                vote: Vote::new_committed(1, 1),
                meta: test_snapshot_meta(),
                offset: 4,
                data: b"efgh".to_vec(),
                done: true,
            },
            RPCOption::new(Duration::from_millis(500)),
        )
        .await;
        responder.await.expect("join cache snapshot responder");

        assert!(matches!(
            error,
            Err(RPCError::RemoteError(RemoteError {
                target: 7,
                target_node: Some(target_node),
                source: RaftError::APIError(InstallSnapshotError::SnapshotMismatch(
                    SnapshotMismatch { expect, got },
                )),
            })) if target_node == test_node()
                && expect == SnapshotSegmentId::from(("snapshot", 0))
                && got == SnapshotSegmentId::from(("snapshot", 4))
        ));
    }
}
