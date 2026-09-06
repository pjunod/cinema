use crate::network::frame_io::{
    CLOSE_WRITE_TIMEOUT, write_close_frame_flushed, write_frame_flushed,
    write_socket_close_frame_flushed,
};
use crate::network::handshake::HandshakeSecret;
use crate::network::{AppStateExt, Error, serialize_network};
use axum::response::IntoResponse;
use fastwebsockets::{FragmentCollectorRead, Frame, OpCode, Payload, upgrade};
use openraft::error::{Fatal, InstallSnapshotError, RaftError};
use serde::{Deserialize, Serialize};
use std::ops::Deref;
use std::sync::atomic::Ordering;
use tokio::sync::{oneshot, watch};
use tokio::task;
use tracing::{debug, error, warn};

#[cfg(feature = "cache")]
use crate::app_state::RaftType;
#[cfg(feature = "cache")]
use crate::helpers;
#[cfg(feature = "cache")]
use crate::store::state_machine::memory::TypeConfigKV;
#[cfg(feature = "cache")]
use std::collections::BTreeSet;

#[cfg(feature = "sqlite")]
use crate::store::state_machine::sqlite::TypeConfigSqlite;

use crate::helpers::deserialize;
#[cfg(any(feature = "cache", feature = "sqlite"))]
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Serialize, Deserialize)]
pub enum RaftStreamRequest {
    #[cfg(feature = "sqlite")]
    AppendDB((usize, AppendEntriesRequest<TypeConfigSqlite>)),
    #[cfg(feature = "sqlite")]
    VoteDB((usize, VoteRequest<u64>)),
    #[cfg(feature = "sqlite")]
    SnapshotDB((usize, InstallSnapshotRequest<TypeConfigSqlite>)),

    #[cfg(feature = "cache")]
    AppendCache((usize, AppendEntriesRequest<TypeConfigKV>)),
    #[cfg(feature = "cache")]
    VoteCache((usize, VoteRequest<u64>)),
    #[cfg(feature = "cache")]
    SnapshotCache((usize, InstallSnapshotRequest<TypeConfigKV>)),
    #[cfg(feature = "cache")]
    RemoveMembershipCache(u64),
}

impl From<&[u8]> for RaftStreamRequest {
    #[inline]
    fn from(value: &[u8]) -> Self {
        deserialize(value).unwrap()
    }
}

