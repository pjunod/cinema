//! Bounded, authenticated owner summaries; missing peers fence all routing.
use super::super::{
    wire::{Dispatch, Proof, Request},
    INTERNAL_PATH,
};
use super::*;
use crate::http::peer_transport::{PeerAuthMode, PeerTransport};
use futures_util::{stream, StreamExt};
use plurx_core::remote_control::{Target, Version as RemoteVersion};
use serde::Deserialize;
use std::time::Duration;
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    pub receiver_id: String,
    pub target: Target,
    pub foreground_id: Uuid,
    pub token_digest: String,
    pub receiver_hash: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    version: RemoteVersion,
    sessions: Vec<Snapshot>,
}
fn parse(bytes: &[u8], node: &str) -> Result<Vec<Snapshot>, ApiError> {
    if bytes.len() > 64 * 1024 {
        return Err(fail(503, "unavailable"));
    }
    let reply: Reply = serde_json::from_slice(bytes).map_err(|_| fail(503, "unavailable"))?;
    let RemoteVersion::V1 = reply.version;
    if reply.sessions.len() > 20
        || reply.sessions.iter().any(|s| {
            !valid_id(&s.receiver_id)
                || s.target.owner_node_id != node
                || [s.token_digest.as_str(), s.receiver_hash.as_str()]
                    .iter()
                    .any(|hash| {
                        hash.len() != 64
                            || !hash
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    })
        })
    {
        return Err(fail(503, "unavailable"));
    }
    Ok(reply.sessions)
}
async fn inner(state: &AppState, user: i64, digest: &str) -> Result<Vec<Snapshot>, ApiError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let peers = tokio::time::timeout_at(deadline, state.membership.operations_peers())
        .await
        .map_err(|_| fail(503, "unavailable"))?
        .map_err(|_| fail(503, "unavailable"))?;
    if peers.len() > 64 {
        return Err(fail(503, "unavailable"));
    }
    let dispatch = Dispatch {
        version: RemoteVersion::V1,
        user_id: user,
        token_digest: digest.to_owned(),
        proof: Proof {
            receiver_hash: None,
            grant_hash: None,
            pairing_hash: None,
        },
        request: Request::InvitationSessions {
            version: RemoteVersion::V1,
        },
    };
    let (status, local) = state.remote.perform(state, dispatch.clone()).await?;
    if status != 200 {
        return Err(fail(503, "unavailable"));
    }
    let mut all = parse(
        &serde_json::to_vec(&local).map_err(|_| fail(503, "unavailable"))?,
        &state.node_id,
    )?;
    let body = serde_json::to_vec(&dispatch).map_err(|_| fail(503, "unavailable"))?;
    let transport = PeerTransport::new(state.membership.clone());
    let replies = stream::iter(peers.into_iter().map(|peer| {
        let transport = transport.clone();
        let body = body.clone();
        async move {
            let base = peer.http_base.ok_or_else(|| fail(503, "unavailable"))?;
            let reply = transport
                .request(
                    &peer.node_id,
                    &base,
                    reqwest::Method::POST,
                    INTERNAL_PATH,
                    body,
                    deadline,
                    64 * 1024,
                    PeerAuthMode::ExactRequestAndMemberResponse,
                )
                .await
                .map_err(|_| fail(503, "unavailable"))?;
            if !reply.status.is_success() {
                return Err(fail(503, "unavailable"));
            }
            parse(&reply.body, &peer.node_id)
        }
    }))
    .buffer_unordered(4)
    .collect::<Vec<_>>()
    .await;
    for rows in replies {
        all.extend(rows?);
    }
    Ok(all)
}
pub(super) fn unique<'a>(rows: &'a [Snapshot], receiver: &str) -> Option<&'a Snapshot> {
    let mut matches = rows.iter().filter(|s| s.receiver_id == receiver);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

pub(super) async fn collect(
    state: &AppState,
    user: i64,
    digest: &str,
) -> Result<Vec<Snapshot>, ApiError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let collection = async {
        let rows = inner(state, user, digest).await?;
        #[cfg(test)]
        {
            let gate = state
                .invitations
                .snapshot_gate
                .lock()
                .expect("test collection gate")
                .clone();
            if let Some(gate) = gate {
                gate.entered.add_permits(1);
                gate.release.acquire().await.expect("test gate").forget();
            }
        }
        Ok(rows)
    };
    tokio::time::timeout_at(deadline, collection)
        .await
        .map_err(|_| fail(503, "unavailable"))?
}

#[cfg(test)]
pub(super) struct TestGate {
    pub entered: tokio::sync::Semaphore,
    pub release: tokio::sync::Semaphore,
}
