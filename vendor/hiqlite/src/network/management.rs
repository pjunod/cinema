use crate::NodeId;
use crate::app_state::{AppState, RaftType};
use crate::network::{
    AppStateExt, Error, fmt_ok, get_payload, validate_secret, validate_secret_value,
};
use crate::{Node, helpers};
use axum::Json;
use axum::body;
use axum::body::Body;
use axum::extract::{Extension, Path};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get};
use openraft::error::{CheckIsLeaderError, ForwardToLeader, RaftError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tracing::{debug, error, info, warn};

#[derive(Debug, Serialize, Deserialize)]
pub struct LearnerReq {
    pub node_id: u64,
    pub addr_api: String,
    pub addr_raft: String,
}

#[cfg(feature = "sqlite")]
macro_rules! membership_params {
    ($($value:expr),* $(,)?) => { vec![$(crate::Param::from($value)),*] };
}

/// A committed SQL intent freezes the admission evidence until a separate
/// committed membership entry reaches the requested outcome. Errors deliberately
/// retain the intent; elapsed time is never evidence that an ambiguous Raft
/// mutation did not commit.
#[cfg(feature = "sqlite")]
struct SharingMembershipIntent {
    raft_id: i64,
    attempt_id: String,
}

#[cfg(feature = "sqlite")]
async fn sharing_membership_write(
    state: &AppStateExt,
    sql: String,
    params: crate::Params,
) -> Result<usize, Error> {
    use crate::store::state_machine::sqlite::state_machine::{Query, QueryWrite};
    let response = state
        .raft_db
        .raft
        .client_write(QueryWrite::Transaction(vec![Query {
            sql: sql.into(),
            params,
        }]))
        .await?;
    let crate::Response::Transaction(results) = response.data else {
        return Err(Error::Config(
            "unexpected membership intent response".into(),
        ));
    };
    let results = results?;
    let [result] = results.as_slice() else {
        return Err(Error::Config("unexpected membership intent result".into()));
    };
    result
        .as_ref()
        .copied()
        .map_err(|error| Error::Config(format!("membership intent refused: {error}").into()))
}

#[cfg(feature = "sqlite")]
async fn sharing_membership_claim(
    state: &AppStateExt,
    raft_type: &RaftType,
    node: &Node,
    operation: &str,
) -> Result<Option<SharingMembershipIntent>, Error> {
    if *raft_type != RaftType::Sqlite {
        return Ok(None);
    }
    let mut rows=crate::query::query_consistent_local(&state.raft_db.raft,state.raft_db.log_statements,state.raft_db.read_pool.clone(),
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name='item_identity_watermark') OR EXISTS (SELECT 1 FROM sqlite_master schema JOIN pragma_table_info(schema.name) info WHERE schema.type='table' AND schema.name IN ('media_session_requests','media_playback_pointers','media_sessions','media_session_preparations','media_playback_desired','media_session_producer_recovery','library_channel_session_recipes','media_session_requests_principal_new','media_playback_pointers_principal_new','media_sessions_principal_new','media_session_preparations_principal_new','media_playback_desired_principal_new','media_session_producer_recovery_principal_new','library_channel_session_recipes_principal_new') AND info.name IN ('owner_key','principal_kind','share_grant_id','share_viewer_key')) OR EXISTS (SELECT 1 FROM sqlite_master WHERE name='cluster_sharing_membership_intents') AS installed",membership_params!()).await?;
    let installed = rows
        .first_mut()
        .ok_or_else(|| Error::Config("missing sharing schema observation".into()))?
        .try_get::<i64>("installed")?;
    if installed == 0 {
        return Ok(None);
    }
    let raft_id = i64::try_from(node.id)
        .map_err(|_| Error::Config("membership target exceeds SQL identity range".into()))?;
    if raft_id <= 0
        || [&node.addr_api, &node.addr_raft].iter().any(|address| {
            address.is_empty() || address.len() > 512 || address.chars().any(char::is_control)
        })
    {
        return Err(Error::Config("invalid sharing membership target".into()));
    }
    // A partial installation must not turn an INSERT into an unguarded proof.
    let mut guards = vec![
        "cluster_sharing_principal_membership_claim_guard".to_owned(),
        "cluster_sharing_catalogue_membership_claim_guard".to_owned(),
        "cluster_sharing_membership_intent_update_guard".to_owned(),
    ];
    for table in [
        "cluster_nodes",
        "cluster_node_capabilities",
        "cluster_sharing_join_declarations",
        "cluster_join_tokens",
    ] {
        for event in ["insert", "update", "delete"] {
            guards.push(format!("cluster_sharing_freeze_{table}_{event}"));
        }
    }
    let guard_names =
        serde_json::to_string(&guards).map_err(|error| Error::Config(error.to_string().into()))?;
    let mut shape=crate::query::query_consistent_local(&state.raft_db.raft,state.raft_db.log_statements,state.raft_db.read_pool.clone(),
        "SELECT (SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name IN (SELECT value FROM json_each($1)))=15 AND (SELECT COUNT(*) FROM pragma_table_list WHERE name IN ('cluster_sharing_membership_intents','cluster_sharing_join_declarations') AND type='table' AND strict=1)=2 AND (SELECT COUNT(*) FROM pragma_table_info('cluster_sharing_membership_intents'))=7 AND (SELECT COUNT(*) FROM pragma_table_info('cluster_sharing_membership_intents') WHERE (name='raft_id' AND type='INTEGER' AND pk=1) OR (name IN ('node_id','attempt_id','operation','api_address','raft_address') AND type='TEXT' AND pk=0 AND \"notnull\"=1) OR (name='claimed_at' AND type='INTEGER' AND pk=0 AND \"notnull\"=1))=7 AND (SELECT COUNT(*) FROM pragma_table_info('cluster_sharing_join_declarations'))=7 AND (SELECT COUNT(*) FROM pragma_table_info('cluster_sharing_join_declarations') WHERE (name='token_hash' AND type='TEXT' AND pk=1 AND \"notnull\"=1) OR (name='capability' AND type='TEXT' AND pk=2 AND \"notnull\"=1) OR (name IN ('node_id','api_address','raft_address') AND type='TEXT' AND pk=0 AND \"notnull\"=1) OR (name IN ('raft_id','last_seen_at') AND type='INTEGER' AND pk=0 AND \"notnull\"=1))=7 AS valid",membership_params!(guard_names.clone())).await?;
    if shape
        .first_mut()
        .ok_or_else(|| Error::Config("missing sharing admission shape".into()))?
        .try_get::<i64>("valid")?
        != 1
    {
        return Err(Error::Config(
            "partial sharing membership admission schema".into(),
        ));
    }
    let mut existing=crate::query::query_consistent_local(&state.raft_db.raft,state.raft_db.log_statements,state.raft_db.read_pool.clone(),
        "SELECT attempt_id,operation,api_address,raft_address FROM cluster_sharing_membership_intents WHERE raft_id=$1",membership_params!(raft_id)).await?;
    if let Some(intent) = existing.first_mut() {
        if intent.try_get::<String>("operation")? != operation
            || intent.try_get::<String>("api_address")? != node.addr_api
            || intent.try_get::<String>("raft_address")? != node.addr_raft
        {
            return Err(Error::Config(
                "another sharing membership outcome remains unresolved".into(),
            ));
        }
        return Ok(Some(SharingMembershipIntent {
            raft_id,
            attempt_id: intent.try_get("attempt_id")?,
        }));
    }
    let metrics = state.raft_db.raft.metrics().borrow().clone();
    let applied_membership =
        metrics
            .membership_config
            .log_id()
            .as_ref()
            .is_some_and(|membership| {
                metrics
                    .last_applied
                    .as_ref()
                    .is_some_and(|applied| applied.index >= membership.index)
            })
            && metrics
                .membership_config
                .membership()
                .get_joint_config()
                .len()
                == 1;
    let exact_member = applied_membership
        && metrics
            .membership_config
            .membership()
            .get_node(&node.id)
            .is_some_and(|member| {
                member.addr_api == node.addr_api && member.addr_raft == node.addr_raft
            });
    if exact_member
        && (operation == "learner"
            || metrics
                .membership_config
                .voter_ids()
                .any(|id| id == node.id))
    {
        return Ok(None);
    }
    let attempt_id = uuid::Uuid::now_v7().to_string();
    let now = chrono::Utc::now().timestamp_millis();
    let changed=sharing_membership_write(state,
        "INSERT INTO cluster_sharing_membership_intents(raft_id,node_id,attempt_id,operation,api_address,raft_address,claimed_at) SELECT $1,node.node_id,$2,$3,$4,$5,$6 FROM cluster_nodes node WHERE node.raft_id=$1 AND node.removed_at IS NULL AND node.api_address=$4 AND node.raft_address=$5 AND (SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name IN (SELECT value FROM json_each($7)))=15".to_owned(),
        membership_params!(raft_id,attempt_id.clone(),operation,node.addr_api.clone(),node.addr_raft.clone(),now,guard_names)).await?;
    if changed != 1 {
        return Err(Error::Config(
            "installed sharing schema has no complete current admission proof".into(),
        ));
    }
    Ok(Some(SharingMembershipIntent {
        raft_id,
        attempt_id,
    }))
}

#[cfg(feature = "sqlite")]
async fn sharing_membership_finish(
    state: &AppStateExt,
    node: &Node,
    voter: bool,
    intent: Option<SharingMembershipIntent>,
) -> Result<(), Error> {
    let Some(intent) = intent else {
        return Ok(());
    };
    state.raft_db.raft.ensure_linearizable().await?;
    let metrics = state.raft_db.raft.metrics().borrow().clone();
    let applied_membership =
        metrics
            .membership_config
            .log_id()
            .as_ref()
            .is_some_and(|membership| {
                metrics
                    .last_applied
                    .as_ref()
                    .is_some_and(|applied| applied.index >= membership.index)
            })
            && metrics
                .membership_config
                .membership()
                .get_joint_config()
                .len()
                == 1;
    let committed = applied_membership
        && metrics
            .membership_config
            .membership()
            .get_node(&node.id)
            .is_some_and(|member| {
                member.addr_api == node.addr_api && member.addr_raft == node.addr_raft
            })
        && (!voter
            || metrics
                .membership_config
                .voter_ids()
                .any(|id| id == node.id));
    if !committed {
        return Err(Error::Config(
            "sharing membership outcome remains unresolved".into(),
        ));
    }
    let changed = sharing_membership_write(
        state,
        "DELETE FROM cluster_sharing_membership_intents WHERE raft_id=$1 AND attempt_id=$2"
            .to_owned(),
        membership_params!(intent.raft_id, intent.attempt_id),
    )
    .await?;
    if changed != 1 {
        return Err(Error::Config(
            "sharing membership intent changed before completion".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClusterLeaveReq {
    pub node_id: u64,
    pub stay_as_learner: bool,
}

#[derive(Clone)]
pub(crate) struct SnapshotTransportEndpoint {
    secret_api: Arc<str>,
    status: crate::LocalSnapshotTransportStatus,
}

pub(crate) fn snapshot_transport_sqlite_route<S>(
    secret_api: &str,
    status: crate::LocalSnapshotTransportStatus,
) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    get(snapshot_transport_sqlite).layer(Extension(SnapshotTransportEndpoint {
        secret_api: Arc::from(secret_api),
        status,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MembershipChangeReq {
    RemoveVoter { remove_voter: NodeId },
    SetVoters(BTreeSet<NodeId>),
}

#[tracing::instrument(skip_all)]
pub(crate) async fn add_learner(
    state: AppStateExt,
    headers: HeaderMap,
    Path(raft_type): Path<RaftType>,
    body: body::Bytes,
) -> Result<Response, Error> {
    validate_secret(&state, &headers)?;

    if helpers::is_raft_stopped(&state, &raft_type)
        || !helpers::is_raft_initialized(&state, &raft_type).await?
    {
        return Err(Error::Error("Raft is not initialized".into()));
    }
    are_we_leader(&state, &raft_type).await?;

    let LearnerReq {
        node_id,
        addr_api,
        addr_raft,
    } = get_payload(&headers, body)?;
    let node = Node {
        id: node_id,
        addr_raft,
        addr_api,
    };
    info!("{:?} requests to be added as {:?} Learner", node, raft_type);
    let lock = state.raft_lock.lock().await;
    are_we_leader(&state, &raft_type).await?;
    #[cfg(feature = "sqlite")]
    let sharing_intent = sharing_membership_claim(&state, &raft_type, &node, "learner").await?;
    let nid = node.id;
    let res = helpers::add_new_learner(&state, &raft_type, node.clone()).await;
    match res {
        Ok(_) => {
            let mut metrics = helpers::get_raft_metrics(&state, &raft_type).await;
            let mut is_member = metrics
                .membership_config
                .membership()
                .get_node(&nid)
                .is_some();
            while !is_member {
                info!("Waiting for node {nid} to become a committed learner");
                time::sleep(Duration::from_millis(500)).await;
                metrics = helpers::get_raft_metrics(&state, &raft_type).await;
                is_member = metrics
                    .membership_config
                    .membership()
                    .get_node(&nid)
                    .is_some();
            }

            #[cfg(feature = "sqlite")]
            sharing_membership_finish(&state, &node, false, sharing_intent).await?;
            // give it a second to sync before dropping the lock
            time::sleep(Duration::from_millis(1000)).await;
            drop(lock);
            info!("Added node {nid} as commited {:?} learner", raft_type);
            fmt_ok(headers, ())
        }
        Err(err) => {
            error!("Error adding node as {:?} learner: {:?}", raft_type, err);
            Err(err)
        }
    }
}

/// Changes specified learners to members, or remove members.
#[tracing::instrument(skip_all)]
pub(crate) async fn become_member(
    state: AppStateExt,
    headers: HeaderMap,
    Path(raft_type): Path<RaftType>,
    body: body::Bytes,
) -> Result<Response, Error> {
    validate_secret(&state, &headers)?;

    if helpers::is_raft_stopped(&state, &raft_type)
        || !helpers::is_raft_initialized(&state, &raft_type).await?
    {
        return Err(Error::Error("Raft is not initialized".into()));
    }
    are_we_leader(&state, &raft_type).await?;

    let lock = state.raft_lock.lock().await;
    are_we_leader(&state, &raft_type).await?;
    let payload = get_payload::<LearnerReq>(&headers, body)?;
    #[cfg(feature = "sqlite")]
    let node = Node {
        id: payload.node_id,
        addr_api: payload.addr_api.clone(),
        addr_raft: payload.addr_raft.clone(),
    };
    info!("{:?} Node membership request: {:?}", raft_type, payload);

    let mut metrics = helpers::get_raft_metrics(&state, &raft_type).await;
    debug!("{:?} Members before add: {:?}", raft_type, metrics);

    #[cfg(feature = "sqlite")]
    let sharing_intent = sharing_membership_claim(&state, &raft_type, &node, "voter").await?;
    let is_voter = metrics
        .membership_config
        .voter_ids()
        .any(|id| id == payload.node_id);
    if is_voter {
        #[cfg(feature = "sqlite")]
        sharing_membership_finish(&state, &node, true, sharing_intent).await?;
        info!(
            "Node {} is a voter already - nothing left to do",
            payload.node_id
        );
        return fmt_ok(headers, ());
    }

    let mut nodes_set = metrics
        .membership_config
        .voter_ids()
        .collect::<BTreeSet<u64>>();
    nodes_set.insert(payload.node_id);

    match helpers::change_membership(&state, &raft_type, nodes_set, true).await {
        Ok(_) => {
            metrics = helpers::get_raft_metrics(&state, &raft_type).await;
            let mut is_voter = metrics
                .membership_config
                .voter_ids()
                .any(|id| id == payload.node_id);
            while !is_voter {
                info!(
                    "Waiting for node {} to become a committed learner",
                    payload.node_id
                );
                time::sleep(Duration::from_millis(500)).await;
                metrics = helpers::get_raft_metrics(&state, &raft_type).await;
                is_voter = metrics
                    .membership_config
                    .voter_ids()
                    .any(|id| id == payload.node_id);
            }

            #[cfg(feature = "sqlite")]
            sharing_membership_finish(&state, &node, true, sharing_intent).await?;
            // give it a second to sync before dropping the lock
            time::sleep(Duration::from_millis(1000)).await;
            drop(lock);
            info!("Added node {} as {:?} member", payload.node_id, raft_type);
            fmt_ok(headers, ())
        }
        Err(err) => {
            error!("Error adding node as member: {:?}", err);
            Err(err)
        }
    }
}

async fn are_we_leader(state: &AppStateExt, raft_type: &RaftType) -> Result<(), Error> {
    if let Some(leader_id) = helpers::get_raft_leader(state, raft_type).await {
        if leader_id == state.id {
            Ok(())
        } else {
            let metrics = helpers::get_raft_metrics(state, raft_type).await;
            let leader = metrics
                .membership_config
                .membership()
                .get_node(&leader_id)
                .expect("Leader ID to always exist in membership config");

            let err = RaftError::APIError(CheckIsLeaderError::ForwardToLeader(ForwardToLeader {
                leader_id: Some(leader_id),
                leader_node: Some(leader.clone()),
            }));
            Err(Error::CheckIsLeaderError(Box::new(err)))
        }
    } else {
        Err(Error::LeaderChange("Leader election in progress".into()))
    }
}

pub(crate) async fn get_membership(
    state: AppStateExt,
    headers: HeaderMap,
    Path(raft_type): Path<RaftType>,
) -> Result<Response, Error> {
    validate_secret(&state, &headers)?;

    if helpers::is_raft_stopped(&state, &raft_type)
        || !helpers::is_raft_initialized(&state, &raft_type).await?
    {
        return Err(Error::Config("Raft node has not been initialized".into()));
    }

    let metrics = helpers::get_raft_metrics(&state, &raft_type).await;
    let mut members = metrics.membership_config;

    // it is possible to end up in a race condition on rolling releases
    if members.nodes().count() == 0 {
        time::sleep(Duration::from_millis(1000)).await;
        let metrics = helpers::get_raft_metrics(&state, &raft_type).await;
        members = metrics.membership_config;
        debug!("Membership after 1000ms timeout: {:?}", members);

        // if we still have no members, return an error
        return Err(Error::Config(
            "Node is initialized but has no members".into(),
        ));
    }

    fmt_ok(headers, members.membership())
}

/// Changes specified learners to members, or remove members.
pub(crate) async fn post_membership(
    state: AppStateExt,
    headers: HeaderMap,
    Path(raft_type): Path<RaftType>,
    body: body::Bytes,
) -> Result<Response, Error> {
    validate_secret(&state, &headers)?;

    if helpers::is_raft_stopped(&state, &raft_type)
        || !helpers::is_raft_initialized(&state, &raft_type).await?
    {
        return Err(Error::Config("Raft node has not been initialized".into()));
    }

    let payload = get_payload::<MembershipChangeReq>(&headers, body)?;
    let _lock = state.raft_lock.lock().await;
    are_we_leader(&state, &raft_type).await?;
    match payload {
        // Apply the delta against the membership OpenRaft sees while holding
        // the same lock as learner promotion. Two sequential operations can no
        // longer overwrite one another with client-snapshotted absolute sets.
        MembershipChangeReq::RemoveVoter { remove_voter } => {
            helpers::remove_voter(&state, &raft_type, remove_voter, false).await?;
        }
        // Older clients snapshotted an absolute set before this request reached
        // the leader. Even under the lock, applying it after a learner
        // promotion could eject that new voter. Accept only an already-applied
        // idempotent set; every real removal must use the delta above.
        MembershipChangeReq::SetVoters(voters) => {
            let current = helpers::get_raft_metrics(&state, &raft_type)
                .await
                .membership_config
                .voter_ids()
                .collect::<BTreeSet<_>>();
            if voters != current {
                return Err(Error::Error(
                    "legacy absolute membership changes are refused; retry with the current node-delta protocol"
                        .into(),
                ));
            }
        }
    }

    fmt_ok(headers, ())
}

/// Queue a pre-emptive election on this voter. Authenticated on the private
/// cluster API; callers must separately observe whether the campaign won.
pub(crate) async fn elect(
    state: AppStateExt,
    headers: HeaderMap,
    Path(raft_type): Path<RaftType>,
) -> Result<Response, Error> {
    validate_secret(&state, &headers)?;
    if helpers::is_raft_stopped(&state, &raft_type)
        || !helpers::is_raft_initialized(&state, &raft_type).await?
    {
        return Err(Error::Config("Raft node has not been initialized".into()));
    }
    helpers::trigger_election(&state, &raft_type).await?;
    fmt_ok(headers, ())
}

#[tracing::instrument(skip_all)]
pub async fn leave_cluster(
    state: AppStateExt,
    headers: HeaderMap,
    Path(raft_type): Path<RaftType>,
    body: body::Bytes,
) -> Result<Response, Error> {
    validate_secret(&state, &headers)?;

    if helpers::is_raft_stopped(&state, &raft_type)
        || !helpers::is_raft_initialized(&state, &raft_type).await?
    {
        return Err(Error::Config("Raft node has not been initialized".into()));
    }
    are_we_leader(&state, &raft_type).await?;

    let payload = get_payload::<ClusterLeaveReq>(&headers, body)?;
    leave_cluster_exec(&state.0, &raft_type, payload).await?;

    Ok(Response::new(Body::empty()))
}

pub async fn leave_cluster_exec(
    state: &Arc<AppState>,
    raft_type: &RaftType,
    payload: ClusterLeaveReq,
) -> Result<(), Error> {
    info!("{:?} Node {:?}", raft_type, payload);

    let lock = state.raft_lock.lock().await;

    let mut metrics = helpers::get_raft_metrics(state, raft_type).await;
    let mut is_member = metrics
        .membership_config
        .nodes()
        .any(|(id, _)| *id == payload.node_id);

    if is_member {
        warn!(
            "Node {} ({:?}) is a cluster member - removing it",
            payload.node_id, raft_type
        );
        let mut is_voter = metrics
            .membership_config
            .voter_ids()
            .any(|id| id == payload.node_id);

        if is_voter {
            warn!("Node {} ({:?}) is a Voter", payload.node_id, raft_type);
            if let Err(err) =
                helpers::remove_voter(state, raft_type, payload.node_id, payload.stay_as_learner)
                    .await
            {
                error!(
                    "Error removing Node {} ({:?}) from Voters: {:?}",
                    payload.node_id, raft_type, err
                );
                return Err(err);
            }
            while is_voter {
                info!(
                    "Waiting until Node {} is not a ({:?}) Voter anymore\nVoter IDs: {:?}\nis_voter: {}",
                    payload.node_id,
                    raft_type,
                    metrics.membership_config.voter_ids().collect::<Vec<_>>(),
                    is_voter
                );
                time::sleep(Duration::from_millis(500)).await;
                metrics = helpers::get_raft_metrics(state, raft_type).await;
                is_voter = metrics
                    .membership_config
                    .voter_ids()
                    .any(|id| id == payload.node_id);
            }
        } else if !payload.stay_as_learner {
            warn!(
                "Node {} ({:?}) is a Learner and should not stay one",
                payload.node_id, raft_type
            );
            if let Err(err) = helpers::remove_learner(state, raft_type, payload.node_id).await {
                error!(
                    "Error removing Node {} ({:?}) from Learners: {:?}",
                    payload.node_id, raft_type, err
                );
                return Err(err);
            }
            while is_member {
                info!(
                    "Waiting until Node {} ({:?}) is not a Learner anymore",
                    payload.node_id, raft_type,
                );
                time::sleep(Duration::from_millis(500)).await;
                metrics = helpers::get_raft_metrics(state, raft_type).await;
                is_member = metrics
                    .membership_config
                    .nodes()
                    .any(|(id, _)| *id == payload.node_id);
            }
        }
    }

    drop(lock);
    info!(
        "Node {} ({:?}) has left the cluster: {:?}",
        payload.node_id,
        raft_type,
        metrics.membership_config.membership()
    );

    Ok(())
}

/// Get the latest metrics of the cluster
pub(crate) async fn metrics(
    state: AppStateExt,
    headers: HeaderMap,
    Path(raft_type): Path<RaftType>,
) -> Result<Response, Error> {
    validate_secret(&state, &headers)?;

    let metrics = helpers::get_raft_metrics(&state, &raft_type).await;
    fmt_ok(headers, &metrics)
}

/// Read this process's bounded SQLite snapshot transport observations.
///
/// The adjacent metrics route uses the same API secret. This projection reads
/// only node-owned memory and therefore remains available while the embedding
/// daemon is still waiting to open its public listener.
pub(crate) async fn snapshot_transport_sqlite(
    Extension(endpoint): Extension<SnapshotTransportEndpoint>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    validate_secret_value(&endpoint.secret_api, &headers)?;
    let mut snapshot = endpoint.status.snapshot();
    snapshot
        .observations
        .retain(|observation| observation.raft_group == "sqlite");
    let mut response = Json(snapshot).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    Ok(response)
}

#[cfg(test)]
mod transport_route_tests {
    use super::*;
    use axum::Router;
    use reqwest::StatusCode;

    #[tokio::test]
    async fn production_transport_route_enforces_auth_and_returns_memory_only_json() {
        let status = crate::LocalSnapshotTransportStatus::new(7, BTreeSet::from([8]));
        let app = Router::new().route(
            "/cluster/transport/sqlite",
            snapshot_transport_sqlite_route("route-secret", status),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind production route test");
        let address = listener.local_addr().expect("route test address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve route test");
        });
        let url = format!("http://{address}/cluster/transport/sqlite");
        // `reqwest` here is `rustls-no-provider`; name the provider before
        // building a client. See `http_client::ensure_rustls_crypto_provider`.
        crate::http_client::ensure_rustls_crypto_provider();
        let client = reqwest::Client::new();

        assert_eq!(
            client
                .get(&url)
                .send()
                .await
                .expect("missing secret")
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .get(&url)
                .header(crate::network::HEADER_NAME_SECRET, "wrong")
                .send()
                .await
                .expect("wrong secret")
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let response = client
            .get(&url)
            .header(crate::network::HEADER_NAME_SECRET, "route-secret")
            .send()
            .await
            .expect("valid secret");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(reqwest::header::CACHE_CONTROL),
            Some(&reqwest::header::HeaderValue::from_static(
                "private, no-store"
            ))
        );
        let snapshot = response
            .json::<crate::SnapshotTransportStatus>()
            .await
            .expect("memory-only status JSON");
        assert_eq!(snapshot.observing_node_id, 7);
        assert!(snapshot.observations.is_empty());

        server.abort();
        let _ = server.await;
    }
}