impl From<Vec<u8>> for RaftStreamRequest {
    #[inline]
    fn from(value: Vec<u8>) -> Self {
        deserialize(&value).unwrap()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RaftStreamResponse {
    pub request_id: usize,
    pub payload: RaftStreamResponsePayload,
}

#[derive(Debug, Serialize, Deserialize)]
#[allow(clippy::enum_variant_names)]
pub enum RaftStreamResponsePayload {
    #[cfg(feature = "sqlite")]
    AppendDB(Result<AppendEntriesResponse<u64>, RaftError<u64>>),
    #[cfg(feature = "sqlite")]
    VoteDB(Result<VoteResponse<u64>, RaftError<u64>>),
    #[cfg(feature = "sqlite")]
    SnapshotDB(Result<InstallSnapshotResponse<u64>, RaftError<u64, InstallSnapshotError>>),

    #[cfg(feature = "cache")]
    AppendCache(Result<AppendEntriesResponse<u64>, RaftError<u64>>),
    #[cfg(feature = "cache")]
    VoteCache(Result<VoteResponse<u64>, RaftError<u64>>),
    #[cfg(feature = "cache")]
    SnapshotCache(Result<InstallSnapshotResponse<u64>, RaftError<u64, InstallSnapshotError>>),
}

#[derive(Debug)]
pub(crate) enum WsWriteMsg {
    Payload(Vec<u8>),
    Break,
}

impl From<Vec<u8>> for RaftStreamResponse {
    #[inline]
    fn from(value: Vec<u8>) -> Self {
        deserialize(&value).unwrap()
    }
}

pub async fn stream_cache(
    state: AppStateExt,
    ws: upgrade::IncomingUpgrade,
) -> Result<impl IntoResponse, Error> {
    tracing::info!("Incoming WebSocket stream for Cache");

    #[cfg(feature = "cache")]
    {
        if !state.raft_cache.is_startup_finished.load(Ordering::Relaxed) {
            warn!("Cache Raft still starting up - rejecting streaming connection");
            return Err(Error::BadRequest("Raft is still starting up".into()));
        }
        if state.raft_cache.is_raft_stopped.load(Ordering::Relaxed) {
            warn!("Cache Raft has been stopped - rejecting streaming connection");
            return Err(Error::BadRequest("Raft has been stopped".into()));
        }
    }

    tracing::info!("WebSocket cache stream request accepted");

    let (response, socket) = ws.upgrade()?;
    tokio::task::spawn(Box::pin(async move {
        if let Err(err) = handle_socket(state, socket).await {
            debug!("Cache WebSocket stream closed: {}", err);
        }
    }));

    Ok(response)
}

pub async fn stream_sqlite(
    state: AppStateExt,
    ws: upgrade::IncomingUpgrade,
) -> Result<impl IntoResponse, Error> {
    tracing::info!("Incoming WebSocket stream for SQLite");

    #[cfg(feature = "sqlite")]
    {
        if !state.raft_db.is_startup_finished.load(Ordering::Relaxed) {
            warn!("Sqlite Raft still starting up - rejecting streaming connection");
            return Err(Error::BadRequest("Raft is still starting up".into()));
        }
        if state.raft_db.is_raft_stopped.load(Ordering::Relaxed) {
            warn!("Sqlite Raft has been stopped - rejecting streaming connection");
            return Err(Error::BadRequest("Raft has been stopped".into()));
        }
    }

    tracing::info!("WebSocket sqlite stream request accepted");

    let (response, socket) = ws.upgrade()?;
    tokio::task::spawn(Box::pin(async move {
        if let Err(err) = handle_socket(state, socket).await {
            debug!("SQLite WebSocket stream closed: {}", err);
        }
    }));

    Ok(response)
}

async fn handle_socket(
    state: AppStateExt,
    socket: upgrade::UpgradeFut,
) -> Result<(), fastwebsockets::WebSocketError> {
    let mut ws = socket.await?;
    ws.set_auto_close(true);

    if let Err(err) = HandshakeSecret::server(&mut ws, state.secret_raft.as_bytes()).await {
        error!("Error during WebSocket handshake: {}", err);
        write_socket_close_frame_flushed(&mut ws, Frame::close(1000, b"Invalid Handshake")).await?;
        return Ok(());
    }

    let (tx_write, rx_write) = flume::bounded::<WsWriteMsg>(1);
    let (rx, write) = ws.split(tokio::io::split);
    // IMPORTANT: the reader is NOT CANCEL SAFE in v0.8!
    let mut read = FragmentCollectorRead::new(rx);

    let (tx_connection_closed, mut rx_connection_closed) = watch::channel(false);
    let (tx_writer_finished, mut rx_writer_finished) = oneshot::channel();
    let writer_connection_closed = tx_connection_closed.clone();
    let handle_write = task::spawn(raft_response_writer(
        write,
        rx_write,
        writer_connection_closed,
        tx_writer_finished,
    ));

    let (tx_read, rx_read) = flume::bounded(1);
    let (tx_reader_finished, mut rx_reader_finished) = oneshot::channel();
    let reader_connection_closed = tx_connection_closed.clone();
    let handle_read = task::spawn(async move {
        let outcome = loop {
            let frame = match read
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
                Ok(frame) => frame,
                Err(err) => break Err(err.to_string()),
            };
            let req = match frame.opcode {
                OpCode::Close => {
                    debug!("received Close frame in server stream");
                    break Ok(());
                }
                OpCode::Binary => {
                    let bytes = frame.payload.deref();
                    match deserialize::<RaftStreamRequest>(bytes) {
                        Ok(req) => req,
                        Err(err) => break Err(format!("invalid Raft stream request: {err}")),
                    }
                }
                _ => break Err("non-binary Raft stream payload".to_owned()),
            };

            tokio::select! {
                _ = rx_connection_closed.changed() => break Ok(()),
                result = tx_read.send_async(req) => {
                    if result.is_err() {
                        break Ok(());
                    }
                }
            }
        };
        reader_connection_closed.send_replace(true);
        let _ = tx_reader_finished.send(outcome);
    });

    let mut writer_failed = false;
    loop {
        let req = tokio::select! {
            biased;
            writer = &mut rx_writer_finished => {
                match writer {
                    Ok(Ok(())) => error!("Raft server WebSocket writer exited while connected"),
                    Ok(Err(err)) => error!("Raft server WebSocket writer failed: {err}"),
                    Err(_) => error!("Raft server WebSocket writer panicked or was cancelled"),
                }
                writer_failed = true;
                break;
            }
            reader = &mut rx_reader_finished => {
                match reader {
                    Ok(Ok(())) => debug!("Raft server WebSocket reader exited"),
                    Ok(Err(err)) => error!("Raft server WebSocket reader failed: {err}"),
                    Err(_) => error!("Raft server WebSocket reader panicked or was cancelled"),
                }
                break;
            }
            req = rx_read.recv_async() => match req {
                Ok(req) => req,
                Err(_) => break,
            },
        };

        #[cfg(feature = "validation-test-helpers")]
        if crate::network::raft_client::validation_raft_partitioned() {
            break;
        }

        let mut work_connection_closed = tx_connection_closed.subscribe();
        let work = execute_raft_request(&state, req, &mut work_connection_closed);
        tokio::pin!(work);
        let work_result = tokio::select! {
            biased;
            writer = &mut rx_writer_finished => {
                match writer {
                    Ok(Ok(())) => error!("Raft server WebSocket writer exited while connected"),
                    Ok(Err(err)) => error!("Raft server WebSocket writer failed: {err}"),
                    Err(_) => error!("Raft server WebSocket writer panicked or was cancelled"),
                }
                writer_failed = true;
                None
            }
            reader = &mut rx_reader_finished => {
                match reader {
                    Ok(Ok(())) => debug!("Raft server WebSocket reader exited"),
                    Ok(Err(err)) => error!("Raft server WebSocket reader failed: {err}"),
                    Err(_) => error!("Raft server WebSocket reader panicked or was cancelled"),
                }
                None
            }
            result = &mut work => Some(result),
        };
        let Some(work_result) = work_result else {
            break;
        };
        let (request_id, payload) = match work_result {
            Ok(Some(response)) => response,
            Ok(None) => break,
            Err(err) => {
                error!("Raft snapshot request was not admitted or lost its response: {err:?}");
                break;
            }
        };

        let response = WsWriteMsg::Payload(serialize_network(&RaftStreamResponse {
            request_id,
            payload,
        }));
        tokio::select! {
            biased;
            writer = &mut rx_writer_finished => {
                match writer {
                    Ok(Ok(())) => error!("Raft server WebSocket writer exited while connected"),
                    Ok(Err(err)) => error!("Raft server WebSocket writer failed: {err}"),
                    Err(_) => error!("Raft server WebSocket writer panicked or was cancelled"),
                }
                writer_failed = true;
                break;
            }
            _ = &mut rx_reader_finished => break,
            result = tx_write.send_async(response) => {
                if let Err(err) = result {
                    error!("Error forwarding raft response to WebSocket writer: {err}");
                    break;
                }
            }
        }
    }

    tx_connection_closed.send_replace(true);
    let mut handle_write = handle_write;
    let writer_finished = if writer_failed {
        false
    } else {
        tokio::time::timeout(CLOSE_WRITE_TIMEOUT, async {
            tx_write.send_async(WsWriteMsg::Break).await.ok()?;
            Some((&mut handle_write).await)
        })
        .await
        .ok()
        .flatten()
        .is_some()
    };
    drop(tx_write);
    if !writer_finished {
        handle_write.abort();
        let _ = handle_write.await;
    }
    handle_read.abort();
    let _ = handle_read.await;

    debug!("handle_socket exiting");

    Ok(())
}

async fn write_raft_response_frame<S>(
    write: &mut fastwebsockets::WebSocketWrite<S>,
    bytes: Vec<u8>,
) -> Result<(), fastwebsockets::WebSocketError>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    write_frame_flushed(write, Frame::binary(Payload::Owned(bytes))).await
}

