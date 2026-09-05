use crate::app_state::RaftType;
use crate::helpers::deserialize;
use crate::network::frame_io::{write_close_frame_flushed, write_frame_flushed};
use crate::network::api::{ApiStreamResponse, ApiStreamResponsePayload};
use crate::network::{serialize_network, web_socket_connect};
use crate::{Client, Error, Node, NodeId};
use fastwebsockets::{FragmentCollectorRead, Frame, OpCode, Payload, WebSocket, WebSocketWrite};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::oneshot::Sender;
use tokio::sync::{RwLock, oneshot};
use tokio::task::JoinHandle;
use tokio::{select, task, time};
use tracing::{debug, error, info};

#[cfg(any(feature = "sqlite", feature = "cache"))]
use crate::network::api::{ApiStreamRequest, ApiStreamRequestPayload};
#[cfg(feature = "cache")]
use crate::store::state_machine::memory::state_machine::CacheRequest;
#[cfg(feature = "sqlite")]
use crate::{migration::Migration, store::state_machine::sqlite::state_machine::Query};

#[derive(Debug)]
pub(crate) enum ClientStreamReq {
    // coming from the `DbClient`
    #[cfg(feature = "sqlite")]
    Execute(ClientExecutePayload),
    #[cfg(feature = "sqlite")]
    ExecuteReturning(ClientExecutePayload),
    #[cfg(feature = "sqlite")]
    Transaction(ClientTransactionPayload),
    #[cfg(feature = "sqlite")]
    Query(ClientQueryPayload),
    #[cfg(feature = "sqlite")]
    QueryConsistent(ClientQueryPayload),
    #[cfg(feature = "sqlite")]
    Batch(ClientBatchPayload),
    #[cfg(feature = "sqlite")]
    Migrate(ClientMigratePayload),

    #[cfg(feature = "backup")]
    Backup(ClientBackupPayload),

    #[cfg(feature = "cache")]
    KV(ClientKVPayload),
    #[cfg(feature = "cache")]
    KVGet(ClientKVPayload),

    #[cfg(feature = "dlock")]
    LockAwait(ClientKVPayload),

    #[cfg(feature = "listen_notify_local")]
    Notify(ClientKVPayload),

    Shutdown,

    // coming from the WebSocket reader
    StreamResponse(ApiStreamResponse),
    StreamClosed,
    CleanupBuffer,

    // The embedded dashboard still reports a local ForwardToLeader through
    // the shared AppState queue. Retried Client operations use the dedicated
    // priority control channel below.
    #[cfg(feature = "dashboard")]
    LeaderChange(
        (Option<u64>, Option<Node>),
        Option<oneshot::Sender<()>>,
    ),
    /// Advance only within the caller-configured proxy pool. The stream
    /// manager acknowledges after closing the old stream and failing every
    /// in-flight request without replay.
    RotateProxy(oneshot::Sender<()>),
}

/// Priority control message consumed even while the manager is opening its
/// current WebSocket. Keeping leader handoff off the one-slot request queue
/// prevents application traffic or a slow obsolete handshake from consuming
/// the bounded recovery window.
#[derive(Debug)]
pub(crate) struct ClientLeaderChange {
    pub(crate) leader_id: NodeId,
    pub(crate) node: Node,
    pub(crate) ready: Option<oneshot::Sender<()>>,
}

#[derive(Default)]
struct PendingLeaderReady {
    target: Option<(NodeId, String)>,
    waiters: Vec<oneshot::Sender<()>>,
}

impl PendingLeaderReady {
    fn register(
        &mut self,
        target: (NodeId, String),
        ready: Option<oneshot::Sender<()>>,
    ) -> bool {
        self.prune_closed();
        if ready.as_ref().is_some_and(oneshot::Sender::is_closed) {
            // Its bounded caller expired while this control message waited to
            // be received. It no longer has authority to redirect the stream.
            return false;
        }
        if self.target.as_ref() != Some(&target) {
            // Dropping superseded senders makes the old recovery generation
            // fail instead of acknowledging it on a stream to another node.
            self.waiters.clear();
            self.target = Some(target);
        }
        if let Some(ready) = ready {
            self.waiters.push(ready);
        }
        true
    }

    fn prune_closed(&mut self) {
        self.waiters.retain(|ready| !ready.is_closed());
        if self.waiters.is_empty() {
            self.target = None;
        }
    }

    fn resolve_connected_target(&mut self, connected: &(NodeId, String)) {
        self.prune_closed();
        if self.target.as_ref().is_some_and(|target| target != connected) {
            // The requested target failed and authenticated discovery selected
            // another live leader. Fail the obsolete handoff rather than
            // dropping the valid stream forever or falsely acknowledging it.
            self.waiters.clear();
            self.target = None;
        }
    }

    fn acknowledge(&mut self, connected: &(NodeId, String)) -> bool {
        if self.target.as_ref() != Some(connected) {
            return false;
        }
        for ready in self.waiters.drain(..) {
            let _ = ready.send(());
        }
        self.target = None;
        true
    }
}

