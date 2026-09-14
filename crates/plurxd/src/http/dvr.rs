//! Recording, the schedule, rules and reminders.
//!
//! Every route here writes *intent* and reads rows. None of them touches a
//! tuner, a file or a disk, and none waits on the owner node — the owner loop
//! reads the same replicated rows on its own fifteen-second tick and is the
//! only writer of `recording` and the terminal states. That split is what lets
//! a viewer on nuc4 stop a capture running on nynuc without a second internal
//! route, and it is why `DELETE` on a live recording answers `202` with a
//! pending flag rather than pretending the file is already closed.
//!
//! The one thing these routes do consult is the owner's guide, and only to
//! copy a programme's facts onto a new row. A recording that has been
//! scheduled never reads the guide again for its own metadata: the guide is a
//! cache of somebody else's data, and a row that re-derived its title on every
//! read would change under the person who scheduled it.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use plurx_core::dvr::{
    is_first_run, normalise_title, DvrAttentionRow, DvrAttentionSection, DvrEvent, DvrEventInput,
    DvrInsertOutcome, DvrKeepMode, DvrMatchMode, DvrOrigin, DvrRecording, DvrRecordingFilter,
    DvrReminder, DvrReminderState, DvrRule, DvrState, DvrStatePatch, DvrTransition,
    DVR_EVENT_PAGE_DEFAULT, DVR_EVENT_PAGE_MAX, DVR_LEAD_MAX_S, DVR_MATCH_VALUE_MAX,
    DVR_MIN_USEFUL_S, DVR_PAD_MAX_S, DVR_RECORDINGS_LIST_PAGE, DVR_REMINDERS_PER_USER_MAX,
    DVR_RULES_MAX, DVR_RULE_NAME_MAX, DVR_SCHEDULE_DAYS_MAX,
};
use plurx_core::store::keys;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::error::ApiError;
use super::extract::{AdminUser, AuthUser};
use crate::live_tv::{DvrConfig, GuideWindow, LiveTvConfig, LiveTvProgramme};
use crate::state::AppState;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/status", get(status))
        .route("/overview", get(overview))
        .route("/recordings", get(list_recordings).post(create_recording))
        .route(
            "/recordings/{id}",
            get(get_recording).delete(delete_recording),
        )
        .route("/recordings/{id}/events", get(recording_events))
        .route(
            "/recordings/{id}/attention/ack",
            post(acknowledge_attention),
        )
        .route("/attention", get(attention))
        .route("/recordings/{id}/restore", post(restore_recording))
        .route("/schedule", get(schedule))
        .route("/rules", get(list_rules).post(create_rule))
        .route("/rules/order", put(reorder_rules))
        .route("/rules/{id}", put(update_rule).delete(delete_rule))
        .route("/reminders", get(list_reminders).post(create_reminder))
        .route("/reminders/{id}", delete(delete_reminder))
        .route("/reminders/{id}/ack", post(ack_reminder))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn(private_no_store))
}

const DVR_OBSERVATION_FRESH_MS: u64 = 20_000;
const DVR_WRITE_HEALTHY_MS: u64 = 10_000;
const DVR_OVERVIEW_ACTIVE_MAX: usize = 64;

#[derive(Clone, Serialize)]
pub(crate) struct DvrOverviewCounts {
    recording: Option<usize>,
    starting: Option<usize>,
    reconnecting: Option<usize>,
    finishing: Option<usize>,
    unconfirmed: Option<usize>,
    attention: Option<usize>,
}

#[derive(Clone, Serialize)]
pub(crate) struct DvrOverviewDiagnostics {
    owner_node_id: String,
    recording_sinks: Option<usize>,
    recording_transports: Option<usize>,
    storage_free_bytes: Option<u64>,
}

#[derive(Clone, Serialize)]
pub(crate) struct DvrActiveProjection {
    recording_id: String,
    channel_id: String,
    airing_start: i64,
    title: String,
    episode_title: Option<String>,
    guide_number: String,
    channel_name: String,
    durable_state: DvrState,
    state_reason: Option<String>,
    airing_end: i64,
    capture_start: i64,
    capture_end: i64,
    stop_requested_at_ms: Option<i64>,
    last_confirmed_bytes: Option<u64>,
    total_bytes_written: Option<u64>,
    observation: Option<crate::live_tv::dvr::DvrCaptureObservation>,
    display_state: String,
    display_detail: String,
    can_stop: bool,
    can_skip: bool,
    can_restore: bool,
    can_delete: bool,
    can_edit_rule: bool,
    can_reorder_rules: bool,
    can_view_diagnostics: bool,
}

#[derive(Clone, Serialize)]
pub(crate) struct DvrOverview {
    version: u8,
    server_now_ms: i64,
    availability: &'static str,
    runtime_supported: bool,
    observation_age_ms: Option<u64>,
    counts: DvrOverviewCounts,
    active_total: Option<usize>,
    active_truncated: bool,
    active: Vec<DvrActiveProjection>,
    next_capture_start: Option<i64>,
    diagnostics: Option<DvrOverviewDiagnostics>,
}

fn unavailable_overview(server_now_ms: i64) -> DvrOverview {
    DvrOverview {
        version: 1,
        server_now_ms,
        availability: "unavailable",
        runtime_supported: true,
        observation_age_ms: None,
        counts: DvrOverviewCounts {
            recording: None,
            starting: None,
            reconnecting: None,
            finishing: None,
            unconfirmed: None,
            attention: None,
        },
        active_total: None,
        active_truncated: false,
        active: Vec::new(),
        next_capture_start: None,
        diagnostics: None,
    }
}

fn attention_worthy(row: &DvrRecording) -> bool {
    matches!(
        row.state,
        DvrState::Conflict
            | DvrState::Withdrawn
            | DvrState::Stale
            | DvrState::Failed
            | DvrState::Missed
    ) || (row.state == DvrState::Partial && (row.gap_s > 0 || row.late_start_s > 0))
}

fn display_state(
    row: &DvrRecording,
    observation: Option<&crate::live_tv::dvr::DvrCaptureObservation>,
    server_now_ms: i64,
) -> (String, String) {
    if row.state != DvrState::Recording {
        let label = match row.state {
            DvrState::Scheduled if row.capture_start.saturating_mul(1_000) <= server_now_ms => {
                "Waiting to start"
            }
            DvrState::Scheduled => "Scheduled",
            DvrState::Conflict => "No tuner",
            DvrState::Withdrawn => "Withdrawn",
            DvrState::Stale => "Programme moved",
            DvrState::Done if row.stopped_by_user_id.is_some() => "Stopped early",
            DvrState::Done => "Recorded",
            DvrState::Partial => "Incomplete",
            DvrState::Failed => "Failed",
            DvrState::Missed => "Missed",
            DvrState::Cancelled => "Skipped",
            DvrState::Deleted => "Deleted",
            DvrState::Recording => unreachable!(),
        };
        return (
            label.to_owned(),
            row.state_reason.clone().unwrap_or_default(),
        );
    }
    if row.stop_requested_at_ms.is_some()
        && observation.is_none_or(|sample| sample.phase != "finishing")
    {
        return (
            "Stop requested".to_owned(),
            observation
                .is_none()
                .then_some("Recorder observation unavailable".to_owned())
                .unwrap_or_default(),
        );
    }
    let Some(sample) =
        observation.filter(|sample| sample.observation_age_ms <= DVR_OBSERVATION_FRESH_MS)
    else {
        return (
            "Status unavailable".to_owned(),
            "No fresh observation from the recorder owner".to_owned(),
        );
    };
    match sample.phase.as_str() {
        "finishing" => ("Finishing".to_owned(), "Closing the capture".to_owned()),
        "reconnecting" => (
            "Reconnecting".to_owned(),
            "A new capture attempt is starting".to_owned(),
        ),
        "starting" => ("Starting".to_owned(), "Waiting for first bytes".to_owned()),
        "writing"
            if sample
                .last_write_age_ms
                .is_some_and(|age| age <= DVR_WRITE_HEALTHY_MS) =>
        {
            ("Recording".to_owned(), "Data is being written".to_owned())
        }
        "writing" => (
            "Recording".to_owned(),
            "No recent data has been written".to_owned(),
        ),
        _ => (
            "Status unavailable".to_owned(),
            "The recorder reported an unknown phase".to_owned(),
        ),
    }
}