async fn raft_response_writer<S>(
    mut write: fastwebsockets::WebSocketWrite<S>,
    rx_write: flume::Receiver<WsWriteMsg>,
    connection_closed: watch::Sender<bool>,
    finished: oneshot::Sender<Result<(), String>>,
) where
    S: tokio::io::AsyncWrite + Unpin,
{
    let outcome = loop {
        let req = match rx_write.recv_async().await {
            Ok(req) => req,
            Err(_) => break Ok(()),
        };
        match req {
            WsWriteMsg::Payload(bytes) => {
                if let Err(err) = write_raft_response_frame(&mut write, bytes).await {
                    error!("Error during WebSocket write: {}", err);
                    break Err(err.to_string());
                }
            }
            WsWriteMsg::Break => {
                debug!("handle_socket -> server stream break message");
                break Ok(());
            }
        }
    };

    debug!("handle_socket -> Raft server WebSocket writer exiting");
    let _ = write_close_frame_flushed(&mut write, Frame::close(1000, b"go away")).await;
    connection_closed.send_replace(true);
    let _ = finished.send(outcome);
}

async fn execute_raft_request(
    state: &AppStateExt,
    request: RaftStreamRequest,
    connection_closed: &mut watch::Receiver<bool>,
) -> Result<
    Option<(usize, RaftStreamResponsePayload)>,
    crate::network::snapshot_executor::SubmitError,