fn leader_handoff_restarts_connection(
    connecting: &(NodeId, String),
    incoming: &(NodeId, String),
) -> bool {
    connecting != incoming
}

impl ClientStreamReq {
    fn fail_on_shutdown(self) {
        let error = || Error::Connect("client stream manager stopped".into());
        match self {
            #[cfg(feature = "sqlite")]
            Self::Execute(payload) | Self::ExecuteReturning(payload) => {
                let _ = payload.ack.send(Err(error()));
            }
            #[cfg(feature = "sqlite")]
            Self::Transaction(payload) => {
                let _ = payload.ack.send(Err(error()));
            }
            #[cfg(feature = "sqlite")]
            Self::Query(payload) | Self::QueryConsistent(payload) => {
                let _ = payload.ack.send(Err(error()));
            }
            #[cfg(feature = "sqlite")]
            Self::Batch(payload) => {
                let _ = payload.ack.send(Err(error()));
            }
            #[cfg(feature = "sqlite")]
            Self::Migrate(payload) => {
                let _ = payload.ack.send(Err(error()));
            }
            #[cfg(feature = "backup")]
            Self::Backup(payload) => {
                let _ = payload.ack.send(Err(error()));
            }
            #[cfg(feature = "cache")]
            Self::KV(payload) | Self::KVGet(payload) => {
                let _ = payload.ack.send(Err(error()));
            }
            #[cfg(feature = "dlock")]
            Self::LockAwait(payload) => {
                let _ = payload.ack.send(Err(error()));
            }
            #[cfg(feature = "listen_notify_local")]
            Self::Notify(payload) => {
                let _ = payload.ack.send(Err(error()));
            }
            Self::Shutdown
            | Self::StreamResponse(_)
            | Self::StreamClosed
            | Self::CleanupBuffer
            | Self::RotateProxy(_) => {}
            #[cfg(feature = "dashboard")]
            Self::LeaderChange(_, _) => {}
        }
    }
}

#[cfg(feature = "sqlite")]
#[derive(Debug)]
pub struct ClientExecutePayload {
    pub request_id: usize,
    pub sql: Query,
    pub ack: oneshot::Sender<Result<ApiStreamResponsePayload, Error>>,
}

#[cfg(feature = "sqlite")]
#[derive(Debug)]
pub struct ClientTransactionPayload {
    pub request_id: usize,
    pub queries: Vec<Query>,
    pub ack: oneshot::Sender<Result<ApiStreamResponsePayload, Error>>,
}

#[cfg(feature = "sqlite")]
#[derive(Debug)]
pub struct ClientQueryPayload {
    pub request_id: usize,
    pub query: Query,
    pub ack: oneshot::Sender<Result<ApiStreamResponsePayload, Error>>,
}

#[cfg(feature = "sqlite")]
#[derive(Debug)]
pub struct ClientBatchPayload {
    pub request_id: usize,
    pub sql: std::borrow::Cow<'static, str>,
    pub ack: oneshot::Sender<Result<ApiStreamResponsePayload, Error>>,
}

#[cfg(feature = "sqlite")]
#[derive(Debug)]
pub struct ClientMigratePayload {
    pub request_id: usize,
    pub migrations: Vec<Migration>,
    pub ack: oneshot::Sender<Result<ApiStreamResponsePayload, Error>>,
}

#[cfg(feature = "backup")]
#[derive(Debug)]
pub struct ClientBackupPayload {
    pub request_id: usize,
    pub node_id: NodeId,
    pub ts: i64,
    pub ack: oneshot::Sender<Result<ApiStreamResponsePayload, Error>>,
}

#[cfg(feature = "cache")]
#[derive(Debug)]
pub struct ClientKVPayload {
    pub request_id: usize,
    pub cache_req: CacheRequest,
    pub ack: oneshot::Sender<Result<ApiStreamResponsePayload, Error>>,
}

#[derive(Debug)]
enum WritePayload {
    Payload(Vec<u8>),
    Close,
}

const CLIENT_STREAM_RETRY_DELAY: Duration = Duration::from_secs(1);

fn reconnect_delay(
    previous_leader: &(NodeId, String),
    current_leader: &(NodeId, String),
) -> Duration {
    if previous_leader == current_leader {
        CLIENT_STREAM_RETRY_DELAY
    } else {
        Duration::ZERO
    }
}

impl Client {
    pub(crate) fn open_stream(
        &self,
        secret: Vec<u8>,
        leader: Arc<RwLock<(NodeId, String)>>,
        rx_client_stream: flume::Receiver<ClientStreamReq>,
        rx_leader_change: flume::Receiver<ClientLeaderChange>,
        raft_type: RaftType,
    ) {
        let handle = task::spawn(Box::pin(client_stream(
            self.clone(),
            secret,
            leader,
            rx_client_stream,
            rx_leader_change,
            raft_type,
            self.inner.stream_shutdown.subscribe(),
        )));
        self.inner
            .background_handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(handle);
    }
}