pub(crate) async fn collect_overview(
    state: &AppState,
    viewer_id: i64,
    is_admin: bool,
    peers: Option<&super::internal_activity::SharedPeerActivity>,
) -> DvrOverview {
    let server_now_ms = now_ms();
    let settings = match state.store.settings_snapshot().await {
        Ok(settings) => settings,
        Err(error) => {
            tracing::warn!(%error, "DVR overview could not read settings");
            return unavailable_overview(server_now_ms);
        }
    };
    let live_tv = LiveTvConfig::from_snapshot(&settings, &state.node_id);
    state.live_tv.observe_config(&live_tv);
    let states = [
        DvrState::Scheduled,
        DvrState::Conflict,
        DvrState::Withdrawn,
        DvrState::Stale,
        DvrState::Recording,
        DvrState::Done,
        DvrState::Partial,
        DvrState::Failed,
        DvrState::Missed,
        DvrState::Cancelled,
    ];
    let rows = match state.store.list_dvr_recordings_in(&states).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "DVR overview could not read durable recordings");
            return unavailable_overview(server_now_ms);
        }
    };
    let rules = state.store.list_dvr_rules().await.unwrap_or_default();
    let rule_owners = rules
        .into_iter()
        .map(|rule| (rule.id, rule.owner_user_id))
        .collect::<HashMap<_, _>>();

    let (runtime, runtime_supported) = if live_tv.owner_node_id == state.node_id {
        (Some(state.live_tv.capture_observation_snapshot()), true)
    } else {
        let owner = peers.and_then(|outcomes| {
            outcomes.iter().find_map(|(node_id, outcome)| {
                (node_id == &live_tv.owner_node_id).then_some(outcome)
            })
        });
        match owner {
            Some(super::internal_activity::PeerActivityOutcome::Answered(snapshot)) => {
                (snapshot.dvr.clone(), snapshot.dvr.is_some())
            }
            _ => (None, true),
        }
    };
    let runtime_complete = runtime.as_ref().is_some_and(|snapshot| !snapshot.truncated);
    let mut observations = runtime
        .as_ref()
        .map(|snapshot| {
            snapshot
                .observations
                .iter()
                .filter(|sample| {
                    sample.owner_node_id == live_tv.owner_node_id
                        && sample.config_generation == live_tv.generation
                })
                .map(|sample| (sample.recording_id.as_str(), sample))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let observation_age_ms = observations
        .values()
        .map(|sample| sample.observation_age_ms)
        .max();
    let attention = match (
        state
            .store
            .list_dvr_attention(viewer_id, DvrAttentionSection::Current, None, None, 1)
            .await,
        state
            .store
            .list_dvr_attention(viewer_id, DvrAttentionSection::Historical, None, None, 1)
            .await,
    ) {
        (Ok((_, current)), Ok((_, historical))) => {
            current.saturating_add(historical).max(0) as usize
        }
        _ => rows.iter().filter(|row| attention_worthy(row)).count(),
    };
    let now_s = server_now_ms / 1_000;
    let mut active_rows = rows
        .iter()
        .filter(|row| {
            row.state == DvrState::Recording
                || (row.state == DvrState::Scheduled && row.capture_start <= now_s)
        })
        .collect::<Vec<_>>();
    active_rows.sort_by(|left, right| {
        left.capture_start
            .cmp(&right.capture_start)
            .then(left.id.cmp(&right.id))
    });
    let active_total = active_rows.len();
    let active_truncated = active_total > DVR_OVERVIEW_ACTIVE_MAX;
    let mut counts = DvrOverviewCounts {
        recording: Some(0),
        starting: Some(0),
        reconnecting: Some(0),
        finishing: Some(0),
        unconfirmed: Some(0),
        attention: Some(attention),
    };
    let mut active = Vec::with_capacity(active_total.min(DVR_OVERVIEW_ACTIVE_MAX));
    for (index, row) in active_rows.into_iter().enumerate() {
        let observation = observations.remove(row.id.as_str()).filter(|sample| {
            sample.channel_id == row.channel_id
                && sample.airing_start == row.airing_start
                && sample.attempt == row.attempt
                && sample.observation_age_ms <= DVR_OBSERVATION_FRESH_MS
        });
        let (label, detail) = display_state(row, observation, server_now_ms);
        match label.as_str() {
            "Recording" => counts.recording = counts.recording.map(|value| value + 1),
            "Starting" => counts.starting = counts.starting.map(|value| value + 1),
            "Reconnecting" => counts.reconnecting = counts.reconnecting.map(|value| value + 1),
            "Finishing" => counts.finishing = counts.finishing.map(|value| value + 1),
            "Status unavailable" | "Waiting to start" => {
                counts.unconfirmed = counts.unconfirmed.map(|value| value + 1)
            }
            _ => {}
        }
        if index >= DVR_OVERVIEW_ACTIVE_MAX {
            continue;
        }
        let last_confirmed_bytes = observation.map(|sample| sample.attempt_bytes_written);
        let total_bytes_written = observation.and_then(|sample| {
            sample
                .prior_attempt_bytes
                .and_then(|prior| prior.checked_add(sample.attempt_bytes_written))
        });
        let can_edit_rule = row.rule_id.as_ref().is_some_and(|rule_id| {
            is_admin || rule_owners.get(rule_id).copied() == Some(viewer_id)
        });
        active.push(DvrActiveProjection {
            recording_id: row.id.clone(),
            channel_id: row.channel_id.clone(),
            airing_start: row.airing_start,
            title: row.title.clone(),
            episode_title: row.episode_title.clone(),
            guide_number: row.guide_number.clone(),
            channel_name: row.channel_name.clone(),
            durable_state: row.state,
            state_reason: row.state_reason.clone(),
            airing_end: row.airing_end,
            capture_start: row.capture_start,
            capture_end: row.capture_end,
            stop_requested_at_ms: row.stop_requested_at_ms,
            last_confirmed_bytes,
            total_bytes_written,
            observation: observation.cloned(),
            display_state: label,
            display_detail: detail,
            can_stop: row.state == DvrState::Recording,
            can_skip: row.state.is_pending(),
            can_restore: row.state == DvrState::Cancelled && row.capture_end > now_s,
            can_delete: row.state.is_terminal() && row.state != DvrState::Deleted,
            can_edit_rule,
            can_reorder_rules: is_admin,
            can_view_diagnostics: is_admin,
        });
    }
    let next_capture_start = rows
        .iter()
        .filter(|row| row.state == DvrState::Scheduled && row.capture_start > now_s)
        .map(|row| row.capture_start)
        .min();
    let diagnostics = is_admin.then(|| DvrOverviewDiagnostics {
        owner_node_id: live_tv.owner_node_id.clone(),
        recording_sinks: runtime
            .as_ref()
            .map(|snapshot| snapshot.observed_sink_count),
        recording_transports: runtime
            .as_ref()
            .map(|snapshot| snapshot.recording_transports),
        storage_free_bytes: runtime
            .as_ref()
            .and_then(|snapshot| snapshot.storage_free_bytes),
    });
    DvrOverview {
        version: 1,
        server_now_ms,
        availability: if runtime_complete {
            "complete"
        } else {
            "partial"
        },
        runtime_supported,
        observation_age_ms,
        counts,
        active_total: Some(active_total),
        active_truncated,
        active,
        next_capture_start,
        diagnostics,
    }
}

pub(crate) async fn overview(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<Json<DvrOverview>, ApiError> {
    let owner = state
        .store
        .settings_snapshot()
        .await
        .ok()
        .map(|settings| LiveTvConfig::from_snapshot(&settings, &state.node_id).owner_node_id);
    let peers = if state.cluster_advertisement && state.membership.is_replicated() {
        match owner.filter(|owner| owner != &state.node_id) {
            Some(owner) => state
                .peer_activity
                .owner_snapshot(&owner)
                .await
                .ok()
                .map(|outcome| {
                    std::sync::Arc::from(vec![(owner, outcome)].into_boxed_slice())
                        as super::internal_activity::SharedPeerActivity
                }),
            None => None,
        }
    } else {
        None
    };
    Ok(Json(
        collect_overview(&state, user.id, user.is_admin, peers.as_ref()).await,
    ))
}

/// A schedule is a person's own plan, and a reminder names what they intend to
/// watch. Neither belongs in a shared cache.
async fn private_no_store(request: Request<axum::body::Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

// ---- shared reads ---------------------------------------------------------

async fn configs(state: &AppState) -> Result<(LiveTvConfig, DvrConfig), ApiError> {
    let settings = state.store.settings_snapshot().await?;
    let live_tv = LiveTvConfig::from_snapshot(&settings, &state.node_id);
    state.live_tv.observe_config(&live_tv);
    Ok((live_tv, DvrConfig::from_snapshot(&settings)))
}

/// The guide programme at `(channel_id, airing_start)`, with its channel.
///
/// Deliberately an exact start match rather than "whatever is on at that
/// moment": the start is the airing's identity, and a client that asks for a
/// programme the owner's guide no longer has at that second is asking about a
/// schedule that changed underneath it. Saying so is more useful than
/// recording the neighbour.
async fn resolve_airing(
    state: &AppState,
    live_tv: &LiveTvConfig,
    channel_id: &str,
    airing_start: i64,
) -> Option<(ResolvedChannel, LiveTvProgramme)> {
    let window = GuideWindow {
        start: airing_start - 1,
        end: airing_start + 1,
    };
    let guide = super::live_tv::owner_guide(state, live_tv, window).await;
    let channel = guide
        .channels
        .into_iter()
        .find(|candidate| candidate.id == channel_id)?;
    let programme = channel
        .programmes
        .iter()
        .find(|row| row.start == airing_start)
        .cloned()?;
    let name = state
        .live_tv
        .channel_name(live_tv, channel_id)
        .await
        .unwrap_or_else(|| channel.guide_number.clone());
    Some((
        ResolvedChannel {
            id: channel.id,
            guide_number: channel.guide_number,
            name,
        },
        programme,
    ))
}

pub(crate) struct ResolvedChannel {
    // Visible to the engine, which builds rows from guide programmes through
    // the same helper these routes use — a scheduler that copied a programme
    // differently from the way Record does would be a second, divergent
    // definition of what a recording is.
    pub(crate) id: String,
    pub(crate) guide_number: String,
    pub(crate) name: String,
}

fn airing_unknown() -> ApiError {
    ApiError::typed(
        StatusCode::CONFLICT,
        "airing_unknown",
        "the owner's guide has no programme starting at that time on that channel; \
         reload the guide and try again",
    )
}

fn dvr_disabled() -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "dvr_disabled",
        "recording is switched off; an administrator can enable it in Settings → Developer",
    )
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

fn now_seconds() -> i64 {
    crate::live_tv::unix_seconds()
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn lifecycle_event(
    kind: &str,
    occurred_at_ms: i64,
    attempt: Option<i64>,
    actor_user_id: Option<i64>,
    reason_code: Option<&str>,
    facts: serde_json::Value,
    actionable: bool,
) -> DvrEventInput {
    DvrEventInput {
        event_id: new_id(),
        kind: kind.to_owned(),
        occurred_at_ms,
        attempt,
        actor_user_id,
        reason_code: reason_code.map(str::to_owned),
        facts_json: serde_json::to_string(&facts).unwrap_or_else(|_| "{}".to_owned()),
        actionable,
    }
}

// ---- status ---------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct DvrSlots {
    max: u8,
    reserve: u8,
    recording: usize,
}

#[derive(Serialize)]
pub(crate) struct DvrStatus {
    enabled: bool,
    owner_node_id: String,
    root: String,
    /// `None` when the root is unset or unreadable from this node — which is
    /// the common answer on a node that is not the owner, and is not an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    free_bytes: Option<u64>,
    floor_bytes: u64,
    slots: DvrSlots,
    /// When the next scheduled capture opens, so a client can say "next: 8pm"
    /// without reading the whole schedule.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_start: Option<i64>,
    pad_start_s: i64,
    pad_end_s: i64,
    reminder_lead_s: i64,
}

pub(crate) async fn status(
    _user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<DvrStatus>, ApiError> {
    let (live_tv, dvr) = configs(&state).await?;
    let scheduled = state
        .store
        .list_dvr_recordings_in(&[DvrState::Scheduled, DvrState::Recording])
        .await?;
    let next_start = scheduled
        .iter()
        .filter(|row| row.state == DvrState::Scheduled)
        .map(|row| row.capture_start)
        .min();
    Ok(Json(DvrStatus {
        enabled: dvr.enabled,
        owner_node_id: live_tv.owner_node_id.clone(),
        free_bytes: crate::live_tv::free_space_bytes(&dvr.root),
        floor_bytes: (dvr.free_floor_gb.max(0) as u64).saturating_mul(1_000_000_000),
        slots: DvrSlots {
            max: live_tv.max_sessions,
            reserve: dvr.tuner_reserve,
            recording: scheduled
                .iter()
                .filter(|row| row.state == DvrState::Recording)
                .count(),
        },
        root: dvr.root,
        next_start,
        pad_start_s: dvr.pad_start_s,
        pad_end_s: dvr.pad_end_s,
        reminder_lead_s: dvr.reminder_lead_s,
    }))
}

// ---- recordings -----------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct RecordingsQuery {
    state: Option<String>,
    after: Option<String>,
    limit: Option<i64>,
}

#[derive(Serialize)]
pub(crate) struct RecordingsPage {
    rows: Vec<DvrRecording>,
    /// Pass back as `?after=` for the next page. Absent on the last one, so a
    /// client knows it has the whole library rather than guessing from a
    /// short page.
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
}

pub(crate) async fn list_recordings(
    _user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<RecordingsQuery>,
) -> Result<Json<RecordingsPage>, ApiError> {
    let states = parse_states(query.state.as_deref())?;
    let include_deleted = states.contains(&DvrState::Deleted);
    let filter = DvrRecordingFilter {
        states,
        include_deleted,
        before_capture_start: None,
    };
    let limit = query.limit.unwrap_or(DVR_RECORDINGS_LIST_PAGE);
    let rows = state
        .store
        .list_dvr_recordings(&filter, query.after.as_deref(), limit)
        .await?;
    // A cursor only when the page was full: a short page is the end of the
    // library, and offering a cursor there would have every client make one
    // more request to learn nothing.
    let next = (rows.len() as i64 >= limit.clamp(1, DVR_RECORDINGS_LIST_PAGE))
        .then(|| rows.last().map(plurx_core::dvr::recording_cursor))
        .flatten();
    Ok(Json(RecordingsPage { rows, next }))
}

/// `?state=scheduled,recording`. An unknown name is refused rather than
/// ignored: a client filtering on a state this server does not have is a
/// client that will silently show an empty list and blame the server.
fn parse_states(raw: Option<&str>) -> Result<Vec<DvrState>, ApiError> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            DvrState::parse(value)
                .ok_or_else(|| ApiError::BadRequest(format!("unknown recording state `{value}`")))
        })
        .collect()
}

