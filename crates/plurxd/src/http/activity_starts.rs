//! Activity → Starts: one row per media-session create, fleet-wide.
//!
//! Every create writes a `start_attempt` row to the node-local telemetry
//! sidecar (`playstart::StartAttemptRecord`), and the client's first-frame or
//! failure report settles it (`playstart::StartAttempts::settle`). Nothing
//! here is replicated: each node answers for what it handled, and this module
//! asks every fresh peer for its page, merges the pages by time, and says
//! which peers did not answer rather than leaving their starts out silently.
//!
//! The title's index state is read once per file at the ingress that is
//! asked, from the same projection item detail uses, so a slow start can be
//! tied to "this title has no index" without reading a log.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::{stream, StreamExt};
use plurx_core::domain::{PlaybackEvent, PlaybackEventQuery};
use plurx_core::store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::error::ApiError;
use super::extract::AdminUser;
use super::peer_transport::{deadline_after, PeerAuthMode, PeerTransport, PeerTransportError};
use crate::playstart::{START_DEADLINE, START_FAILURE_EVENTS, START_NOT_YET_CODES};
use crate::state::AppState;

pub(crate) const PEER_PATH: &str = "/_internal/v1/activity-starts";
const DEFAULT_LIMIT: i64 = 25;
const MAX_LIMIT: i64 = 100;
const PEER_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PEER_BYTES: usize = 512 * 1024;
const PEER_CONCURRENCY: usize = 8;
/// How many creates sharing a page's oldest millisecond one node adds to its
/// page ([`local_page`]); a peer's page is at most `MAX_LIMIT` plus these.
const MAX_TIE_ROWS: i64 = MAX_LIMIT;
/// How many settling reports one page reads back from the sidecar.
const SETTLING_ROWS: i64 = 2_000;
/// The index projection is advisory: it must not hold a page of starts up.
const INDEX_DEADLINE: Duration = Duration::from_secs(3);