/// Manager task which handles connection creation, split into sender / receiver, keeps the state,
/// handles reconnects and leader switches.
async fn client_stream(
    client: Client,
    secret: Vec<u8>,
    leader: Arc<RwLock<(NodeId, String)>>,
    rx_req: flume::Receiver<ClientStreamReq>,
    rx_leader: flume::Receiver<ClientLeaderChange>,
    raft_type: RaftType,
    mut stream_shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut in_flight: HashMap<usize, oneshot::Sender<Result<ApiStreamResponsePayload, Error>>> =
        HashMap::with_capacity(8);
    let mut in_flight_buf: HashMap<
        usize,
        oneshot::Sender<Result<ApiStreamResponsePayload, Error>>,
    > = HashMap::new();

    let mut shutdown = false;
    let mut pending_leader_ready = PendingLeaderReady::default();
    // DB and cache managers each own their cursor. An index, rather than an
    // address lookup, keeps duplicate configured endpoints from pinning a
    // stream forever.
    let mut proxy_index = 0;

    'manager: loop {
        pending_leader_ready.prune_closed();
        let connecting_target = leader.read().await.clone();
        let connection = try_connect(
                &leader,
                &raft_type,
                client.inner.tls_config.clone(),
                &secret,
            );
        tokio::pin!(connection);
        let connection = loop {
            select! {
                _ = stream_shutdown.changed() => {
                    fail_client_stream_shutdown(&mut in_flight, &mut in_flight_buf, &rx_req);
                    return;
                }
                change = rx_leader.recv_async() => {
                    let Ok(ClientLeaderChange { leader_id, node, ready }) = change else {
                        fail_client_stream_shutdown(&mut in_flight, &mut in_flight_buf, &rx_req);
                        return;
                    };
                    let target = (leader_id, node.addr_api.clone());
                    if !pending_leader_ready.register(target.clone(), ready) {
                        continue;
                    }
                    if !leader_handoff_restarts_connection(&connecting_target, &target) {
                        // A duplicate for the stream already being opened
                        // shares that handshake instead of resetting its
                        // five-second attempt near the deadline.
                        continue;
                    }
                    update_leader(&leader, Some(leader_id), Some(node)).await;
                    continue 'manager;
                }
                connection = &mut connection => break connection,
            }
        };
        let (ws, connected_leader) = match connection {
            Ok((ws, connected_leader)) => {
                info!(
                    "Client API WebSocket to {} opened successfully",
                    connected_leader.1
                );
                (ws, connected_leader)
            }
            Err(err) => {
                let previous_leader = leader.read().await.clone();
                let mut retry_delay = CLIENT_STREAM_RETRY_DELAY;
                if client.inner.proxy_mode {
                    // No request was dispatched, so every handshake/TLS/API
                    // error is safe to recover at the next configured proxy.
                    rotate_proxy_endpoint(&client, &leader, &mut proxy_index).await;
                } else if let Error::Connect(_) = &err {
                    select! {
                        _ = stream_shutdown.changed() => {
                            fail_client_stream_shutdown(
                                &mut in_flight,
                                &mut in_flight_buf,
                                &rx_req,
                            );
                            return;
                        }
                        () = client.find_set_active_leader() => {}
                    }
                    let current_leader = leader.read().await.clone();
                    retry_delay = reconnect_delay(&previous_leader, &current_leader);
                }

                if !retry_delay.is_zero() {
                    select! {
                        _ = stream_shutdown.changed() => {
                            fail_client_stream_shutdown(
                                &mut in_flight,
                                &mut in_flight_buf,
                                &rx_req,
                            );
                            return;
                        }
                        () = time::sleep(retry_delay) => {}
                    }
                }
                error!(
                    "Could not connect Client API WebSocket to {}: {}",
                    leader.read().await.1,
                    err
                );
                continue;
            }
        };

        pending_leader_ready.resolve_connected_target(&connected_leader);

        let (tx_write, rx_write) = flume::bounded(1);
        let (tx_read, rx_read) = flume::bounded(1);

        // TODO splitting needs `unstable-split` feature right now but is about to be stabilized soon
        let (rx, write) = ws.split(tokio::io::split);
        // IMPORTANT: the reader is NOT CANCEL SAFE in v0.8!
        let read = FragmentCollectorRead::new(rx);

        let handle_read = task::spawn(stream_reader(read, tx_read.clone()));
        let (tx_writer_finished, mut rx_writer_finished) = oneshot::channel();
        let handle_write = task::spawn(stream_writer(write, rx_write, tx_writer_finished));

        pending_leader_ready.acknowledge(&connected_leader);

        let handle_buf = cleanup_buffer_timeout(tx_read, 10);
        let mut awaiting_timeout = true;
        let mut rotate_after_disconnect = false;

        loop {
            let res = select! {
                biased;
                _ = stream_shutdown.changed() => {
                    shutdown = true;
                    None
                }
                writer_result = &mut rx_writer_finished => {
                    match writer_result {
                        Ok(Ok(())) => error!("API WebSocket writer exited while connected"),
                        Ok(Err(err)) => error!("API WebSocket writer failed: {err}"),
                        Err(_) => error!("API WebSocket writer task exited without an outcome"),
                    }
                    rotate_after_disconnect = client.inner.proxy_mode;
                    None
                }
                res = rx_read.recv_async() => Some(res),
                res = rx_req.recv_async() => Some(res),
                change = rx_leader.recv_async() => {
                    let Ok(ClientLeaderChange { leader_id, node, ready }) = change else {
                        let _ = tx_write.try_send(WritePayload::Close);
                        shutdown = true;
                        break;
                    };
                    let target = (leader_id, node.addr_api.clone());
                    if target == connected_leader {
                        if let Some(ready) = ready {
                            let _ = ready.send(());
                        }
                        continue;
                    }
                    if !pending_leader_ready.register(target, ready) {
                        continue;
                    }
                    let _ = tx_write.try_send(WritePayload::Close);
                    update_leader(&leader, Some(leader_id), Some(node)).await;
                    for (_, ack) in in_flight.drain() {
                        let _ = ack.send(Err(Error::LeaderChange(
                            "Action not allowed, Raft leader has changed".into(),
                        )));
                    }
                    break;
                }
            };
            let Some(res) = res else {
                let _ = tx_write.try_send(WritePayload::Close);
                break;
            };
            let req = match res {
                Ok(req) => req,
                Err(err) => {
                    error!("Client stream reader error: {}", err,);
                    if rx_req.is_disconnected() {
                        let _ = tx_write.try_send(WritePayload::Close);
                        shutdown = true;
                    } else if client.inner.proxy_mode {
                        rotate_after_disconnect = true;
                    }
                    break;
                }
            };

            let payload = match req {
                #[cfg(feature = "sqlite")]
                ClientStreamReq::Execute(ClientExecutePayload {
                    request_id,
                    sql,
                    ack,
                }) => {
                    let req = ApiStreamRequest {
                        request_id,
                        payload: ApiStreamRequestPayload::Execute(sql),
                    };
                    Some((
                        WritePayload::Payload(serialize_network(&req)),
                        req.request_id,
                        ack,
                    ))
                }

                #[cfg(feature = "sqlite")]
                ClientStreamReq::ExecuteReturning(ClientExecutePayload {
                    request_id,
                    sql,
                    ack,
                }) => {
                    let req = ApiStreamRequest {
                        request_id,
                        payload: ApiStreamRequestPayload::ExecuteReturning(sql),
                    };
                    Some((
                        WritePayload::Payload(serialize_network(&req)),
                        request_id,
                        ack,
                    ))
                }

                #[cfg(feature = "sqlite")]
                ClientStreamReq::Transaction(ClientTransactionPayload {
                    request_id,
                    queries,
                    ack,
                }) => {
                    let req = ApiStreamRequest {
                        request_id,
                        payload: ApiStreamRequestPayload::Transaction(queries),
                    };
                    Some((
                        WritePayload::Payload(serialize_network(&req)),
                        request_id,
                        ack,
                    ))
                }

                #[cfg(feature = "sqlite")]
                ClientStreamReq::Query(ClientQueryPayload {
                    request_id,
                    query,
                    ack,
                }) => {
                    let req = ApiStreamRequest {
                        request_id,
                        payload: ApiStreamRequestPayload::Query(query),
                    };
                    Some((
                        WritePayload::Payload(serialize_network(&req)),
                        request_id,
                        ack,
                    ))
                }

                #[cfg(feature = "sqlite")]
                ClientStreamReq::QueryConsistent(ClientQueryPayload {
                    request_id,
                    query,
                    ack,
                }) => {
                    let req = ApiStreamRequest {
                        request_id,
                        payload: ApiStreamRequestPayload::QueryConsistent(query),
                    };
                    Some((
                        WritePayload::Payload(serialize_network(&req)),
                        request_id,
                        ack,
                    ))
                }

                #[cfg(feature = "sqlite")]
                ClientStreamReq::Batch(ClientBatchPayload {
                    request_id,
                    sql,
                    ack,
                }) => {
                    let req = ApiStreamRequest {
                        request_id,
                        payload: ApiStreamRequestPayload::Batch(sql),
                    };
                    Some((
                        WritePayload::Payload(serialize_network(&req)),
                        request_id,
                        ack,
                    ))
                }

                #[cfg(feature = "sqlite")]
                ClientStreamReq::Migrate(ClientMigratePayload {
                    request_id,
                    migrations,
                    ack,
                }) => {
                    let req = ApiStreamRequest {
                        request_id,
                        payload: ApiStreamRequestPayload::Migrate(migrations),
                    };
                    Some((
                        WritePayload::Payload(serialize_network(&req)),
                        request_id,
                        ack,
                    ))
                }

                #[cfg(feature = "backup")]
                ClientStreamReq::Backup(ClientBackupPayload {
                    request_id,
                    node_id,
                    ts,
                    ack,
                }) => {
                    let req = ApiStreamRequest {
                        request_id,
                        payload: ApiStreamRequestPayload::Backup((node_id, ts)),
                    };
                    Some((
                        WritePayload::Payload(serialize_network(&req)),
                        request_id,
                        ack,
                    ))
                }

                #[cfg(feature = "cache")]
                ClientStreamReq::KV(ClientKVPayload {
                    request_id,
                    cache_req,
                    ack,
                }) => {
                    let req = ApiStreamRequest {
                        request_id,
                        payload: ApiStreamRequestPayload::KV(cache_req),
                    };
                    Some((
                        WritePayload::Payload(serialize_network(&req)),
                        request_id,
                        ack,
                    ))
                }

                #[cfg(feature = "cache")]
                ClientStreamReq::KVGet(ClientKVPayload {
                    request_id,
                    cache_req,
                    ack,
                }) => {
                    let req = ApiStreamRequest {
                        request_id,
                        payload: ApiStreamRequestPayload::KVGet(cache_req),
                    };
                    Some((
                        WritePayload::Payload(serialize_network(&req)),
                        request_id,
                        ack,
                    ))
                }

                #[cfg(feature = "dlock")]
                ClientStreamReq::LockAwait(ClientKVPayload {
                    request_id,
                    cache_req,
                    ack,
                }) => {
                    let req = ApiStreamRequest {
                        request_id,
                        payload: ApiStreamRequestPayload::LockAwait(cache_req),
                    };
                    Some((
                        WritePayload::Payload(serialize_network(&req)),
                        request_id,
                        ack,
                    ))
                }

                #[cfg(feature = "listen_notify_local")]
                ClientStreamReq::Notify(ClientKVPayload {
                    request_id,
                    cache_req,
                    ack,
                }) => {
                    let req = ApiStreamRequest {
                        request_id,
                        payload: ApiStreamRequestPayload::Notify(cache_req),
                    };
                    Some((
                        WritePayload::Payload(serialize_network(&req)),
                        request_id,
                        ack,
                    ))
                }

                #[cfg(feature = "dashboard")]
                ClientStreamReq::LeaderChange((node_id, node), ready) => {
                    if leader_change_matches_connection(&connected_leader, node_id, node.as_ref()) {
                        // A detached recovery from an earlier timed-out request
                        // may finish after this stream already reached the same
                        // leader. Closing it would fail unrelated in-flight
                        // work and turn successful recovery into LeaderChange.
                        if let Some(ready) = ready {
                            let _ = ready.send(());
                        }
                        continue;
                    }
                    // ignore result just in case the writer has already exited anyway
                    let _ = tx_write.try_send(WritePayload::Close);

                    // If we don't receive a value here, we expect the lock to
                    // have been updated already somewhere else
                    let ready_target = node_id
                        .zip(node.as_ref())
                        .map(|(node_id, node)| (node_id, node.addr_api.clone()));
                    update_leader(&leader, node_id, node).await;
                    if let (Some(target), Some(ready)) = (ready_target, ready) {
                        let _ = pending_leader_ready.register(target, Some(ready));
                    }

                    // in case of a leader change, we should not use the in flight buffer
                    // since no modifying write after this error will be Ok(_) anyway.
                    for (_, ack) in in_flight.drain() {
                        let _ = ack.send(Err(Error::LeaderChange(
                            "Action not allowed, Raft leader has changed".into(),
                        )));
                    }
                    break;
                }

                ClientStreamReq::RotateProxy(ack) => {
                    // ForwardToLeader proves the triggering request was not
                    // accepted. This stream task owns its DB/cache cursor:
                    // close the old connection, fail other in-flight work
                    // without replay, then advance inside the configured pool.
                    // Closing is best effort: acknowledgement and rotation
                    // must not queue behind a writer blocked on the old link.
                    let _ = tx_write.try_send(WritePayload::Close);
                    for (_, request_ack) in in_flight.drain().chain(in_flight_buf.drain()) {
                        let _ = request_ack.send(Err(Error::LeaderChange(
                            "Action not allowed, proxy endpoint has changed".into(),
                        )));
                    }
                    rotate_proxy_endpoint(&client, &leader, &mut proxy_index).await;
                    let _ = ack.send(());
                    break;
                }

                ClientStreamReq::StreamResponse(resp) => {
                    try_forward_response(
                        &mut in_flight,
                        &mut in_flight_buf,
                        awaiting_timeout,
                        resp,
                    )
                    .await;
                    None
                }

                ClientStreamReq::StreamClosed => {
                    rotate_after_disconnect = client.inner.proxy_mode;
                    break;
                }

                ClientStreamReq::CleanupBuffer => {
                    for (_, ack) in in_flight_buf {
                        let _ = ack.send(Err(Error::Connect("request timed out".to_string())));
                    }
                    in_flight_buf = HashMap::new();
                    awaiting_timeout = false;
                    None
                }

                ClientStreamReq::Shutdown => {
                    shutdown = true;
                    break;
                }
            };

            if let Some((payload, request_id, ack)) = payload {
                let write_result = select! {
                    _ = stream_shutdown.changed() => {
                        shutdown = true;
                        None
                    }
                    result = tx_write.send_async(payload) => Some(result),
                };
                let Some(write_result) = write_result else {
                    let _ = ack.send(Err(Error::Connect("client stream manager stopped".into())));
                    break;
                };
                match write_result {
                    Ok(_) => {
                        in_flight.insert(request_id, ack);
                    }
                    Err(err) => {
                        error!("Error sending txn request to writer: {}", err);
                        let _ =
                            ack.send(Err(Error::Connect("Connection to Raft leader lost".into())));
                        rotate_after_disconnect = client.inner.proxy_mode;
                        break;
                    }
                }
            }
        }

        handle_buf.abort();
        handle_write.abort();
        handle_read.abort();
        let _ = handle_write.await;
        let _ = handle_read.await;

        debug!("make sure reader rx is empty and closed");
        while let Ok(req) = rx_read.recv_async().await {
            debug!("Answer from reader into buffer: {:?}", req);
            // we are very explicit here for better debugging
            match req {
                #[cfg(feature = "sqlite")]
                ClientStreamReq::Execute(_) => {
                    unreachable!("we should never receive ClientStreamReq::Execute from WS reader")
                }
                #[cfg(feature = "sqlite")]
                ClientStreamReq::ExecuteReturning(_) => {
                    unreachable!(
                        "we should never receive ClientStreamReq::ExecuteReturning from WS reader"
                    )
                }
                #[cfg(feature = "sqlite")]
                ClientStreamReq::Transaction(_) => {
                    unreachable!(
                        "we should never receive ClientStreamReq::Transaction from WS reader"
                    )
                }
                #[cfg(feature = "sqlite")]
                ClientStreamReq::Query(_) => {
                    unreachable!(
                        "we should never receive ClientStreamReq::QueryConsistent from WS reader"
                    )
                }
                #[cfg(feature = "sqlite")]
                ClientStreamReq::QueryConsistent(_) => {
                    unreachable!(
                        "we should never receive ClientStreamReq::QueryConsistent from WS reader"
                    )
                }
                #[cfg(feature = "sqlite")]
                ClientStreamReq::Batch(_) => {
                    unreachable!("we should never receive ClientStreamReq::Batch from WS reader")
                }
                #[cfg(feature = "sqlite")]
                ClientStreamReq::Migrate(_) => {
                    unreachable!("we should never receive ClientStreamReq::Migrate from WS reader")
                }
                #[cfg(feature = "backup")]
                ClientStreamReq::Backup(_) => {
                    unreachable!("we should never receive ClientStreamReq::Backup from WS reader")
                }
                #[cfg(feature = "cache")]
                ClientStreamReq::KV(_) => {
                    unreachable!("we should never receive ClientStreamReq::KV from WS reader")
                }
                #[cfg(feature = "cache")]
                ClientStreamReq::KVGet(_) => {
                    unreachable!("we should never receive ClientStreamReq::KVGet from WS reader")
                }
                #[cfg(feature = "dlock")]
                ClientStreamReq::LockAwait(_) => {
                    unreachable!(
                        "we should never receive ClientStreamReq::LockAwait from WS reader"
                    )
                }
                #[cfg(feature = "listen_notify_local")]
                ClientStreamReq::Notify(_) => {
                    unreachable!("we should never receive ClientStreamReq::Notify from WS reader")
                }
                ClientStreamReq::Shutdown => {
                    unreachable!("we should never receive ClientStreamReq::Shutdown from WS reader")
                }
                #[cfg(feature = "dashboard")]
                ClientStreamReq::LeaderChange((node_id, node), ready) => {
                    let ready_target = node_id
                        .zip(node.as_ref())
                        .map(|(node_id, node)| (node_id, node.addr_api.clone()));
                    update_leader(&leader, node_id, node).await;
                    if let (Some(target), Some(ready)) = (ready_target, ready) {
                        let _ = pending_leader_ready.register(target, Some(ready));
                    }
                }
                ClientStreamReq::RotateProxy(_) => {
                    unreachable!("we should never receive RotateProxy from WS reader")
                }
                ClientStreamReq::StreamResponse(resp) => {
                    try_forward_response(&mut in_flight, &mut in_flight_buf, false, resp).await;
                }
                ClientStreamReq::StreamClosed => {
                    // The outer manager already owns reconnect policy.
                }
                ClientStreamReq::CleanupBuffer => {
                    // ignore - we are re-connecting anyway
                }
            }
        }

        if shutdown {
            fail_client_stream_shutdown(&mut in_flight, &mut in_flight_buf, &rx_req);
            debug!("Shutting down Client stream receiver");
            break;
        }

        if rotate_after_disconnect {
            for (_, ack) in in_flight.drain().chain(in_flight_buf.drain()) {
                let _ = ack.send(Err(Error::Connect(
                    "Connection to proxy endpoint lost".into(),
                )));
            }
            rotate_proxy_endpoint(&client, &leader, &mut proxy_index).await;
        } else {
            for (req_id, ack) in in_flight.drain() {
                in_flight_buf.insert(req_id, ack);
            }
        }
        assert!(in_flight.is_empty());

        debug!("client stream tasks killed - re-connecting now");
    }
}