#[derive(Deserialize)]
pub(crate) struct CreateRecording {
    channel_id: String,
    /// Present for a programme from the guide: the server copies its facts.
    airing_start: Option<i64>,
    /// The free-tier story. A viewer whose guide reaches four hours can still
    /// say "record this channel from 8 to 9", and the row carries their title
    /// because nothing else knows one.
    capture_start: Option<i64>,
    capture_end: Option<i64>,
    title: Option<String>,
}

pub(crate) async fn create_recording(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Json(request): Json<CreateRecording>,
) -> Result<(StatusCode, Json<DvrRecording>), ApiError> {
    let (live_tv, dvr) = configs(&state).await?;
    if !dvr.enabled {
        return Err(dvr_disabled());
    }
    let at_ms = now_ms();
    let row = match request.airing_start {
        Some(airing_start) => {
            let (channel, programme) =
                resolve_airing(&state, &live_tv, &request.channel_id, airing_start)
                    .await
                    .ok_or_else(airing_unknown)?;
            recording_from_programme(
                &channel,
                &programme,
                DvrOrigin::Manual,
                None,
                Some(user.id),
                dvr.pad_start_s,
                dvr.pad_end_s,
                at_ms,
            )
        }
        None => manual_recording(&state, &live_tv, &request, user.id, at_ms).await?,
    };
    if row.capture_end - now_seconds() < DVR_MIN_USEFUL_S {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "airing_past",
            "that programme has already finished, or has less than a minute left",
        ));
    }
    let event = lifecycle_event(
        "scheduled",
        at_ms,
        None,
        Some(user.id),
        None,
        serde_json::json!({"origin": row.origin.as_str(), "rule_id": row.rule_id}),
        false,
    );
    match state
        .store
        .insert_dvr_airing_with_event(&row, &event)
        .await?
    {
        DvrInsertOutcome::Inserted => Ok((StatusCode::CREATED, Json(row))),
        DvrInsertOutcome::Exists(_) => {
            // One airing is one row, so an existing one is the answer rather
            // than a conflict: two people pressing Record on the same cell
            // both wanted the same thing and both got it.
            let existing = state
                .store
                .get_dvr_recording_for_airing(&row.channel_id, row.airing_start)
                .await?
                .ok_or_else(airing_unknown)?;
            Ok((StatusCode::OK, Json(existing)))
        }
    }
}