/// One create as the node that handled it recorded it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct StartRow {
    pub attempt: String,
    pub at_unix_ms: i64,
    /// The node that handled the create (the ingress), not the session owner.
    pub node_id: String,
    pub user_id: Option<i64>,
    pub file_id: Option<i64>,
    pub client_build: String,
    pub route: Option<String>,
    pub refusal: Option<String>,
    pub refusal_message: Option<String>,
    pub retryable: bool,
    pub phases: Value,
    pub owner_node: Option<String>,
    pub elapsed_ms: Option<u64>,
    pub authority_reads: Option<u64>,
    pub local_reads: Option<u64>,
    pub mutations: Option<u64>,
    pub store_ms: Option<u64>,
    pub ttff_ms: Option<i64>,
    /// `presented`, `failed`, `refused`, `pending` or `unobserved`.
    pub outcome: String,
    /// The client event that reported the failure, when one did.
    pub failure: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct StartsParams {
    limit: Option<i64>,
    before: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerRequest {
    limit: i64,
    before_ms: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PeerPage {
    node_id: String,
    rows: Vec<StartRow>,
}

/// One node's contribution to a page.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NodeReport {
    pub node_id: String,
    /// `None` when the node answered; otherwise why it did not.
    pub unavailable: Option<&'static str>,
    pub rows: Vec<StartRow>,
}

fn row_from_attempt(
    event: &PlaybackEvent,
    node_id: &str,
    settled: Option<&Settling>,
    now_ms: i64,
) -> Option<StartRow> {
    let attempt = event.attempt.clone()?;
    let extra = event
        .extra
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or(Value::Null);
    let number = |key: &str| extra.get(key).and_then(Value::as_u64);
    let refusal = event.reason.clone();
    let presented = settled.and_then(|s| s.presented_ms);
    let failure = settled.and_then(|s| s.failure.clone());
    let age_ms = now_ms.saturating_sub(event.at_unix_ms);
    let outcome = if refusal.is_some() {
        "refused"
    } else if presented.is_some() {
        "presented"
    } else if failure.is_some() {
        "failed"
    } else if u128::try_from(age_ms).unwrap_or(0) < START_DEADLINE.as_millis() {
        "pending"
    } else {
        "unobserved"
    };
    Some(StartRow {
        attempt,
        at_unix_ms: event.at_unix_ms,
        node_id: node_id.to_owned(),
        user_id: event.user_id,
        file_id: event.file_id,
        client_build: extra
            .get("client_build")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        route: event.method.clone(),
        retryable: refusal
            .as_deref()
            .is_some_and(|code| START_NOT_YET_CODES.contains(&code)),
        refusal,
        refusal_message: event.detail.clone(),
        phases: extra.get("phases").cloned().unwrap_or_else(|| json!([])),
        owner_node: extra
            .get("owner_node")
            .and_then(Value::as_str)
            .map(str::to_owned),
        elapsed_ms: number("elapsed_ms"),
        authority_reads: number("authority_reads"),
        local_reads: number("local_reads"),
        mutations: number("mutations"),
        store_ms: number("store_ms"),
        ttff_ms: presented,
        outcome: outcome.to_owned(),
        failure,
    })
}

/// What the client reported about one create.
#[derive(Default)]
struct Settling {
    presented_ms: Option<i64>,
    failure: Option<String>,
}

/// This node's newest-first page of creates older than `before_ms`.
///
/// A full page is extended across its oldest millisecond: every create this
/// node holds at that millisecond comes back (up to [`MAX_TIE_ROWS`] more),
/// so the merged page's cursor -- strictly older than its last row -- skips
/// none of them.
pub(crate) async fn local_page(
    store: &Arc<dyn Store>,
    node_id: &str,
    before_ms: Option<i64>,
    limit: i64,
    now_ms: i64,
) -> Result<Vec<StartRow>, ApiError> {
    let limit = limit.clamp(1, MAX_LIMIT);
    let mut attempts = store
        .playback_events(&PlaybackEventQuery {
            event: Some("start_attempt".to_owned()),
            before_ms,
            limit,
            ..PlaybackEventQuery::default()
        })
        .await?;
    let Some(oldest) = attempts.last().map(|event| event.at_unix_ms) else {
        return Ok(Vec::new());
    };
    if attempts.len() >= usize::try_from(limit).unwrap_or(usize::MAX) {
        let tied = store
            .playback_events(&PlaybackEventQuery {
                event: Some("start_attempt".to_owned()),
                since_ms: Some(oldest),
                before_ms: Some(oldest.saturating_add(1)),
                limit: MAX_TIE_ROWS,
                ..PlaybackEventQuery::default()
            })
            .await?;
        attempts.retain(|event| event.at_unix_ms != oldest);
        attempts.extend(tied);
    }
    // The first-frame rows and the failure beacons that settled these creates
    // were written after them, so one read from the oldest create onward
    // holds every one of them; they are matched on (attempt, session).
    let mut names = vec!["start_presented".to_owned()];
    names.extend(START_FAILURE_EVENTS.iter().map(|name| (*name).to_owned()));
    let settling_events = store
        .playback_events(&PlaybackEventQuery {
            events: names,
            since_ms: Some(oldest),
            limit: SETTLING_ROWS,
            ..PlaybackEventQuery::default()
        })
        .await?;
    let mut settled: HashMap<(String, Option<String>), Settling> = HashMap::new();
    for event in settling_events {
        let Some(attempt) = event.attempt.clone() else {
            continue;
        };
        let entry = settled
            .entry((attempt, event.session_id.clone()))
            .or_default();
        if event.event == "start_presented" {
            entry.presented_ms = entry.presented_ms.or(event.ms);
        } else {
            entry.failure.get_or_insert(event.event.clone());
        }
    }
    Ok(attempts
        .iter()
        .filter_map(|event| {
            let key = (event.attempt.clone()?, event.session_id.clone());
            row_from_attempt(event, node_id, settled.get(&key), now_ms)
        })
        .collect())
}

/// Merge the nodes' pages newest first. A page boundary never splits rows
/// that share a millisecond, so the next page (strictly older) loses none.
/// The cursor is present when any node returned a full page or the merge
/// itself cut rows.
pub(crate) fn merge_pages(reports: &[NodeReport], limit: i64) -> (Vec<StartRow>, Option<i64>) {
    let limit = usize::try_from(limit.clamp(1, MAX_LIMIT)).unwrap_or(1);
    let mut rows: Vec<StartRow> = reports
        .iter()
        .flat_map(|report| report.rows.iter().cloned())
        .collect();
    rows.sort_by(|a, b| {
        b.at_unix_ms
            .cmp(&a.at_unix_ms)
            .then_with(|| a.node_id.cmp(&b.node_id))
            .then_with(|| a.attempt.cmp(&b.attempt))
    });
    let full_page = reports
        .iter()
        .any(|report| report.unavailable.is_none() && report.rows.len() >= limit);
    let mut cut = limit.min(rows.len());
    while cut > 0 && cut < rows.len() && rows[cut].at_unix_ms == rows[cut - 1].at_unix_ms {
        cut += 1;
    }
    let truncated = cut < rows.len();
    rows.truncate(cut);
    let next = (truncated || full_page)
        .then(|| rows.last().map(|row| row.at_unix_ms))
        .flatten();
    (rows, next)
}

/// Counts over the rows of one page, with the number of nodes whose rows are
/// missing from it: the observation a reader needs to know how far to trust
/// the page.
pub(crate) fn observation(rows: &[StartRow], unavailable_nodes: usize) -> Value {
    let count = |outcome: &str| rows.iter().filter(|row| row.outcome == outcome).count();
    json!({
        "attempts": rows.len(),
        "presented": count("presented"),
        "failed": count("failed"),
        "refused": count("refused"),
        "pending": count("pending"),
        "unobserved": count("unobserved"),
        "unavailable_nodes": unavailable_nodes,
    })
}

// ---- the fleet read -------------------------------------------------------

fn failure_reason(error: PeerTransportError) -> &'static str {
    match error {
        PeerTransportError::TimedOut => "timed_out",
        PeerTransportError::InvalidResponse => "invalid_response",
        PeerTransportError::Unreachable => "unreachable",
    }
}