fn fail_client_stream_shutdown(
    in_flight: &mut HashMap<usize, Sender<Result<ApiStreamResponsePayload, Error>>>,
    in_flight_buf: &mut HashMap<usize, Sender<Result<ApiStreamResponsePayload, Error>>>,
    rx_req: &flume::Receiver<ClientStreamReq>,
) {
    for (_, ack) in in_flight.drain().chain(in_flight_buf.drain()) {
        let _ = ack.send(Err(Error::Connect("client stream manager stopped".into())));
    }
    while let Ok(request) = rx_req.try_recv() {
        request.fail_on_shutdown();
    }
}

#[inline(always)]
async fn try_forward_response(
    in_flight: &mut HashMap<usize, Sender<Result<ApiStreamResponsePayload, Error>>>,
    in_flight_buf: &mut HashMap<usize, Sender<Result<ApiStreamResponsePayload, Error>>>,
    awaiting_timeout: bool,
    response: ApiStreamResponse,
) {
    match in_flight.remove(&response.request_id) {
        None => {
            if awaiting_timeout {
                match in_flight_buf.remove(&response.request_id) {
                    None => {
                        error!("client ack for ApiStreamResponse missing");
                    }
                    Some(ack) => match ack.send(Ok(response.result)) {
                        Ok(_) => {
                            debug!("ApiStreamResponse sent to client from in_flight_buf");
                        }
                        Err(err) => {
                            error!("client ack could not be sent for {:?}", err);
                        }
                    },
                }
            } else {
                error!("client ack for ApiStreamResponse missing");
            }
        }

        Some(ack) => match ack.send(Ok(response.result)) {
            Ok(_) => {
                debug!("ApiStreamResponse sent to client");
            }
            Err(err) => {
                error!("client ack could not be sent for {:?}", err);
            }
        },
    }
}