async fn manual_recording(
    state: &AppState,
    live_tv: &LiveTvConfig,
    request: &CreateRecording,
    user_id: i64,
    at_ms: i64,
) -> Result<DvrRecording, ApiError> {
    let (Some(capture_start), Some(capture_end)) = (request.capture_start, request.capture_end)
    else {
        return Err(ApiError::BadRequest(
            "a recording needs either an airing_start from the guide, or a capture_start and \
             capture_end"
                .into(),
        ));
    };
    if capture_end <= capture_start {
        return Err(ApiError::BadRequest(
            "capture_end must be after capture_start".into(),
        ));
    }
    let name = state
        .live_tv
        .channel_name(live_tv, &request.channel_id)
        .await
        .ok_or(ApiError::NotFound("no such channel"))?;
    let guide_number = state
        .live_tv
        .channel_guide_number(live_tv, &request.channel_id)
        .await
        .unwrap_or_else(|| request.channel_id.clone());
    let title = request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&name)
        .to_owned();
    Ok(DvrRecording {
        id: new_id(),
        origin: DvrOrigin::Manual,
        rule_id: None,
        requested_by_user_id: Some(user_id),
        channel_id: request.channel_id.clone(),
        guide_number,
        channel_name: name,
        // With no guide row, the airing *is* the capture: there is no
        // programme whose edges the padding could sit outside.
        airing_start: capture_start,
        airing_end: capture_end,
        capture_start,
        capture_end,
        title,
        episode_title: None,
        episode: None,
        synopsis: None,
        image_url: None,
        original_air_date: None,
        series_id: None,
        programme_id: None,
        state: DvrState::Scheduled,
        state_reason: None,
        attempt: 0,
        gap_s: 0,
        late_start_s: 0,
        tuner_owner_node_id: None,
        path: None,
        bytes: 0,
        last_progress_ms: None,
        stop_requested_at_ms: None,
        stop_requested_by_user_id: None,
        item_id: None,
        file_id: None,
        started_at_ms: None,
        finished_at_ms: None,
        stopped_by_user_id: None,
        created_at_ms: at_ms,
        updated_at_ms: at_ms,
    })
}

/// Copy a guide programme onto a new row, once.
#[allow(clippy::too_many_arguments)]
pub(crate) fn recording_from_programme(
    channel: &ResolvedChannel,
    programme: &LiveTvProgramme,
    origin: DvrOrigin,
    rule_id: Option<String>,
    requested_by_user_id: Option<i64>,
    pad_start_s: i64,
    pad_end_s: i64,
    at_ms: i64,
) -> DvrRecording {
    DvrRecording {
        id: new_id(),
        origin,
        rule_id,
        requested_by_user_id,
        channel_id: channel.id.clone(),
        guide_number: channel.guide_number.clone(),
        channel_name: channel.name.clone(),
        airing_start: programme.start,
        airing_end: programme.end,
        capture_start: programme.start - pad_start_s,
        capture_end: programme.end + pad_end_s,
        title: programme.title.clone(),
        episode_title: programme.episode_title.clone(),
        episode: programme.episode.clone(),
        synopsis: programme.synopsis.clone(),
        image_url: programme.image_url.clone(),
        original_air_date: programme.original_air_date.clone(),
        series_id: programme.series_id.clone(),
        programme_id: programme.programme_id.clone(),
        state: DvrState::Scheduled,
        state_reason: None,
        attempt: 0,
        gap_s: 0,
        late_start_s: 0,
        tuner_owner_node_id: None,
        path: None,
        bytes: 0,
        last_progress_ms: None,
        stop_requested_at_ms: None,
        stop_requested_by_user_id: None,
        item_id: None,
        file_id: None,
        started_at_ms: None,
        finished_at_ms: None,
        stopped_by_user_id: None,
        created_at_ms: at_ms,
        updated_at_ms: at_ms,
    }
}