> {
    let response = match request {
        #[cfg(feature = "sqlite")]
        RaftStreamRequest::AppendDB((request_id, request)) => {
            let result = state.raft_db.raft.append_entries(request).await;
            if let Err(RaftError::Fatal(Fatal::Stopped)) = &result {
                debug!("Raft DB stopped - exiting");
                state.raft_db.is_raft_stopped.store(true, Ordering::Relaxed);
                return Ok(None);
            }
            (request_id, RaftStreamResponsePayload::AppendDB(result))
        }
        #[cfg(feature = "sqlite")]
        RaftStreamRequest::VoteDB((request_id, request)) => {
            let result = state.raft_db.raft.vote(request).await;
            (request_id, RaftStreamResponsePayload::VoteDB(result))
        }
        #[cfg(feature = "sqlite")]
        RaftStreamRequest::SnapshotDB((request_id, request)) => {
            let result = state
                .raft_db
                .snapshot_executor
                .submit(request, connection_closed)
                .await?;
            (request_id, RaftStreamResponsePayload::SnapshotDB(result))
        }
        #[cfg(feature = "cache")]
        RaftStreamRequest::AppendCache((request_id, request)) => {
            let result = state.raft_cache.raft.append_entries(request).await;
            if let Err(RaftError::Fatal(Fatal::Stopped)) = &result {
                debug!("Raft Cache stopped - exiting");
                state
                    .raft_cache
                    .is_raft_stopped
                    .store(true, Ordering::Relaxed);
                return Ok(None);
            }
            (request_id, RaftStreamResponsePayload::AppendCache(result))
        }
        #[cfg(feature = "cache")]
        RaftStreamRequest::VoteCache((request_id, request)) => {
            let result = state.raft_cache.raft.vote(request).await;
            (request_id, RaftStreamResponsePayload::VoteCache(result))
        }
        #[cfg(feature = "cache")]
        RaftStreamRequest::SnapshotCache((request_id, request)) => {
            let result = state
                .raft_cache
                .snapshot_executor
                .submit(request, connection_closed)
                .await?;
            (request_id, RaftStreamResponsePayload::SnapshotCache(result))
        }
        #[cfg(feature = "cache")]
        RaftStreamRequest::RemoveMembershipCache(node_id) => {
            debug!("Node drop membership request for Node: {}\n", node_id);
            let _lock = state.raft_lock.lock().await;
            let metrics = helpers::get_raft_metrics(state, &RaftType::Cache).await;
            let members = metrics.membership_config;
            let mut nodes_set = BTreeSet::new();
            for (id, _node) in members.nodes() {
                if *id != node_id {
                    nodes_set.insert(*id);
                }
            }
            if let Err(err) =
                helpers::change_membership(state, &RaftType::Cache, nodes_set, false).await
            {
                error!("Error removing remote Cache Member: {:?}", err);
            }
            return Ok(None);
        }
    };

    Ok(Some(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastwebsockets::Role;
    use openraft::Vote;
    use openraft::raft::InstallSnapshotResponse;

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn production_raft_response_writer_flushes_serialized_response_through_tls() {
        let response = RaftStreamResponse {
            request_id: 9,
            payload: RaftStreamResponsePayload::SnapshotDB(Ok(InstallSnapshotResponse {
                vote: Vote::new_committed(1, 1),
            })),
        };
        let bytes = serialize_network(&response);
        crate::network::frame_io::tests::exercise_gated_tls_writer(
            Role::Server,
            bytes,
            true,
            |mut write, bytes| async move { write_raft_response_frame(&mut write, bytes).await },
        )
        .await;
    }

    #[tokio::test]
    async fn production_raft_response_writer_reports_flush_failure_and_closes() {
        let write = crate::network::frame_io::tests::split_writer(
            crate::network::frame_io::tests::TestIo::failing_flush(),
        );
        let (tx, rx) = flume::bounded(1);
        let (closed, closed_rx) = watch::channel(false);
        let (finished, outcome) = oneshot::channel();
        tx.send_async(WsWriteMsg::Payload(b"Raft response".to_vec()))
            .await
            .expect("queue Raft response");

        raft_response_writer(write, rx, closed, finished).await;

        assert!(*closed_rx.borrow());
        let error = outcome
            .await
            .expect("writer terminal outcome")
            .expect_err("flush failure must terminate the writer");
        assert!(error.contains("injected flush failure"));
    }
}