async fn update_leader(
    leader: &Arc<RwLock<(NodeId, String)>>,
    node_id: Option<u64>,
    node: Option<Node>,
) {
    if let Some(leader_id) = node_id
        && let Some(node) = node
    {
        let api_addr = node.addr_api.clone();
        info!(
            "API Client received a Leader Change: {} / {}",
            leader_id, api_addr
        );
        {
            let mut lock = leader.write().await;
            *lock = (leader_id, api_addr);
        }
    }
}

fn next_configured_proxy(nodes: &[String], proxy_index: &mut usize) -> Option<String> {
    if nodes.is_empty() {
        return None;
    }
    *proxy_index = (*proxy_index + 1) % nodes.len();
    Some(nodes[*proxy_index].clone())
}

async fn rotate_proxy_endpoint(
    client: &Client,
    leader: &Arc<RwLock<(NodeId, String)>>,
    proxy_index: &mut usize,
) {
    debug_assert!(client.inner.proxy_mode);
    let mut lock = leader.write().await;
    if let Some(endpoint) = next_configured_proxy(&client.inner.nodes, proxy_index) {
        // Preserve this stream's synthetic/current id and replace only the
        // address with a member of the original configured trust boundary.
        let node_id = lock.0;
        *lock = (node_id, endpoint);
    }
}