pub(crate) async fn get_recording(
    _user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DvrRecording>, ApiError> {
    state
        .store
        .get_dvr_recording(&id)
        .await?
        .map(Json)
        .ok_or(ApiError::NotFound("no such recording"))
}

#[derive(Deserialize)]
pub(crate) struct EventsQuery {
    before: Option<String>,
    after: Option<String>,
    limit: Option<i64>,
}

#[derive(Serialize)]
pub(crate) struct EventsPage {
    rows: Vec<DvrEventView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
    history_complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    truncated_before_sequence: Option<i64>,
}

#[derive(Serialize)]
pub(crate) struct DvrEventView {
    recording_id: String,
    sequence: i64,
    event_id: String,
    kind: String,
    occurred_at_ms: i64,
    attempt: Option<i64>,
    reason_code: Option<String>,
    facts: serde_json::Value,
}

impl From<DvrEvent> for DvrEventView {
    fn from(event: DvrEvent) -> Self {
        Self {
            recording_id: event.recording_id,
            sequence: event.sequence,
            event_id: event.event_id,
            kind: event.kind,
            occurred_at_ms: event.occurred_at_ms,
            attempt: event.attempt,
            reason_code: event.reason_code,
            facts: event.facts,
        }
    }
}

fn sequence_cursor(value: Option<&str>, name: &str) -> Result<Option<i64>, ApiError> {
    value
        .map(|raw| {
            raw.parse::<i64>()
                .ok()
                .filter(|value| *value >= 0)
                .ok_or_else(|| {
                    ApiError::typed(
                        StatusCode::BAD_REQUEST,
                        "invalid_cursor",
                        format!("{name} must be a non-negative event sequence"),
                    )
                })
        })
        .transpose()
}

pub(crate) async fn recording_events(
    _user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<EventsPage>, ApiError> {
    if query.before.is_some() && query.after.is_some() {
        return Err(ApiError::typed(
            StatusCode::BAD_REQUEST,
            "ambiguous_cursor",
            "before and after are mutually exclusive",
        ));
    }
    if state.store.get_dvr_recording(&id).await?.is_none() {
        return Err(ApiError::NotFound("no such recording"));
    }
    let before = sequence_cursor(query.before.as_deref(), "before")?;
    let after = sequence_cursor(query.after.as_deref(), "after")?;
    let limit = query
        .limit
        .unwrap_or(DVR_EVENT_PAGE_DEFAULT)
        .clamp(1, DVR_EVENT_PAGE_MAX);
    let page = state
        .store
        .list_dvr_events(&id, before, after, limit)
        .await?;
    let next = (page.rows.len() as i64 >= limit)
        .then(|| page.rows.last().map(|event| event.sequence.to_string()))
        .flatten();
    let history_complete = page
        .head
        .as_ref()
        .is_some_and(|head| !head.history_has_gap && head.pruned_through_sequence == 0);
    let truncated_before_sequence = page.head.as_ref().and_then(|head| {
        (head.pruned_through_sequence > 0).then_some(head.pruned_through_sequence)
    });
    Ok(Json(EventsPage {
        rows: page.rows.into_iter().map(DvrEventView::from).collect(),
        next,
        history_complete,
        truncated_before_sequence,
    }))
}

#[derive(Deserialize)]
pub(crate) struct AttentionAckRequest {
    through_sequence: i64,
}

#[derive(Serialize)]
pub(crate) struct AttentionAckResponse {
    recording_id: String,
    through_sequence: i64,
}

pub(crate) async fn acknowledge_attention(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<AttentionAckRequest>,
) -> Result<Json<AttentionAckResponse>, ApiError> {
    let at_ms = now_ms();
    let baseline = lifecycle_event(
        "legacy_outcome",
        at_ms,
        None,
        Some(user.id),
        Some("history_not_collected"),
        serde_json::json!({"baseline_created_at_ms": at_ms}),
        true,
    );
    let through_sequence = state
        .store
        .acknowledge_dvr_attention(&id, user.id, request.through_sequence, at_ms, &baseline)
        .await?
        .ok_or_else(|| {
            ApiError::typed(
                StatusCode::BAD_REQUEST,
                "invalid_attention_sequence",
                "that sequence is not part of this recording's retained history",
            )
        })?;
    Ok(Json(AttentionAckResponse {
        recording_id: id,
        through_sequence,
    }))
}

#[derive(Deserialize)]
pub(crate) struct AttentionQuery {
    after: Option<String>,
    limit: Option<i64>,
}

#[derive(Serialize)]
pub(crate) struct AttentionPage {
    rows: Vec<AttentionProjection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
    total: i64,
}

#[derive(Serialize)]
pub(crate) struct AttentionProjection {
    recording: DvrRecording,
    latest_attention_sequence: i64,
    latest_attention_at_ms: i64,
    acknowledged_through_sequence: i64,
}

impl From<DvrAttentionRow> for AttentionProjection {
    fn from(row: DvrAttentionRow) -> Self {
        Self {
            recording: row.recording,
            latest_attention_sequence: row.latest_attention_sequence,
            latest_attention_at_ms: row.latest_attention_at_ms,
            acknowledged_through_sequence: row.acknowledged_through_sequence,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum AttentionCursorSection {
    Current,
    Historical,
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
struct AttentionCursor {
    version: u8,
    section: AttentionCursorSection,
    after_at_ms: i64,
    after_id: String,
    current_upper: Option<(i64, String)>,
    historical_upper: Option<(i64, String)>,
}

fn invalid_attention_cursor() -> ApiError {
    ApiError::typed(
        StatusCode::BAD_REQUEST,
        "invalid_cursor",
        "invalid attention cursor",
    )
}

fn parse_attention_cursor(raw: Option<&str>) -> Result<Option<AttentionCursor>, ApiError> {
    let Some(raw) = raw else { return Ok(None) };
    if raw.len() > 2_048 || raw.len() % 2 != 0 {
        return Err(invalid_attention_cursor());
    }
    let bytes = raw
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).map_err(|_| invalid_attention_cursor())?;
            u8::from_str_radix(text, 16).map_err(|_| invalid_attention_cursor())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cursor: AttentionCursor =
        serde_json::from_slice(&bytes).map_err(|_| invalid_attention_cursor())?;
    if cursor.version != 1
        || cursor.after_id.is_empty()
        || cursor
            .current_upper
            .as_ref()
            .is_some_and(|(_, id)| id.is_empty())
        || cursor
            .historical_upper
            .as_ref()
            .is_some_and(|(_, id)| id.is_empty())
    {
        return Err(invalid_attention_cursor());
    }
    Ok(Some(cursor))
}

fn encode_attention_cursor(cursor: &AttentionCursor) -> Result<String, ApiError> {
    let bytes = serde_json::to_vec(cursor)
        .map_err(|_| ApiError::Internal("could not encode attention cursor".to_owned()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub(crate) async fn attention(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Query(query): Query<AttentionQuery>,
) -> Result<Json<AttentionPage>, ApiError> {
    let limit = query
        .limit
        .unwrap_or(DVR_EVENT_PAGE_DEFAULT)
        .clamp(1, DVR_EVENT_PAGE_MAX);
    let cursor = parse_attention_cursor(query.after.as_deref())?;
    let mut rows = Vec::with_capacity(limit as usize);
    let mut next_section = AttentionCursorSection::Current;
    let mut current_upper = cursor
        .as_ref()
        .and_then(|value| value.current_upper.clone());
    let mut historical_upper = cursor
        .as_ref()
        .and_then(|value| value.historical_upper.clone());
    let current_total;
    let historical_total;

    // Freeze both section ceilings when traversal starts. Historical rows
    // must not appear midway through a long current-condition traversal just
    // because a new failure happened after page one was served.
    if cursor.is_none() {
        let (top, _) = state
            .store
            .list_dvr_attention(user.id, DvrAttentionSection::Historical, None, None, 1)
            .await?;
        historical_upper = top
            .first()
            .map(|row| (row.latest_attention_at_ms, row.recording.id.clone()));
    }

    if !matches!(
        cursor.as_ref().map(|value| value.section),
        Some(AttentionCursorSection::Historical)
    ) {
        let current_after = cursor
            .as_ref()
            .map(|value| (value.after_at_ms, value.after_id.as_str()));
        let (page, total) = state
            .store
            .list_dvr_attention(
                user.id,
                DvrAttentionSection::Current,
                current_after,
                current_upper.as_ref().map(|(at, id)| (*at, id.as_str())),
                limit,
            )
            .await?;
        current_total = total;
        if current_upper.is_none() {
            current_upper = page
                .first()
                .map(|row| (row.latest_attention_at_ms, row.recording.id.clone()));
        }
        rows.extend(page);
    } else {
        current_total = state
            .store
            .list_dvr_attention(user.id, DvrAttentionSection::Current, None, None, 1)
            .await?
            .1;
    }

    if rows.len() < limit as usize {
        next_section = AttentionCursorSection::Historical;
        let historical_after = cursor.as_ref().and_then(|value| {
            matches!(value.section, AttentionCursorSection::Historical)
                .then_some((value.after_at_ms, value.after_id.as_str()))
        });
        let remaining = limit.saturating_sub(rows.len() as i64);
        let (page, total) = state
            .store
            .list_dvr_attention(
                user.id,
                DvrAttentionSection::Historical,
                historical_after,
                historical_upper.as_ref().map(|(at, id)| (*at, id.as_str())),
                remaining,
            )
            .await?;
        historical_total = total;
        if historical_upper.is_none() {
            historical_upper = page
                .first()
                .map(|row| (row.latest_attention_at_ms, row.recording.id.clone()));
        }
        rows.extend(page);
    } else {
        historical_total = state
            .store
            .list_dvr_attention(user.id, DvrAttentionSection::Historical, None, None, 1)
            .await?
            .1;
    }

    let next = if rows.len() as i64 >= limit {
        rows.last()
            .map(|row| {
                encode_attention_cursor(&AttentionCursor {
                    version: 1,
                    section: next_section,
                    after_at_ms: row.latest_attention_at_ms,
                    after_id: row.recording.id.clone(),
                    current_upper,
                    historical_upper,
                })
            })
            .transpose()?
    } else {
        None
    };
    let total = current_total.saturating_add(historical_total);
    Ok(Json(AttentionPage {
        rows: rows.into_iter().map(Into::into).collect(),
        next,
        total,
    }))
}

#[derive(Deserialize)]
pub(crate) struct DeleteRecordingQuery {
    delete_file: Option<u8>,
}

#[derive(Serialize)]
pub(crate) struct StopPending {
    pending: bool,
    requested_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_by_user_id: Option<i64>,
}

/// One verb, four meanings, decided by what the row is doing.
///
/// A planned airing is cancelled; a running capture is asked to stop and the
/// owner's tick consumes the request; a finished recording is deleted along
/// with its file, but only when the caller says so in the URL, because a
/// client that means "take this off my schedule" and a client that means
/// "delete the file" must not be the same request by accident.
pub(crate) async fn delete_recording(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<DeleteRecordingQuery>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;

    let row = state
        .store
        .get_dvr_recording(&id)
        .await?
        .ok_or(ApiError::NotFound("no such recording"))?;
    let at_ms = now_ms();
    match row.state {
        state_value if state_value.is_pending() => {
            let event = lifecycle_event(
                "cancelled",
                at_ms,
                Some(row.attempt),
                Some(user.id),
                Some("cancelled"),
                serde_json::json!({}),
                false,
            );
            state
                .store
                .transition_dvr_recording_with_event(
                    &DvrTransition {
                        id: &id,
                        from: DvrState::PENDING,
                        to: DvrState::Cancelled,
                        reason: Some("cancelled"),
                        patch: DvrStatePatch::None,
                        fence_generation: None,
                        now_ms: at_ms,
                    },
                    &event,
                )
                .await?;
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        DvrState::Recording => {
            let event = lifecycle_event(
                "stop_requested",
                at_ms,
                Some(row.attempt),
                Some(user.id),
                None,
                serde_json::json!({}),
                false,
            );
            let requested = state
                .store
                .request_dvr_stop_with_event(&id, at_ms, user.id, &event)
                .await?
                .ok_or(ApiError::NotFound("no such recording"))?;
            Ok((
                StatusCode::ACCEPTED,
                Json(StopPending {
                    pending: true,
                    requested_at: requested.stop_requested_at_ms.unwrap_or(at_ms),
                    requested_by_user_id: requested.stop_requested_by_user_id,
                }),
            )
                .into_response())
        }
        DvrState::Deleted => Ok(StatusCode::NO_CONTENT.into_response()),
        _terminal => {
            if query.delete_file != Some(1) {
                return Err(ApiError::typed(
                    StatusCode::CONFLICT,
                    "delete_file_required",
                    "this recording has a file; repeat the request with ?delete_file=1 to remove it",
                ));
            }
            let event = lifecycle_event(
                "deleted",
                at_ms,
                Some(row.attempt),
                Some(user.id),
                Some("deleted_by_request"),
                serde_json::json!({"delete_file": true}),
                false,
            );
            state
                .store
                .transition_dvr_recording_with_event(
                    &DvrTransition {
                        id: &id,
                        from: &[
                            DvrState::Done,
                            DvrState::Partial,
                            DvrState::Failed,
                            DvrState::Missed,
                            DvrState::Cancelled,
                        ],
                        to: DvrState::Deleted,
                        reason: Some("deleted by request"),
                        patch: DvrStatePatch::None,
                        fence_generation: None,
                        now_ms: at_ms,
                    },
                    &event,
                )
                .await?;
            Ok(StatusCode::NO_CONTENT.into_response())
        }
    }
}

/// The only way back from a cancel, and deliberately explicit: rule expansion
/// will not do it, however many times the rule matches again.
pub(crate) async fn restore_recording(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DvrRecording>, ApiError> {
    let row = state
        .store
        .get_dvr_recording(&id)
        .await?
        .ok_or(ApiError::NotFound("no such recording"))?;
    if row.capture_end - now_seconds() < DVR_MIN_USEFUL_S {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "airing_past",
            "that programme has already finished, or has less than a minute left",
        ));
    }
    let at_ms = now_ms();
    let event = lifecycle_event(
        "restored",
        at_ms,
        Some(row.attempt),
        Some(user.id),
        None,
        serde_json::json!({}),
        false,
    );
    let restored = state
        .store
        .transition_dvr_recording_with_event(
            &DvrTransition {
                id: &id,
                from: &[DvrState::Cancelled],
                to: DvrState::Scheduled,
                reason: None,
                patch: DvrStatePatch::None,
                fence_generation: None,
                now_ms: at_ms,
            },
            &event,
        )
        .await?;
    if !restored {
        return Err(ApiError::Conflict(
            "only a cancelled recording can be restored".into(),
        ));
    }
    state
        .store
        .get_dvr_recording(&id)
        .await?
        .map(Json)
        .ok_or(ApiError::NotFound("no such recording"))
}

// ---- schedule -------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct DvrSchedule {
    /// How many rows have no tuner. The number the UI puts on the Scheduled
    /// chip, so a viewer learns about a clash without opening the list.
    conflicts: usize,
    rows: Vec<DvrRecording>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
}

pub(crate) async fn schedule(
    _user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<Vec<(String, String)>>,
) -> Result<Json<DvrSchedule>, ApiError> {
    let value = |name: &str| {
        query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    };
    let number = |name: &str| -> Result<Option<i64>, ApiError> {
        value(name)
            .map(|raw| {
                raw.parse::<i64>()
                    .map_err(|_| ApiError::BadRequest(format!("invalid {name}")))
            })
            .transpose()
    };
    let filtered = query.iter().any(|(key, _)| {
        matches!(
            key.as_str(),
            "from" | "to" | "channel_id" | "after" | "limit"
        )
    });
    let days = number("days")?
        .unwrap_or(DVR_SCHEDULE_DAYS_MAX)
        .clamp(1, DVR_SCHEDULE_DAYS_MAX);
    let now = now_seconds();
    let from = number("from")?.unwrap_or_else(|| if filtered { now - 43_200 } else { now });
    let to = number("to")?.unwrap_or_else(|| {
        if filtered {
            from + 86_400
        } else {
            now + days * 86_400
        }
    });
    if filtered && (to <= from || to - from > 86_400) {
        return Err(ApiError::BadRequest(
            "schedule window must be positive and no longer than 24 hours".into(),
        ));
    }
    let channels = query
        .iter()
        .filter(|(key, _)| key == "channel_id")
        .map(|(_, value)| value.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if channels.len() > 64 {
        return Err(ApiError::BadRequest(
            "a schedule window accepts at most 64 channels".into(),
        ));
    }
    let after = value("after")
        .map(|cursor| {
            let (start, id) = cursor
                .split_once(':')
                .ok_or_else(|| ApiError::BadRequest("invalid schedule cursor".into()))?;
            let start = start
                .parse::<i64>()
                .map_err(|_| ApiError::BadRequest("invalid schedule cursor".into()))?;
            if id.is_empty() {
                return Err(ApiError::BadRequest("invalid schedule cursor".into()));
            }
            Ok((start, id))
        })
        .transpose()?;
    let limit = number("limit")?
        .unwrap_or(DVR_EVENT_PAGE_DEFAULT)
        .clamp(1, 100) as usize;
    let mut states = vec![
        DvrState::Scheduled,
        DvrState::Conflict,
        DvrState::Withdrawn,
        DvrState::Stale,
        DvrState::Recording,
    ];
    if value("cancelled").is_some_and(|value| value == "1") || filtered {
        // Skip stays visible and reversible: a viewer who skipped the wrong
        // episode needs to find it again to restore it.
        states.push(DvrState::Cancelled);
    }
    if filtered {
        states.extend([
            DvrState::Done,
            DvrState::Partial,
            DvrState::Failed,
            DvrState::Missed,
        ]);
    }
    let mut rows = state
        .store
        .list_dvr_recordings_in(&states)
        .await?
        .into_iter()
        .filter(|row| {
            row.capture_start < to
                && (!filtered || row.capture_end > from)
                && (channels.is_empty() || channels.contains(row.channel_id.as_str()))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.capture_start
            .cmp(&right.capture_start)
            .then(left.id.cmp(&right.id))
    });
    let conflicts = rows
        .iter()
        .filter(|row| row.state == DvrState::Conflict)
        .count();
    if let Some((start, id)) = after {
        rows.retain(|row| (row.capture_start, row.id.as_str()) > (start, id));
    }
    let next = if filtered && rows.len() > limit {
        rows.truncate(limit);
        rows.last()
            .map(|row| format!("{}:{}", row.capture_start, row.id))
    } else {
        None
    };
    Ok(Json(DvrSchedule {
        conflicts,
        rows,
        next,
    }))
}

// ---- rules ----------------------------------------------------------------

pub(crate) async fn list_rules(
    _user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<DvrRule>>, ApiError> {
    Ok(Json(state.store.list_dvr_rules().await?))
}

#[derive(Deserialize)]
pub(crate) struct FromAiring {
    channel_id: String,
    airing_start: i64,
}

#[derive(Deserialize)]
pub(crate) struct RuleBody {
    /// Fill mode, value and channel from a guide cell — how "Record series"
    /// creates a rule in two presses rather than a form.
    from_airing: Option<FromAiring>,
    name: Option<String>,
    match_mode: Option<String>,
    match_value: Option<String>,
    channel_id: Option<String>,
    /// Explicit `false` means "any channel", which is different from absent.
    any_channel: Option<bool>,
    new_only: Option<bool>,
    keep_mode: Option<String>,
    keep_value: Option<i64>,
    pad_start_s: Option<i64>,
    pad_end_s: Option<i64>,
    enabled: Option<bool>,
}

pub(crate) async fn create_rule(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Json(body): Json<RuleBody>,
) -> Result<(StatusCode, Json<DvrRule>), ApiError> {
    let (live_tv, dvr) = configs(&state).await?;
    if !dvr.enabled {
        return Err(dvr_disabled());
    }
    let existing = state.store.list_dvr_rules().await?;
    if existing.len() as i64 >= DVR_RULES_MAX {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "rule_limit",
            format!("this server already has the maximum of {DVR_RULES_MAX} recording rules"),
        ));
    }
    let at_ms = now_ms();
    let mut rule = DvrRule {
        id: new_id(),
        owner_user_id: user.id,
        // New rules go last: an existing schedule keeps the tuners it was
        // already planned to get, and the operator reorders deliberately.
        priority: existing.iter().map(|rule| rule.priority).max().unwrap_or(0) + 1,
        name: String::new(),
        match_mode: DvrMatchMode::Title,
        match_value: String::new(),
        channel_id: None,
        new_only: true,
        keep_mode: DvrKeepMode::All,
        keep_value: 0,
        pad_start_s: dvr.pad_start_s,
        pad_end_s: dvr.pad_end_s,
        enabled: true,
        created_at_ms: at_ms,
        updated_at_ms: at_ms,
    };
    if let Some(from) = &body.from_airing {
        let (channel, programme) =
            resolve_airing(&state, &live_tv, &from.channel_id, from.airing_start)
                .await
                .ok_or_else(airing_unknown)?;
        rule.name = programme.title.clone();
        match programme.series_id.clone() {
            // An id survives a renamed programme and a channel change, so when
            // the source gives one it is strictly better than the title — and
            // the rule stores which, because the UI has to be able to say
            // "title match · 7.1 only" rather than implying an exactness it
            // does not have.
            Some(series_id) => {
                rule.match_mode = DvrMatchMode::SeriesId;
                rule.match_value = series_id;
            }
            None => {
                rule.match_mode = DvrMatchMode::Title;
                rule.match_value = normalise_title(&programme.title);
                rule.channel_id = Some(channel.id);
            }
        }
    }
    apply_rule_body(&mut rule, &body, at_ms)?;
    validate_rule(&rule)?;
    if !state.store.put_dvr_rule(&rule).await? {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "rule_limit",
            format!("this server already has the maximum of {DVR_RULES_MAX} recording rules"),
        ));
    }
    Ok((StatusCode::CREATED, Json(rule)))
}

pub(crate) async fn update_rule(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RuleBody>,
) -> Result<Json<DvrRule>, ApiError> {
    let mut rule = state
        .store
        .get_dvr_rule(&id)
        .await?
        .ok_or(ApiError::NotFound("no such rule"))?;
    if rule.owner_user_id != user.id && !user.is_admin {
        return Err(ApiError::Forbidden);
    }
    apply_rule_body(&mut rule, &body, now_ms())?;
    validate_rule(&rule)?;
    state.store.put_dvr_rule(&rule).await?;
    Ok(Json(rule))
}

fn apply_rule_body(rule: &mut DvrRule, body: &RuleBody, at_ms: i64) -> Result<(), ApiError> {
    if let Some(name) = body.name.as_deref().map(str::trim) {
        rule.name = name.to_owned();
    }
    if let Some(mode) = body.match_mode.as_deref() {
        rule.match_mode = DvrMatchMode::parse(mode)
            .ok_or_else(|| ApiError::BadRequest(format!("unknown match mode `{mode}`")))?;
    }
    if let Some(value) = body.match_value.as_deref().map(str::trim) {
        rule.match_value = match rule.match_mode {
            DvrMatchMode::Title => normalise_title(value),
            DvrMatchMode::SeriesId => value.to_owned(),
        };
    }
    if body.any_channel == Some(true) {
        rule.channel_id = None;
    } else if let Some(channel_id) = body.channel_id.as_deref().map(str::trim) {
        rule.channel_id = (!channel_id.is_empty()).then(|| channel_id.to_owned());
    }
    if let Some(new_only) = body.new_only {
        rule.new_only = new_only;
    }
    if let Some(mode) = body.keep_mode.as_deref() {
        rule.keep_mode = DvrKeepMode::parse(mode)
            .ok_or_else(|| ApiError::BadRequest(format!("unknown keep mode `{mode}`")))?;
    }
    if let Some(value) = body.keep_value {
        rule.keep_value = value.max(0);
    }
    if let Some(value) = body.pad_start_s {
        rule.pad_start_s = value.clamp(0, DVR_PAD_MAX_S);
    }
    if let Some(value) = body.pad_end_s {
        rule.pad_end_s = value.clamp(0, DVR_PAD_MAX_S);
    }
    if let Some(enabled) = body.enabled {
        rule.enabled = enabled;
    }
    rule.updated_at_ms = at_ms;
    Ok(())
}

fn validate_rule(rule: &DvrRule) -> Result<(), ApiError> {
    if rule.name.is_empty() || rule.name.len() > DVR_RULE_NAME_MAX {
        return Err(ApiError::BadRequest(format!(
            "a rule needs a name of 1 to {DVR_RULE_NAME_MAX} bytes"
        )));
    }
    if rule.match_value.is_empty() || rule.match_value.len() > DVR_MATCH_VALUE_MAX {
        return Err(ApiError::BadRequest(format!(
            "a rule needs something to match on, at most {DVR_MATCH_VALUE_MAX} bytes"
        )));
    }
    if rule.keep_mode == DvrKeepMode::LastN && rule.keep_value < 1 {
        return Err(ApiError::BadRequest(
            "keeping the last N episodes needs an N of at least 1".into(),
        ));
    }
    if rule.keep_mode == DvrKeepMode::Days && rule.keep_value < 1 {
        return Err(ApiError::BadRequest(
            "keeping recordings for a number of days needs at least one day".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn delete_rule(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let rule = state
        .store
        .get_dvr_rule(&id)
        .await?
        .ok_or(ApiError::NotFound("no such rule"))?;
    if rule.owner_user_id != user.id && !user.is_admin {
        return Err(ApiError::Forbidden);
    }
    // The rows it materialised are not cancelled here. The owner's next tick
    // withdraws the pending ones, which is a different word deliberately:
    // `cancelled` is a person's decision about one airing, and deleting a rule
    // is not that.
    state.store.delete_dvr_rule(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(crate) struct RuleOrder {
    ids: Vec<String>,
}

pub(crate) async fn reorder_rules(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<RuleOrder>,
) -> Result<Json<Vec<DvrRule>>, ApiError> {
    if !state.store.reorder_dvr_rules(&body.ids, now_ms()).await? {
        return Err(ApiError::BadRequest(
            "the order must name every rule exactly once".into(),
        ));
    }
    Ok(Json(state.store.list_dvr_rules().await?))
}

// ---- reminders ------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct RemindersQuery {
    due: Option<u8>,
}

#[derive(Serialize)]
pub(crate) struct DueReminder {
    #[serde(flatten)]
    reminder: DvrReminder,
    /// Whether a recording already covers this airing, so the overlay can say
    /// "recording" instead of offering to start one.
    covered_by_recording: bool,
}

pub(crate) async fn list_reminders(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Query(query): Query<RemindersQuery>,
) -> Result<Json<Vec<DueReminder>>, ApiError> {
    let wanted = (query.due == Some(1)).then_some(DvrReminderState::Fired);
    let reminders = state.store.list_dvr_reminders(user.id, wanted).await?;
    if reminders.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let covering = state
        .store
        .list_dvr_recordings_in(&[DvrState::Scheduled, DvrState::Recording])
        .await?;
    Ok(Json(
        reminders
            .into_iter()
            .map(|reminder| DueReminder {
                covered_by_recording: covering.iter().any(|row| {
                    row.channel_id == reminder.channel_id
                        && row.airing_start == reminder.airing_start
                }),
                reminder,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub(crate) struct CreateReminder {
    channel_id: String,
    airing_start: i64,
    lead_s: Option<i64>,
}

pub(crate) async fn create_reminder(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Json(request): Json<CreateReminder>,
) -> Result<(StatusCode, Json<DvrReminder>), ApiError> {
    let (live_tv, dvr) = configs(&state).await?;
    // Deliberately not gated on `dvr.enabled`: a reminder is a row with a time
    // in it. It needs neither a tuner nor a disk, and refusing one because
    // recording is off would be a gate on a feature that does not use it.
    let (channel, programme) =
        resolve_airing(&state, &live_tv, &request.channel_id, request.airing_start)
            .await
            .ok_or_else(airing_unknown)?;
    if programme.start <= now_seconds() {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "airing_past",
            "that programme has already started",
        ));
    }
    // Armed and fired both count, matching what the Store charges: a fired
    // reminder is still on someone's screen waiting to be acknowledged.
    let mut existing = state
        .store
        .list_dvr_reminders(user.id, Some(DvrReminderState::Armed))
        .await?;
    existing.extend(
        state
            .store
            .list_dvr_reminders(user.id, Some(DvrReminderState::Fired))
            .await?,
    );
    if existing.len() as i64 >= DVR_REMINDERS_PER_USER_MAX {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "reminder_limit",
            format!("you already have {DVR_REMINDERS_PER_USER_MAX} reminders set"),
        ));
    }
    let at_ms = now_ms();
    let reminder = DvrReminder {
        id: existing
            .iter()
            .find(|candidate| {
                candidate.channel_id == channel.id && candidate.airing_start == programme.start
            })
            .map(|candidate| candidate.id.clone())
            .unwrap_or_else(new_id),
        user_id: user.id,
        channel_id: channel.id,
        guide_number: channel.guide_number,
        airing_start: programme.start,
        airing_end: programme.end,
        title: programme.title,
        lead_s: request
            .lead_s
            .unwrap_or(dvr.reminder_lead_s)
            .clamp(0, DVR_LEAD_MAX_S),
        state: DvrReminderState::Armed,
        fired_at_ms: None,
        acked_at_ms: None,
        created_at_ms: at_ms,
        updated_at_ms: at_ms,
    };
    if !state.store.put_dvr_reminder(&reminder).await? {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "reminder_limit",
            format!("you already have {DVR_REMINDERS_PER_USER_MAX} reminders set"),
        ));
    }
    Ok((StatusCode::CREATED, Json(reminder)))
}

pub(crate) async fn delete_reminder(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if state.store.delete_dvr_reminder(user.id, &id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("no such reminder"))
    }
}

/// Acknowledged on one device, gone from every other. A reminder that keeps
/// reappearing on the tablet after being dismissed on the television is the
/// failure this exists to prevent.
pub(crate) async fn ack_reminder(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let changed = state
        .store
        .set_dvr_reminder_state(
            Some(user.id),
            &id,
            DvrReminderState::Fired,
            DvrReminderState::Acked,
            now_ms(),
        )
        .await?;
    if changed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("no such reminder is waiting"))
    }
}

// ---- helpers shared with the engine ---------------------------------------

/// Whether a rule matches a guide programme on a channel.
///
/// Not called from a route: the scheduler in the owner loop is its only
/// caller, and it lands with the engine.
#[allow(dead_code)]
///
/// Lives here rather than in the engine because the rule editor and the
/// scheduler have to agree about it exactly: a preview that disagreed with
/// what actually gets recorded would be worse than no preview.
pub(crate) fn rule_matches(rule: &DvrRule, channel_id: &str, programme: &LiveTvProgramme) -> bool {
    if !rule.enabled {
        return false;
    }
    let matched = match rule.match_mode {
        DvrMatchMode::SeriesId => programme.series_id.as_deref() == Some(rule.match_value.as_str()),
        DvrMatchMode::Title => {
            normalise_title(&programme.title) == rule.match_value
                && rule
                    .channel_id
                    .as_deref()
                    .is_none_or(|wanted| wanted == channel_id)
        }
    };
    if !matched {
        return false;
    }
    if rule.new_only {
        return is_first_run(
            programme.is_new,
            programme.original_air_date.as_deref(),
            programme.start,
        );
    }
    true
}

/// Every settings key this feature owns, in one place, so a future audit of
/// what the DVR reads does not have to grep for `dvr.` across the tree.
#[allow(dead_code)]
pub(crate) fn dvr_setting_keys() -> [&'static str; 8] {
    [
        keys::DVR_ENABLED,
        keys::DVR_ROOT,
        keys::DVR_FREE_FLOOR_GB,
        keys::DVR_TUNER_RESERVE,
        keys::DVR_PAD_START_S,
        keys::DVR_PAD_END_S,
        keys::DVR_REMINDER_LEAD_S,
        keys::DVR_WEBHOOK_URL,
    ]
}

#[cfg(test)]
mod visibility_tests {
    use super::*;

    fn row(state: DvrState) -> DvrRecording {
        let mut row = recording_from_programme(
            &ResolvedChannel {
                id: "7.1".into(),
                guide_number: "7.1".into(),
                name: "WABC".into(),
            },
            &LiveTvProgramme {
                start: 1_789_000_800,
                end: 1_789_002_600,
                title: "Kitchen Table".into(),
                episode_title: None,
                episode: None,
                synopsis: None,
                image_url: None,
                original_air_date: None,
                series_id: None,
                programme_id: None,
                is_new: None,
                filters: Vec::new(),
            },
            DvrOrigin::Manual,
            None,
            Some(1),
            60,
            120,
            0,
        );
        row.state = state;
        row.attempt = 1;
        row
    }

    fn observation(last_write_age_ms: Option<u64>) -> crate::live_tv::dvr::DvrCaptureObservation {
        crate::live_tv::dvr::DvrCaptureObservation {
            recording_id: "recording".into(),
            channel_id: "7.1".into(),
            airing_start: 1_789_000_800,
            owner_node_id: "node-a".into(),
            config_generation: 4,
            serving_generation: 2,
            attempt: 1,
            phase: "writing".into(),
            observation_age_ms: 0,
            last_write_age_ms,
            first_write_at_ms: Some(1_789_000_000_000),
            attempt_bytes_written: 4_096,
            prior_attempt_bytes: Some(0),
            write_bps: Some(2_048),
            reason_code: None,
        }
    }

    #[test]
    fn display_state_separates_freshness_from_write_health() {
        let recording = row(DvrState::Recording);
        let healthy = observation(Some(2_000));
        assert_eq!(display_state(&recording, Some(&healthy), 0).0, "Recording");
        assert!(display_state(&recording, Some(&healthy), 0)
            .1
            .contains("being written"));

        let quiet = observation(Some(15_000));
        assert_eq!(display_state(&recording, Some(&quiet), 0).0, "Recording");
        assert!(display_state(&recording, Some(&quiet), 0)
            .1
            .contains("No recent data"));

        let mut stale = healthy;
        stale.observation_age_ms = DVR_OBSERVATION_FRESH_MS + 1;
        assert_eq!(
            display_state(&recording, Some(&stale), 0).0,
            "Status unavailable"
        );
    }

    #[test]
    fn a_reached_clock_never_claims_that_capture_started() {
        let scheduled = row(DvrState::Scheduled);
        let now = scheduled.capture_start.saturating_mul(1_000);
        assert_eq!(display_state(&scheduled, None, now).0, "Waiting to start");
    }

    #[test]
    fn attention_cursor_round_trips_section_and_both_watermarks() {
        let cursor = AttentionCursor {
            version: 1,
            section: AttentionCursorSection::Historical,
            after_at_ms: 1_789_000_000_123,
            after_id: "recording-7".into(),
            current_upper: Some((1_789_000_001_000, "recording-2".into())),
            historical_upper: Some((1_789_000_000_900, "recording-9".into())),
        };
        let encoded = encode_attention_cursor(&cursor).expect("cursor encodes");
        assert!(!encoded.contains("recording"));
        assert_eq!(
            parse_attention_cursor(Some(&encoded)).expect("cursor parses"),
            Some(cursor)
        );
        assert!(parse_attention_cursor(Some("not-hex")).is_err());
    }
}