/// Ask every committed member -- voters and learners, stale ones included
/// (`operations_peers`) -- for its page. A learner is an ingress too, so its
/// starts are asked for, and a member that is down is named unavailable
/// rather than left out (`activity_peers` lists voters only, which would
/// drop a learner's starts without saying so). `Err` means the directory
/// itself could not be read, so the peers are unknown.
async fn peer_reports(
    state: &AppState,
    before_ms: Option<i64>,
    limit: i64,
) -> Result<Vec<NodeReport>, ()> {
    let peers = state.membership.operations_peers().await.map_err(|_| ())?;
    let transport = PeerTransport::new(state.membership.clone());
    let body = serde_json::to_vec(&PeerRequest { limit, before_ms }).map_err(|_| ())?;
    let deadline = deadline_after(PEER_TIMEOUT);
    let mut reports = stream::iter(peers.into_iter().map(|peer| {
        let transport = transport.clone();
        let body = body.clone();
        async move {
            let node_id = peer.node_id.clone();
            let unavailable = |reason| NodeReport {
                node_id: node_id.clone(),
                unavailable: Some(reason),
                rows: Vec::new(),
            };
            if !peer.reachable {
                return unavailable("unhealthy");
            }
            let Some(base) = peer.http_base.as_deref() else {
                return unavailable("unsupported");
            };
            let answer = transport
                .request(
                    &peer.node_id,
                    base,
                    reqwest::Method::POST,
                    PEER_PATH,
                    body,
                    deadline,
                    MAX_PEER_BYTES,
                    PeerAuthMode::ExactRequestAndMemberResponse,
                )
                .await;
            match answer {
                Err(error) => unavailable(failure_reason(error)),
                Ok(answer) if answer.status.is_success() => {
                    match serde_json::from_slice::<PeerPage>(&answer.body) {
                        Ok(page)
                            if page.node_id == peer.node_id
                                && page.rows.len() <= (MAX_LIMIT + MAX_TIE_ROWS) as usize
                                && page.rows.iter().all(|row| row.node_id == peer.node_id) =>
                        {
                            NodeReport {
                                node_id: peer.node_id,
                                unavailable: None,
                                rows: page.rows,
                            }
                        }
                        _ => unavailable("invalid_response"),
                    }
                }
                Ok(answer) if answer.status == reqwest::StatusCode::UNAUTHORIZED => {
                    unavailable("refused")
                }
                Ok(_) => unavailable("http_error"),
            }
        }
    }))
    .buffer_unordered(PEER_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    reports.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    Ok(reports)
}

/// GET /api/v1/activity/starts — newest-first creates across the fleet.
pub async fn starts(
    _admin: AdminUser,
    State(state): State<AppState>,
    Query(params): Query<StartsParams>,
) -> Result<Json<Value>, ApiError> {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let before_ms = params.before.filter(|before| *before > 0);
    let now_ms = crate::state::clock_ms();
    let local = NodeReport {
        node_id: state.node_id.clone(),
        unavailable: None,
        rows: local_page(&state.store, &state.node_id, before_ms, limit, now_ms).await?,
    };
    let clustered = state.cluster_advertisement && state.membership.is_replicated();
    let (mut reports, directory_unavailable) = if clustered {
        match peer_reports(&state, before_ms, limit).await {
            Ok(reports) => (reports, false),
            Err(()) => (Vec::new(), true),
        }
    } else {
        (Vec::new(), false)
    };
    reports.retain(|report| report.node_id != state.node_id);
    reports.insert(0, local);
    let mut body = assemble(&reports, limit, now_ms);
    decorate(&state, &mut body).await;
    body["node_id"] = json!(state.node_id);
    body["clustered"] = json!(clustered);
    body["directory_unavailable"] = json!(directory_unavailable);
    body["node_hostnames"] = json!(super::system::node_hostnames(&state, true).await);
    Ok(Json(body))
}

/// The page as the wire carries it, before titles and index state are added.
pub(crate) fn assemble(reports: &[NodeReport], limit: i64, now_ms: i64) -> Value {
    let (rows, next_before) = merge_pages(reports, limit);
    let unavailable = reports
        .iter()
        .filter(|report| report.unavailable.is_some())
        .count();
    let nodes = reports
        .iter()
        .map(|report| match report.unavailable {
            None => {
                json!({"node_id": report.node_id, "status": "answered", "rows": report.rows.len()})
            }
            Some(reason) => {
                json!({"node_id": report.node_id, "status": "unavailable", "reason": reason})
            }
        })
        .collect::<Vec<_>>();
    json!({
        "now_ms": now_ms,
        "limit": limit,
        "next_before": next_before,
        "nodes": nodes,
        "unavailable": reports
            .iter()
            .filter(|report| report.unavailable.is_some())
            .map(|report| report.node_id.clone())
            .collect::<Vec<_>>(),
        "observation": observation(&rows, unavailable),
        "rows": rows,
    })
}

// ---- titles, users and the index state ------------------------------------

/// What `index now` would do for one file.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum IndexJob {
    /// A job for this file is queued, running or staged: joining it is all
    /// there is to do.
    Active {
        request_id: String,
        state: String,
    },
    /// The newest job ended failed or cancelled and can be retried.
    Terminal {
        request_id: String,
        state: String,
    },
    None,
}