fn cleanup_buffer_timeout(tx: flume::Sender<ClientStreamReq>, seconds: u64) -> JoinHandle<()> {
    task::spawn(async move {
        time::sleep(Duration::from_secs(seconds)).await;
        let _ = tx.send_async(ClientStreamReq::CleanupBuffer).await;
    })
}

async fn stream_reader(
    mut read: FragmentCollectorRead<ReadHalf<TokioIo<Upgraded>>>,
    tx: flume::Sender<ClientStreamReq>,
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
                let payload = deserialize::<ApiStreamResponse>(bytes).unwrap();
                if let Err(err) = tx
                    .send_async(ClientStreamReq::StreamResponse(payload))
                    .await
                {
                    error!("Error sending Response to Client Stream Manager: {:?}", err);
                }
            }
            OpCode::Close => break,
            OpCode::Ping => {}
            OpCode::Pong => {}
        }
    }

    let _ = tx.send_async(ClientStreamReq::StreamClosed).await;
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
                let _ =
                    write_close_frame_flushed(&mut write, Frame::close(1000, b"go away")).await;
                break Ok(());
            }
        }
    };

    let _ = finished.send(outcome);
    debug!("Exiting Client Stream Writer");
}

async fn try_connect(
    leader: &Arc<RwLock<(NodeId, String)>>,
    raft_type: &RaftType,
    tls_config: Option<Arc<rustls::ClientConfig>>,
    secret: &[u8],
) -> Result<(WebSocket<TokioIo<Upgraded>>, (NodeId, String)), Error> {
    let (node_id, addr) = {
        let lock = leader.read().await;
        (lock.0, lock.1.clone())
    };
    let socket = web_socket_connect::try_connect(node_id, &addr, raft_type, tls_config, secret).await?;
    Ok((socket, (node_id, addr)))
}

#[cfg(any(feature = "dashboard", test))]
fn leader_change_matches_connection(
    connected: &(NodeId, String),
    node_id: Option<NodeId>,
    node: Option<&Node>,
) -> bool {
    node_id == Some(connected.0)
        && node.is_some_and(|node| node.addr_api == connected.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn duplicate_near_deadline_shares_the_connecting_target() {
        let target = (9, "node-nine:21000".to_owned());
        let mut pending = PendingLeaderReady::default();
        let (first_tx, first_rx) = oneshot::channel();
        pending.register(target.clone(), Some(first_tx));

        time::advance(Duration::from_millis(4_900)).await;
        let (duplicate_tx, duplicate_rx) = oneshot::channel();
        assert!(!leader_handoff_restarts_connection(&target, &target));
        pending.register(target.clone(), Some(duplicate_tx));

        assert!(pending.acknowledge(&target));
        first_rx.await.expect("first same-target waiter");
        duplicate_rx.await.expect("duplicate same-target waiter");
    }

    #[tokio::test]
    async fn a_new_target_fails_superseded_waiters_and_acks_only_itself() {
        let target_b = (2, "node-two:21000".to_owned());
        let target_c = (3, "node-three:21000".to_owned());
        let mut pending = PendingLeaderReady::default();
        let (b_tx, b_rx) = oneshot::channel();
        pending.register(target_b.clone(), Some(b_tx));
        let (c_tx, c_rx) = oneshot::channel();
        assert!(leader_handoff_restarts_connection(&target_b, &target_c));
        pending.register(target_c.clone(), Some(c_tx));

        assert!(b_rx.await.is_err(), "B must fail when C supersedes it");
        assert!(!pending.acknowledge(&target_b));
        assert!(pending.acknowledge(&target_c));
        c_rx.await.expect("C waiter");
    }

    #[tokio::test]
    async fn failed_target_yields_to_the_leader_selected_by_discovery() {
        let target_c = (3, "node-three:21000".to_owned());
        let discovered_b = (2, "node-two:21000".to_owned());
        let mut pending = PendingLeaderReady::default();
        let (c_tx, c_rx) = oneshot::channel();
        pending.register(target_c, Some(c_tx));

        pending.resolve_connected_target(&discovered_b);
        assert!(
            pending.target.is_none(),
            "obsolete C must not make the manager reject B forever"
        );
        assert!(c_rx.await.is_err(), "C handoff must fail on discovered B");
        assert!(!pending.acknowledge(&discovered_b));
    }

    #[tokio::test(start_paused = true)]
    async fn a_handoff_expired_before_receipt_cannot_redirect_the_manager() {
        let stale_target = (3, "node-three:21000".to_owned());
        let mut pending = PendingLeaderReady::default();
        let (stale_tx, stale_rx) = oneshot::channel();

        time::advance(crate::LEADER_STREAM_HANDOFF_TIMEOUT).await;
        drop(stale_rx);
        assert!(!pending.register(stale_target, Some(stale_tx)));
        assert!(pending.target.is_none());
        assert!(pending.waiters.is_empty());
    }

    #[test]
    fn proxy_failover_cycles_only_through_configured_endpoints() {
        let nodes = vec![
            "proxy-a".to_owned(),
            "proxy-b".to_owned(),
            "proxy-c".to_owned(),
        ];
        let mut proxy_index = 0;
        assert_eq!(
            next_configured_proxy(&nodes, &mut proxy_index).as_deref(),
            Some("proxy-b")
        );
        assert_eq!(
            next_configured_proxy(&nodes, &mut proxy_index).as_deref(),
            Some("proxy-c")
        );
        assert_eq!(
            next_configured_proxy(&nodes, &mut proxy_index).as_deref(),
            Some("proxy-a")
        );

        let duplicates = vec![
            "proxy-a".to_owned(),
            "proxy-a".to_owned(),
            "proxy-b".to_owned(),
        ];
        let mut duplicate_index = 0;
        assert_eq!(
            next_configured_proxy(&duplicates, &mut duplicate_index).as_deref(),
            Some("proxy-a")
        );
        assert_eq!(
            next_configured_proxy(&duplicates, &mut duplicate_index).as_deref(),
            Some("proxy-b")
        );

        let mut singleton_index = 0;
        assert_eq!(
            next_configured_proxy(&["only-proxy".to_owned()], &mut singleton_index).as_deref(),
            Some("only-proxy")
        );
        let mut empty_index = 0;
        assert_eq!(next_configured_proxy(&[], &mut empty_index), None);
    }

    #[test]
    fn stale_recovery_for_the_connected_leader_does_not_require_reconnect() {
        let connected = (7, "node-seven:21000".to_owned());
        let same = Node {
            id: 7,
            addr_raft: "node-seven:21001".to_owned(),
            addr_api: "node-seven:21000".to_owned(),
        };
        assert!(leader_change_matches_connection(
            &connected,
            Some(7),
            Some(&same)
        ));

        let replacement = Node {
            id: 8,
            addr_raft: "node-eight:21001".to_owned(),
            addr_api: "node-eight:21000".to_owned(),
        };
        assert!(!leader_change_matches_connection(
            &connected,
            Some(8),
            Some(&replacement)
        ));
    }

    #[test]
    fn a_discovered_leader_reconnects_without_the_failure_backoff() {
        let previous = (1, "node-1".to_owned());
        let replacement = (2, "node-2".to_owned());

        assert_eq!(reconnect_delay(&previous, &replacement), Duration::ZERO);
        assert_eq!(
            reconnect_delay(&replacement, &replacement),
            CLIENT_STREAM_RETRY_DELAY
        );
    }
}