/// The job by file, from each file's own fragment-index request (the Store
/// answers an active one when there is any, else the newest): active work is
/// joined, a failed or cancelled request is retried, and a superseded one --
/// or a finished one -- leaves the file to be asked afresh.
pub(crate) fn jobs_by_file(
    requests: &[plurx_core::store::AnalysisRequest],
) -> HashMap<i64, IndexJob> {
    requests
        .iter()
        .filter(|request| request.component == "fragment_index")
        .filter_map(|request| {
            let job = match request.state.as_str() {
                "queued" | "running" | "submitted" => IndexJob::Active {
                    request_id: request.request_id.clone(),
                    state: request.state.clone(),
                },
                "failed" | "cancelled" if request.last_error_code != "source_superseded" => {
                    IndexJob::Terminal {
                        request_id: request.request_id.clone(),
                        state: request.state.clone(),
                    }
                }
                _ => return None,
            };
            Some((request.file_id, job))
        })
        .collect()
}

/// The action object a row offers for its title's index.
pub(crate) fn index_action(file_id: i64, status: Option<&str>, job: &IndexJob) -> Value {
    match (status, job) {
        (None | Some("indexed" | "unsupported"), _) => json!({"kind": "none"}),
        (_, IndexJob::Active { request_id, state }) => {
            json!({"kind": "joining", "request_id": request_id, "state": state})
        }
        (_, IndexJob::Terminal { request_id, state }) => json!({
            "kind": "retry",
            "request_id": request_id,
            "state": state,
            "method": "POST",
            "path": format!("/api/v1/analysis/jobs/{request_id}/retry"),
        }),
        (_, IndexJob::None) => json!({
            "kind": "enqueue",
            "method": "POST",
            "path": format!("/api/v1/files/{file_id}/analysis"),
            "body": {"components": ["fragment_index"]},
        }),
    }
}

async fn index_states(state: &AppState, file_ids: &BTreeSet<i64>) -> HashMap<i64, Value> {
    let mut out = HashMap::new();
    let mut files = Vec::new();
    let mut probes = Vec::new();
    for id in file_ids {
        let Ok(Some(file)) = state.catalogue.get_file(*id).await else {
            continue;
        };
        let Ok(probe) = state.catalogue.get_file_probe_json(*id).await else {
            continue;
        };
        probes.push(probe);
        files.push(file);
    }
    if files.is_empty() {
        return out;
    }
    let paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let observations = state.detail_availability.observe_many(&paths).await;
    let have_dovi = crate::ffmpeg::has_dovi_rpu().await;
    let convert = state.transcode.dv_convert_enabled().await;
    let Ok((coverage, _)) = super::browse::vod_index_coverage(
        state,
        &files,
        &probes,
        &observations,
        have_dovi,
        convert,
    )
    .await
    else {
        return out;
    };
    // Each file's own request, however deep the queue is.
    let ids = files
        .iter()
        .map(|file| file.id)
        .take(plurx_core::store::MAX_FILE_REQUEST_LOOKUP)
        .collect::<Vec<_>>();
    let jobs = state
        .store
        .fragment_index_requests_for_files(&ids)
        .await
        .ok()
        .map(|requests| jobs_by_file(&requests));
    for (file, coverage) in files.iter().zip(coverage) {
        let action = match &jobs {
            Some(jobs) => index_action(
                file.id,
                coverage.status,
                jobs.get(&file.id).unwrap_or(&IndexJob::None),
            ),
            None => json!({"kind": "unknown"}),
        };
        out.insert(
            file.id,
            json!({
                "status": coverage.status,
                "refusal": coverage.refusal.map(|(_, detail)| detail),
                "identities": coverage.identities,
                "present": coverage.present,
                "action": action,
            }),
        );
    }
    out
}

/// Add titles, user names and each title's index state to the merged rows.
async fn decorate(state: &AppState, body: &mut Value) {
    let Some(rows) = body.get("rows").and_then(Value::as_array) else {
        return;
    };
    let file_ids: BTreeSet<i64> = rows
        .iter()
        .filter_map(|row| row.get("file_id").and_then(Value::as_i64))
        .collect();
    let user_ids: BTreeSet<i64> = rows
        .iter()
        .filter_map(|row| row.get("user_id").and_then(Value::as_i64))
        .collect();
    let mut titles: BTreeMap<i64, String> = BTreeMap::new();
    for id in &file_ids {
        if let Ok(Some(file)) = state.catalogue.get_file(*id).await {
            if let Ok(Some(item)) = state.catalogue.get_item(file.item_id).await {
                titles.insert(*id, item.title);
            }
        }
    }
    let mut users: BTreeMap<i64, String> = BTreeMap::new();
    for id in &user_ids {
        if let Ok(Some(user)) = state.store.get_user(*id).await {
            users.insert(*id, user.username);
        }
    }
    let index = tokio::time::timeout(INDEX_DEADLINE, index_states(state, &file_ids))
        .await
        .unwrap_or_default();
    if let Some(rows) = body.get_mut("rows").and_then(Value::as_array_mut) {
        for row in rows {
            let file_id = row.get("file_id").and_then(Value::as_i64);
            let user_id = row.get("user_id").and_then(Value::as_i64);
            row["title"] = json!(file_id.and_then(|id| titles.get(&id)));
            row["user"] = json!(user_id.and_then(|id| users.get(&id)));
            row["index"] = file_id
                .and_then(|id| index.get(&id).cloned())
                .unwrap_or_else(|| json!({"status": null, "action": {"kind": "unknown"}}));
        }
    }
}

// ---- the peer side --------------------------------------------------------

/// A voting member or learner serves its own page to a member that signs the
/// request; the response is signed back so the asker knows whose rows these
/// are.
pub(crate) fn eligible(method: &axum::http::Method, path: &str) -> bool {
    method == axum::http::Method::POST && path == PEER_PATH
}

pub(crate) async fn peer(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StatusCode> {
    super::internal_media::authorize(&state, &headers, "POST", PEER_PATH, &body).await?;
    let request: PeerRequest =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    let rows = local_page(
        &state.store,
        &state.node_id,
        request.before_ms.filter(|before| *before > 0),
        request.limit,
        crate::state::clock_ms(),
    )
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let bytes = serde_json::to_vec(&PeerPage {
        node_id: state.node_id.clone(),
        rows,
    })
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if bytes.len() > MAX_PEER_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let auth =
        super::peer_transport::exact_auth_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let payload = super::peer_transport::signed_response_payload(200, &bytes);
    let signature = state
        .membership
        .sign_internal_peer_response(&auth.node_id, &auth.nonce, PEER_PATH, &payload)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let mut response = (StatusCode::OK, bytes).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().insert(
        super::peer_transport::RESPONSE_SIGNATURE_HEADER,
        axum::http::HeaderValue::from_str(&signature)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?,
    );
    Ok(response)
}

#[cfg(test)]
mod tests;
