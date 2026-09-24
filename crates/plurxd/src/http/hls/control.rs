use super::*;

/// GET /api/v1/files/:id/hls/start — **deprecated**; use `POST …/hls/sessions`.
///
/// Kept as a bridge for clients that predate the POST route, and implemented
/// over the same creation path so it cannot bypass identity or cleanup. Its
/// one concession: with no `playback_id` to key supersession by, it
/// synthesises the old (viewer, file) key, so its behaviour is exactly what it
/// was — including the two-devices-one-account collision the new route fixes.
pub async fn start(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    AxPath(id): AxPath<i64>,
    Query(q): Query<StartQuery>,
    headers: HeaderMap,
    super::super::network::RemoteAddress(remote): super::super::network::RemoteAddress,
) -> Result<Json<StartResponse>, ApiError> {
    let legacy = CreateSession {
        intent: None,
        // No control exchange precedes a legacy start, so there is no
        // ordering for it to be stale against.
        control_sequence: None,
        playback_id: format!("legacy:{}:{id}", user.username),
        request_id: None,
        previous_session_id: None,
        reopen_reason: None,
        height: q.height,
        // The GET bridge has no quality-intent parameter, so it keeps the
        // wire-presence inference it has always had.
        quality_auto: None,
        subtitle_burn: None, // the deprecated GET bridge never offered a burn
        subtitle_burn_sdr: None,
        native_subtitles: None,
        subtitle: None,
        start: q.start,
        audio: q.audio,
        copy: Some(q.copy == Some(1)),
        aac: Some(q.aac == Some(1)),
        preserve_dolby_vision: Some(false),
        // The deprecated GET bridge has no capability parameters at all, so
        // it keeps the SDR ladder it has always had.
        hdr10: None,
        // …and no capabilities document to re-derive from, so it lands on the
        // trust path with every other client that predates caps v2.
        caps: None,
        overrides: None,
        audio_offset_ms: None,
        presentation: None,
        block_budget_secs: None,
        transport: None,
    };
    create(
        AuthUser(user),
        State(state),
        AxPath(id),
        headers,
        super::super::network::RemoteAddress(remote),
        Json(legacy),
    )
    .await
}

/// GET /api/v1/hls/:session/status — how the session is actually doing.
///
/// Capability auth like its siblings (the session id is the credential), and
/// deliberately outside the `{segment}` route: a static path segment wins over
/// a parameter in the router, and `status` is not a valid segment name anyway.
///
/// This exists because the first question every buffering report asks —
/// *is the server keeping up?* — had no answer anywhere in the product. The
/// player puts `speed` and `ahead_seconds` in the stats overlay, so "why did
/// it stutter" resolves to a number instead of a theory.
pub async fn status(
    State(state): State<AppState>,
    AxPath(session): AxPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let request_deadline = response_publication_deadline();
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::Status,
        RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await?
    {
        return Ok(response);
    }
    status_local_before(&state, &session, request_deadline).await
}

/// POST /api/v1/hls/:session/control — one bounded, capability-authenticated
/// playback-control exchange. The session UUID remains the bearer capability;
/// generation, epoch, client instance and sequence are mutation fences.
pub async fn control(
    State(state): State<AppState>,
    AxPath(session): AxPath<String>,
    body: Bytes,
) -> Response {
    let deadline_unix_ms = crate::media_sessions::unix_ms().saturating_add(
        i64::try_from(crate::playback_control::EXCHANGE_DEADLINE.as_millis()).unwrap_or(i64::MAX),
    );
    match tokio::time::timeout(
        crate::playback_control::EXCHANGE_DEADLINE,
        control_inner(state, session, body, deadline_unix_ms),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the playback control exchange exceeded its deadline",
                None,
                None,
                Some(500),
                None,
            )
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
pub(super) struct RetainedTerminalResponse {
    pub(super) platform: crate::playback_control::ClientPlatform,
    pub(super) response: crate::playback_control::ControlResponseV1,
}

/// Resolve the selected native text track's cache state for the control
/// response.
///
/// Gated on `Native` before anything is read: `Off`, `Overlay` and `Burn` name
/// no sidecar, so the common case costs one enum comparison and no store hit.
/// Nothing here starts an extraction — a readiness probe that did would make
/// every control exchange a reason to spawn ffmpeg.
async fn subtitle_track_cache(
    state: &AppState,
    recipe: &RemoteStartRequest,
    selection: &crate::playback_control::SubtitleSelection,
    request: &crate::playback_control::ControlRequestV1,
    window_seconds: i64,
) -> Option<crate::playback_control::SubtitleTrackCache> {
    use crate::playback_control::{SubtitleMode, SubtitleTrackCache};

    if !matches!(selection.mode, SubtitleMode::Native) {
        return None;
    }
    let index = selection.track?;
    let file = state.store.get_file(recipe.request.file_id).await.ok()??;
    // A track that is not there, or is there as a bitmap, will never produce a
    // sidecar however long the client waits. Saying `unavailable` is the whole
    // point of distinguishing it from `warming`.
    let Some(track) = usize::try_from(index)
        .ok()
        .and_then(|i| file.subtitle_streams.get(i))
    else {
        return Some(SubtitleTrackCache::Unavailable);
    };
    if !plurx_core::tracks::is_native_text_subtitle(&track.codec) {
        return Some(SubtitleTrackCache::Unavailable);
    }
    // Control positions are already absolute film time. The segment path gets
    // the same value by adding its item-local segment start to
    // `media_origin_seconds`; snapping both through the shared grid is what
    // prevents readiness from reporting on a window the segment will not use.
    let demand_seconds = request.seek_target_ms.unwrap_or(request.position_ms).max(0) / 1_000;
    Some(
        match crate::subtitles::sidecar_state_for_demand(
            &state.subs_dir,
            &file,
            index,
            demand_seconds,
            window_seconds,
        )
        .await
        {
            crate::subtitles::SidecarState::Ready => SubtitleTrackCache::Ready,
            crate::subtitles::SidecarState::Failed => SubtitleTrackCache::Unavailable,
            crate::subtitles::SidecarState::Warming | crate::subtitles::SidecarState::Absent => {
                SubtitleTrackCache::Warming
            }
        },
    )
}

fn local_control_response(
    route: &MediaSessionRoute,
    start: &StartResponse,
    recipe: &RemoteStartRequest,
    request: &crate::playback_control::ControlRequestV1,
    result: &crate::playback_control::LocalControlResult,
    server_time_unix_ms: i64,
    subtitle_readiness: Option<String>,
) -> crate::playback_control::ControlResponseV1 {
    let owner_epoch = u64::try_from(route.owner_epoch).unwrap_or_default();
    let response = crate::playback_control::ControlResponseV1 {
        protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
        generation: route.incarnation_id.clone(),
        control_epoch: owner_epoch,
        accepted_sequence: result.accepted_sequence,
        server_time_unix_ms,
        lease: crate::playback_control::PlaybackLeaseView {
            state: result.lease_state.to_owned(),
            renew_after_ms: crate::playback_control::NEXT_EXCHANGE_MS,
            expires_at_unix_ms: if result.lease_state == "ended" {
                server_time_unix_ms
            } else {
                result.lease_expires_at_unix_ms
            },
        },
        delivery: crate::playback_control::DeliveryView::from_status(
            &result.status,
            request,
            &route.owner_node_id,
            owner_epoch,
            route.media_origin_ms,
            subtitle_readiness,
        ),
        effective_selection: crate::playback_control::EffectiveSelection::from_recipe(
            recipe,
            start.height,
            start.delivered_dynamic_range.clone(),
        ),
        action: crate::playback_control::ControlAction::None,
    };
    // The advisory hold is derived from the delivery this response is already
    // carrying, so the two can never disagree. Building the response first and
    // resolving after is what makes that guarantee structural rather than a
    // rule two call sites have to remember.
    let action =
        crate::playback_control::resolve_action(&result.action, &response.delivery, request);
    crate::playback_control::record_action(
        &action,
        &response.delivery,
        request,
        result.platform,
        result.action_suppressed,
    );
    crate::playback_control::ControlResponseV1 { action, ..response }
}

const PREPARATION_SETTLEMENT_CAPACITY: usize = 32;
pub(super) const PREPARATION_SETTLEMENT_RETRY_BUDGET: Duration = Duration::from_secs(5);
pub(super) const PREPARATION_SETTLEMENT_RETRY_MIN: Duration = Duration::from_millis(25);
pub(super) const PREPARATION_SETTLEMENT_RETRY_MAX: Duration = Duration::from_millis(500);

#[derive(Clone)]
pub(super) enum PreparationSettlement {
    Committed(Box<crate::playback_control::ControlResponseV1>),
    Aborted,
    Rejected,
    Unavailable,
}

#[derive(Clone)]
pub(super) struct PreparationSettlementReceipt {
    result: tokio::sync::watch::Receiver<Option<PreparationSettlement>>,
}

impl PreparationSettlementReceipt {
    pub(super) async fn wait_before(&self, deadline_unix_ms: i64) -> Option<PreparationSettlement> {
        let remaining = crate::playback_control::inherited_exchange_budget(
            deadline_unix_ms,
            crate::media_sessions::unix_ms(),
        )?;
        let mut result = self.result.clone();
        let wait = async move {
            loop {
                if let Some(outcome) = result.borrow().clone() {
                    return Some(outcome);
                }
                if result.changed().await.is_err() {
                    return None;
                }
            }
        };
        tokio::time::timeout(remaining, wait).await.ok().flatten()
    }
}

#[derive(Clone)]
pub(super) struct ActivePreparationSettlement {
    request_fingerprint: String,
    receipt: PreparationSettlementReceipt,
    /// `None` while the owner is running; completed canonical responses stay
    /// joinable through the same durable replay window as their Store row.
    replay_until: Option<std::time::Instant>,
}

pub(super) fn preparation_settlements(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, ActivePreparationSettlement>> {
    static OPERATIONS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, ActivePreparationSettlement>>,
    > = std::sync::OnceLock::new();
    OPERATIONS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

pub(super) fn preparation_settlement_slots() -> Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    Arc::clone(
        SLOTS
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(PREPARATION_SETTLEMENT_CAPACITY))),
    )
}

#[cfg(test)]
fn preparation_settlement_faults(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, usize>> {
    static FAULTS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, usize>>> =
        std::sync::OnceLock::new();
    FAULTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
pub(super) fn fail_next_preparation_settlements(incarnation_id: &str, count: usize) {
    preparation_settlement_faults()
        .lock()
        .expect("preparation settlement faults")
        .insert(incarnation_id.to_owned(), count);
}

#[cfg(test)]
fn consume_preparation_settlement_fault(incarnation_id: &str) -> bool {
    let mut faults = preparation_settlement_faults()
        .lock()
        .expect("preparation settlement faults");
    let Some(remaining) = faults.get_mut(incarnation_id) else {
        return false;
    };
    if *remaining == 0 {
        faults.remove(incarnation_id);
        return false;
    }
    *remaining -= 1;
    true
}

pub(super) struct DurablePreparationSettlementAdmission {
    pub(super) state: AppState,
    pub(super) gate: Arc<dyn crate::playback_control::PreparationGate>,
    pub(super) route: MediaSessionRoute,
    pub(super) start: StartResponse,
    pub(super) recipe: RemoteStartRequest,
    pub(super) request: crate::playback_control::ControlRequestV1,
    pub(super) status: Option<crate::transcode::HlsSessionInfo>,
    pub(super) reservation: std::sync::Mutex<Option<PreparationSettlementReservation>>,
    pub(super) receipt: std::sync::Mutex<Option<PreparationSettlementReceipt>>,
}

pub(super) struct PreparationSettlementReservation {
    _permit: tokio::sync::OwnedSemaphorePermit,
    _serving_commit: tokio::sync::OwnedRwLockReadGuard<()>,
}

pub(super) async fn reserve_preparation_settlement(
    state: &AppState,
    deadline_unix_ms: i64,
    slots: Arc<tokio::sync::Semaphore>,
) -> Option<PreparationSettlementReservation> {
    let remaining = crate::playback_control::inherited_exchange_budget(
        deadline_unix_ms,
        crate::media_sessions::unix_ms(),
    )?;
    let deadline = tokio::time::Instant::now() + remaining.min(PREPARATION_SETTLEMENT_RETRY_BUDGET);
    let permit = tokio::time::timeout_at(deadline, slots.acquire_owned())
        .await
        .ok()?
        .ok()?;
    let authority = state.serving.authority();
    let serving_generation = authority.admit()?;
    let serving_commit = authority
        .commit_guard_before(serving_generation, deadline.into_std())
        .await?;
    Some(PreparationSettlementReservation {
        _permit: permit,
        _serving_commit: serving_commit,
    })
}

pub(super) fn preparation_settlement_key(
    incarnation_id: &str,
    owner_epoch: i64,
    client_instance_id: &str,
    sequence: u64,
    staged_incarnation_id: &str,
) -> String {
    format!(
        "{incarnation_id}:{owner_epoch}:{client_instance_id}:{sequence}:{staged_incarnation_id}"
    )
}

impl DurablePreparationSettlementAdmission {
    pub(super) fn receipt(&self) -> Option<PreparationSettlementReceipt> {
        self.receipt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl crate::playback_control::PreparationSettlementAdmission
    for DurablePreparationSettlementAdmission
{
    fn accepted(&self, outcome: crate::playback_control::PreparationControlOutcome) {
        let staged_incarnation_id = match &outcome.preparation_directive {
            crate::playback_control::PreparationDirective::Commit {
                staged_incarnation_id,
            }
            | crate::playback_control::PreparationDirective::Abort {
                staged_incarnation_id,
                ..
            } => staged_incarnation_id.clone(),
        };
        let Some(request_fingerprint) = self.request.fingerprint() else {
            return;
        };
        let key = preparation_settlement_key(
            &self.route.incarnation_id,
            self.route.owner_epoch,
            &self.request.client_instance_id,
            self.request.sequence,
            &staged_incarnation_id,
        );
        let mut operations = preparation_settlements()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = std::time::Instant::now();
        operations.retain(|_, operation| {
            operation
                .replay_until
                .is_none_or(|replay_until| replay_until > now)
        });
        if let Some(active) = operations.get(&key) {
            if active.request_fingerprint == request_fingerprint {
                *self
                    .receipt
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(active.receipt.clone());
            }
            return;
        }
        let Some(reservation) = self
            .reservation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            // Every request shape which can emit a directive reserves first.
            // Fail closed if a future actor path violates that invariant;
            // absence of a reservation is never evidence of durable cleanup.
            let (_sender, result) =
                tokio::sync::watch::channel(Some(PreparationSettlement::Unavailable));
            *self
                .receipt
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(PreparationSettlementReceipt { result });
            return;
        };
        let (sender, receiver) = tokio::sync::watch::channel(None);
        let receipt = PreparationSettlementReceipt { result: receiver };
        operations.insert(
            key.clone(),
            ActivePreparationSettlement {
                request_fingerprint,
                receipt: receipt.clone(),
                replay_until: None,
            },
        );
        *self
            .receipt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(receipt);
        drop(operations);

        let state = self.state.clone();
        let gate = Arc::clone(&self.gate);
        let route = self.route.clone();
        let start = self.start.clone();
        let recipe = self.recipe.clone();
        let request = self.request.clone();
        let status = self.status.clone();
        tokio::spawn(async move {
            let result = settle_preparation_control(
                &state,
                gate,
                &route,
                &start,
                &recipe,
                &request,
                status,
                reservation,
                outcome,
            )
            .await;
            let retain = !matches!(result, PreparationSettlement::Unavailable);
            sender.send_replace(Some(result));
            let mut operations = preparation_settlements()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if retain {
                if let Some(operation) = operations.get_mut(&key) {
                    operation.replay_until = Some(
                        std::time::Instant::now()
                            + Duration::from_millis(
                                crate::playback_control::TERMINAL_ACK_REPLAY_TTL_MS as u64,
                            ),
                    );
                }
            } else {
                operations.remove(&key);
            }
        });
    }
}

#[allow(clippy::too_many_arguments)]
async fn settle_preparation_control(
    state: &AppState,
    gate: Arc<dyn crate::playback_control::PreparationGate>,
    route: &MediaSessionRoute,
    start: &StartResponse,
    recipe: &RemoteStartRequest,
    request: &crate::playback_control::ControlRequestV1,
    status: Option<crate::transcode::HlsSessionInfo>,
    _reservation: PreparationSettlementReservation,
    outcome: crate::playback_control::PreparationControlOutcome,
) -> PreparationSettlement {
    let executor = crate::playback_control::PreparationExecutor::new(
        Arc::clone(&state.store),
        gate,
        route.user_id,
        route.playback_id.clone(),
        route.owner_node_id.clone(),
        route.owner_epoch,
    );
    let now = tokio::time::Instant::now();
    let deadline = now + PREPARATION_SETTLEMENT_RETRY_BUDGET;
    #[cfg(test)]
    let settlement_delay = {
        preparation_settlement_delays()
            .lock()
            .expect("preparation settlement delays")
            .remove(&route.incarnation_id)
    };
    #[cfg(test)]
    if let Some(delay) = settlement_delay {
        tokio::time::sleep(delay).await;
    }
    let staged_incarnation_id = match &outcome.preparation_directive {
        crate::playback_control::PreparationDirective::Commit {
            staged_incarnation_id,
        }
        | crate::playback_control::PreparationDirective::Abort {
            staged_incarnation_id,
            ..
        } => staged_incarnation_id.as_str(),
    };
    // Take the one process-local owner before the deadline/disable/wait paths
    // can do so. From here this detached settlement task owns the exact row,
    // actor slot, and worker until it commits or explicitly aborts them.
    let active_preparation = take_active_preparation(staged_incarnation_id);
    let acknowledgement_rejected = matches!(
        &outcome.preparation_directive,
        crate::playback_control::PreparationDirective::Abort {
            acknowledgement_rejected: true,
            ..
        }
    );
    let commit_acknowledgement = if matches!(
        &outcome.preparation_directive,
        crate::playback_control::PreparationDirective::Commit { .. }
    ) {
        status.and_then(|status| {
            let response_time = unix_ms();
            let result = crate::playback_control::LocalControlResult {
                disposition: outcome.disposition,
                accepted_sequence: outcome.accepted_sequence,
                action: outcome.action.clone(),
                action_suppressed: outcome.action_suppressed,
                preparation_directive: Some(outcome.preparation_directive.clone()),
                lease_expires_at_unix_ms: outcome.lease_expires_at_unix_ms,
                lease_timeout_ms: outcome.lease_timeout_ms,
                lease_state: outcome.lease_state,
                status,
                platform: outcome.platform,
                terminal_handoff: None,
                terminal_commit: None,
                selection: outcome.selection.clone(),
            };
            let mut response =
                local_control_response(route, start, recipe, request, &result, response_time, None);
            // Answered in the receipt, not stamped on top of it afterwards. A
            // settled answer announces no successor — it carries no `Prepare`
            // action and its ask is consumed or aborted — and saying so here is
            // what makes the first answer and its own replay the same bytes.
            response.delivery.preparation = Some("none".to_owned());
            match (
                i64::try_from(request.sequence),
                request.fingerprint(),
                serde_json::to_string(&RetainedTerminalResponse {
                    platform: outcome.platform,
                    response,
                }),
            ) {
                (Ok(sequence), Some(request_fingerprint), Ok(response_json)) => {
                    Some(MediaSessionTerminalAck {
                        incarnation_id: route.incarnation_id.clone(),
                        session_id: route.session_id.clone(),
                        owner_node_id: route.owner_node_id.clone(),
                        owner_epoch: route.owner_epoch,
                        client_instance_id: request.client_instance_id.clone(),
                        sequence,
                        request_fingerprint,
                        response_json,
                        expires_at_ms: response_time
                            .saturating_add(crate::playback_control::TERMINAL_ACK_REPLAY_TTL_MS),
                        updated_at_ms: response_time,
                    })
                }
                _ => None,
            }
        })
    } else {
        None
    };
    let mut delay = PREPARATION_SETTLEMENT_RETRY_MIN;
    loop {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let settled: Result<PreparationSettlement, StoreError> = match &outcome
            .preparation_directive
        {
            crate::playback_control::PreparationDirective::Commit { .. } => {
                let planned_commit_guard =
                    match active_preparation.as_ref().map(|active| active.purpose) {
                        Some(PreparationPurpose::PlannedRelocation(fence)) => {
                            let Ok(guard) = state.serving.try_planned_outage_operation_guard()
                            else {
                                let wake = (tokio::time::Instant::now() + delay).min(deadline);
                                tokio::time::sleep_until(wake).await;
                                delay = delay
                                    .saturating_mul(2)
                                    .min(PREPARATION_SETTLEMENT_RETRY_MAX);
                                continue;
                            };
                            if !state.serving.planned_outage_is_current(fence).await {
                                if let Some(active) = active_preparation.clone() {
                                    settle_cancelled_preparation(
                                        active,
                                        "planned relocation expired before commit",
                                    )
                                    .await;
                                } else if executor
                                    .reject_commit(staged_incarnation_id, unix_ms())
                                    .await
                                    .is_ok_and(|rejected| rejected)
                                {
                                    retire_prepared_incarnation(
                                        state,
                                        staged_incarnation_id,
                                        "planned relocation expired before commit",
                                    )
                                    .await;
                                }
                                return PreparationSettlement::Rejected;
                            }
                            Some(guard)
                        }
                        _ => None,
                    };
                if active_preparation.is_none()
                    && !planned_preparation_still_current(state, staged_incarnation_id).await
                {
                    match executor
                        .reject_commit(staged_incarnation_id, unix_ms())
                        .await
                    {
                        Ok(rejected) => {
                            if rejected {
                                retire_prepared_incarnation(
                                    state,
                                    staged_incarnation_id,
                                    "planned relocation expired before commit",
                                )
                                .await;
                            }
                            return PreparationSettlement::Rejected;
                        }
                        Err(_) => return PreparationSettlement::Unavailable,
                    }
                }
                #[cfg(test)]
                if consume_preparation_settlement_fault(&route.incarnation_id) {
                    let wake = (tokio::time::Instant::now() + delay).min(deadline);
                    tokio::time::sleep_until(wake).await;
                    delay = delay
                        .saturating_mul(2)
                        .min(PREPARATION_SETTLEMENT_RETRY_MAX);
                    continue;
                }
                if let Some(acknowledgement) = commit_acknowledgement.clone() {
                    let committed = executor
                        .commit(
                            staged_incarnation_id,
                            unix_ms(),
                            outcome.lease_expires_at_unix_ms,
                            Some(acknowledgement),
                        )
                        .await;
                    // A cancellation/exit request needs this same gate, so it
                    // cannot clear the exact fence between the predicate above
                    // and the durable pointer CAS.
                    drop(planned_commit_guard);
                    match committed {
                        Ok(crate::playback_control::PreparationCommitOutcome::Committed(
                            commit,
                        ))
                        | Ok(crate::playback_control::PreparationCommitOutcome::Replayed(commit)) =>
                        {
                            settle_committed_preparation(state, &commit).await;
                            Ok(commit
                                .control_receipt
                                .filter(|receipt| {
                                    commit_acknowledgement.as_ref().is_some_and(|expected| {
                                        terminal_ack_matches(receipt, expected)
                                    })
                                })
                                .and_then(|receipt| {
                                    serde_json::from_str::<RetainedTerminalResponse>(
                                        &receipt.response_json,
                                    )
                                    .ok()
                                })
                                .map(|retained| {
                                    PreparationSettlement::Committed(Box::new(retained.response))
                                })
                                .unwrap_or(PreparationSettlement::Unavailable))
                        }
                        Ok(crate::playback_control::PreparationCommitOutcome::Refused) => {
                            if let Some(active) = active_preparation.as_ref() {
                                retire_prepared_worker(
                                    state,
                                    &active.preparation.owner_node_id,
                                    staged_incarnation_id,
                                    &active.preparation.session_id,
                                    "prepared successor commit refused",
                                )
                                .await;
                            } else {
                                retire_prepared_incarnation(
                                    state,
                                    staged_incarnation_id,
                                    "prepared successor commit refused",
                                )
                                .await;
                            }
                            Ok(PreparationSettlement::Rejected)
                        }
                        Err(error) => Err(error),
                    }
                } else {
                    // A status snapshot is needed only to construct the exact
                    // successful response. If it disappeared after actor
                    // acceptance, convert the reserved commit to an abort and
                    // durably clean it up instead of leaving Committing stuck.
                    match executor
                        .reject_commit(staged_incarnation_id, unix_ms())
                        .await
                    {
                        Ok(rejected) => {
                            if rejected {
                                retire_prepared_incarnation(
                                    state,
                                    staged_incarnation_id,
                                    "prepared successor commit response unavailable",
                                )
                                .await;
                            }
                            Ok(PreparationSettlement::Unavailable)
                        }
                        Err(error) => Err(error),
                    }
                }
            }
            crate::playback_control::PreparationDirective::Abort { .. } => {
                match executor.abort(staged_incarnation_id, unix_ms()).await {
                    Ok(aborted) => {
                        if aborted {
                            if let Some(active) = active_preparation.as_ref() {
                                retire_prepared_worker(
                                    state,
                                    &active.preparation.owner_node_id,
                                    staged_incarnation_id,
                                    &active.preparation.session_id,
                                    "prepared successor aborted",
                                )
                                .await;
                            } else {
                                retire_prepared_incarnation(
                                    state,
                                    staged_incarnation_id,
                                    "prepared successor aborted",
                                )
                                .await;
                            }
                        }
                        Ok(if !aborted {
                            PreparationSettlement::Unavailable
                        } else if acknowledgement_rejected {
                            PreparationSettlement::Rejected
                        } else {
                            PreparationSettlement::Aborted
                        })
                    }
                    Err(error) => Err(error),
                }
            }
        };
        match settled {
            Ok(result) => return result,
            Err(_) => {
                let wake = (tokio::time::Instant::now() + delay).min(deadline);
                tokio::time::sleep_until(wake).await;
                delay = delay
                    .saturating_mul(2)
                    .min(PREPARATION_SETTLEMENT_RETRY_MAX);
            }
        }
    }
    if matches!(
        &outcome.preparation_directive,
        crate::playback_control::PreparationDirective::Commit { .. }
    ) && executor
        .reject_commit(staged_incarnation_id, unix_ms())
        .await
        .is_ok_and(|rejected| rejected)
    {
        retire_prepared_incarnation(
            state,
            staged_incarnation_id,
            "prepared successor commit settlement expired",
        )
        .await;
    }
    if let Some(active) = active_preparation {
        settle_cancelled_preparation(active, "prepared successor settlement expired").await;
    }
    PreparationSettlement::Unavailable
}

async fn planned_preparation_still_current(state: &AppState, incarnation_id: &str) -> bool {
    let Ok(Some(route)) = state
        .store
        .media_session_route_by_incarnation(incarnation_id)
        .await
    else {
        return false;
    };
    let Ok(response) = serde_json::from_str::<serde_json::Value>(&route.response_json) else {
        return false;
    };
    let reason = response
        .get(PREPARATION_REASON_RESPONSE_FIELD)
        .and_then(serde_json::Value::as_str);
    let Some(reason) = reason else {
        return true;
    };
    state
        .serving
        .current_planned_outage()
        .await
        .is_some_and(|fence| reason == format!("planned_relocation:{}", fence.identity()))
}

async fn retire_prepared_incarnation(
    state: &AppState,
    staged_incarnation_id: &str,
    reason: &'static str,
) {
    let route = tokio::time::timeout(
        PREPARATION_STORE_BUDGET,
        state
            .store
            .media_session_route_by_incarnation(staged_incarnation_id),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .flatten()
    .filter(|route| route.state == "ended" && route.terminal_reason.as_deref() == Some("replaced"));
    if let Some(route) = route {
        retire_prepared_worker(
            state,
            &route.owner_node_id,
            &route.incarnation_id,
            &route.session_id,
            reason,
        )
        .await;
    }
}

/// Project the exact predecessor once its intentional drain retires it, then
/// clear a committed successor's control publication fence. If the fast proof
/// cannot be obtained, arm the existing full response-lifetime safety
/// boundary; the lease reconciler also resumes that durable state after a
/// crash.
async fn settle_committed_preparation(
    state: &AppState,
    commit: &plurx_core::domain::MediaSessionPreparationCommit,
) {
    let successor = commit.route.clone();
    state
        .transcode
        .promote_prepared_session(&successor.session_id)
        .await;
    state.media_sessions.cache_route(successor.clone()).await;
    if successor.owner_node_id == state.node_id {
        state.media_sessions.seed_owned_lease(&successor).await;
    }
    if successor.publication_ready_at_ms == 0 {
        return;
    }
    let authority = state.serving.authority();
    let Some(admitted_generation) = authority.admit() else {
        return;
    };
    let predecessor = commit
        .predecessor
        .as_ref()
        .map(|route| route.incarnation_id.clone());
    let state = state.clone();
    tokio::spawn(async move {
        publish_committed_preparation(
            state,
            successor,
            predecessor,
            authority,
            admitted_generation,
        )
        .await;
    });
}

/// Continue publication independently of the four-second control exchange.
/// The commit is already durable and replayable; making its response wait for
/// a draining predecessor would turn success into a false 503. Prepared media
/// remains authorized by its durable response marker and current pointer while
/// this task proves the control-plane publication boundary.
async fn publish_committed_preparation(
    state: AppState,
    successor: MediaSessionRoute,
    predecessor: Option<String>,
    authority: crate::serving_fence::ServingAuthority,
    admitted_generation: u64,
) {
    if let Some(predecessor_incarnation) = predecessor.as_ref() {
        let deadline = tokio::time::Instant::now() + PREDECESSOR_PROJECTION_FAST_WINDOW;
        if project_activation_predecessor_until(&state, predecessor_incarnation, deadline).await
            && matches!(
                complete_activation_handoff_until(
                    &state,
                    &successor,
                    MediaSessionProjectionCompletion::PredecessorAcknowledged,
                    &authority,
                    admitted_generation,
                    deadline,
                )
                .await,
                ActivationHandoffVerdict::Ready | ActivationHandoffVerdict::SuccessorGone
            )
        {
            return;
        }
    }

    let now_ms = unix_ms();
    let not_before_ms = now_ms.saturating_add(
        i64::try_from(TERMINAL_PROJECTION_SAFETY_WINDOW.as_millis()).unwrap_or(i64::MAX),
    );
    let arm_deadline = tokio::time::Instant::now() + ACTIVATION_STORE_DEADLINE;
    let Some(_guard) = authority
        .commit_guard_before(admitted_generation, arm_deadline.into_std())
        .await
    else {
        return;
    };
    let armed = tokio::time::timeout_at(
        arm_deadline,
        state.store.arm_media_session_handoff(
            &successor.incarnation_id,
            &successor.owner_node_id,
            successor.owner_epoch,
            not_before_ms,
            now_ms,
        ),
    )
    .await;
    let Ok(Ok(Some(armed))) = armed else {
        return;
    };
    drop(_guard);
    state.media_sessions.cache_route(armed.clone()).await;
    if let Some(predecessor_incarnation) = predecessor {
        let _ = settle_armed_activation_handoff(
            &state,
            predecessor_incarnation,
            armed,
            authority,
            admitted_generation,
        )
        .await;
    }
}

struct DurableTerminalCommitter {
    store: Arc<dyn plurx_core::store::Store>,
    route: MediaSessionRoute,
    start: StartResponse,
    recipe: RemoteStartRequest,
    request: crate::playback_control::ControlRequestV1,
    faults: Option<Arc<TerminalCommitFaults>>,
}

pub(super) const TERMINAL_COMMIT_RETRY_BUDGET: Duration = Duration::from_secs(5);
const TERMINAL_COMMIT_RETRY_MIN: Duration = Duration::from_millis(25);
const TERMINAL_COMMIT_RETRY_MAX: Duration = Duration::from_millis(500);

fn terminal_ack_matches(
    stored: &MediaSessionTerminalAck,
    acknowledgement: &MediaSessionTerminalAck,
) -> bool {
    stored.incarnation_id == acknowledgement.incarnation_id
        && stored.session_id == acknowledgement.session_id
        && stored.owner_node_id == acknowledgement.owner_node_id
        && stored.owner_epoch == acknowledgement.owner_epoch
        && stored.client_instance_id == acknowledgement.client_instance_id
        && stored.sequence == acknowledgement.sequence
        && stored.request_fingerprint == acknowledgement.request_fingerprint
        && stored.response_json == acknowledgement.response_json
}

pub(super) async fn terminal_io_before<T>(
    deadline: tokio::time::Instant,
    operation: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::time::timeout_at(deadline, operation).await.ok()
}

async fn terminal_ack_is_visible_before(
    store: &dyn plurx_core::store::Store,
    acknowledgement: &MediaSessionTerminalAck,
    deadline: tokio::time::Instant,
) -> bool {
    terminal_io_before(
        deadline,
        store.media_session_terminal_ack(&acknowledgement.session_id, unix_ms()),
    )
    .await
    .and_then(Result::ok)
    .flatten()
    .as_ref()
    .is_some_and(|stored| terminal_ack_matches(stored, acknowledgement))
}

#[derive(Default)]
pub(super) struct TerminalCommitFaults {
    pub(super) fail_before_commit: std::sync::atomic::AtomicUsize,
    pub(super) fail_after_commit: std::sync::atomic::AtomicUsize,
}

impl TerminalCommitFaults {
    fn consume(counter: &std::sync::atomic::AtomicUsize) -> bool {
        counter
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
    }
}

async fn persist_terminal_ack(
    store: Arc<dyn plurx_core::store::Store>,
    acknowledgement: MediaSessionTerminalAck,
) -> bool {
    persist_terminal_ack_with_faults(store, acknowledgement, None).await
}

pub(super) async fn persist_terminal_ack_with_faults(
    store: Arc<dyn plurx_core::store::Store>,
    acknowledgement: MediaSessionTerminalAck,
    faults: Option<&TerminalCommitFaults>,
) -> bool {
    let remaining_ms = acknowledgement.expires_at_ms.saturating_sub(unix_ms());
    let Some(remaining_ms) = u64::try_from(remaining_ms)
        .ok()
        .filter(|remaining| *remaining > 0)
    else {
        return false;
    };
    let now = tokio::time::Instant::now();
    let acknowledgement_deadline = now
        .checked_add(Duration::from_millis(remaining_ms))
        .unwrap_or(now);
    let deadline = (now + TERMINAL_COMMIT_RETRY_BUDGET).min(acknowledgement_deadline);
    let mut delay = TERMINAL_COMMIT_RETRY_MIN;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let fail_before =
            faults.is_some_and(|faults| TerminalCommitFaults::consume(&faults.fail_before_commit));
        let write = if fail_before {
            Some(Err(()))
        } else {
            let write = match terminal_io_before(
                deadline,
                store.record_media_session_terminal_ack(&acknowledgement),
            )
            .await
            {
                Some(Ok(persisted)) => Ok(persisted),
                Some(Err(_)) => Err(()),
                None => return false,
            };
            let fail_after = write == Ok(true)
                && faults
                    .is_some_and(|faults| TerminalCommitFaults::consume(&faults.fail_after_commit));
            Some(if fail_after { Err(()) } else { write })
        };
        match write {
            Some(Ok(true)) => return true,
            Some(Ok(false)) => {
                // `false` is normally a definitive route/identity conflict,
                // but first resolve an earlier unknown commit of these exact
                // immutable bytes.
                return terminal_ack_is_visible_before(store.as_ref(), &acknowledgement, deadline)
                    .await;
            }
            Some(Err(_))
                if terminal_ack_is_visible_before(store.as_ref(), &acknowledgement, deadline)
                    .await =>
            {
                // The write may have committed before its answer was lost.
                // Read-after-unknown prevents a second outcome from replacing
                // the accepted terminal response.
                return true;
            }
            Some(Err(_)) => {}
            None => unreachable!("write outcome is always classified"),
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let wake = now.checked_add(delay).unwrap_or(deadline).min(deadline);
        tokio::time::sleep_until(wake).await;
        delay = delay.saturating_mul(2).min(TERMINAL_COMMIT_RETRY_MAX);
    }
}

impl crate::playback_control::TerminalControlCommitter for DurableTerminalCommitter {
    fn start(
        &self,
        result: &crate::playback_control::LocalControlResult,
    ) -> crate::playback_control::TerminalCommitReceipt {
        let server_time_unix_ms = unix_ms();
        let mut response = local_control_response(
            &self.route,
            &self.start,
            &self.recipe,
            &self.request,
            result,
            server_time_unix_ms,
            // The session is ending. Subtitle readiness is a fact about a
            // stream that will keep serving segments, and this one will not;
            // absent is the honest answer, and this trait method cannot await
            // a probe anyway.
            None,
        );
        // Answered in the receipt, not stamped on top of it afterwards. A
        // settled answer announces no successor — it carries no `Prepare`
        // action and its ask is consumed or aborted — and saying so here is
        // what makes the first answer and its own replay the same bytes.
        response.delivery.preparation = Some("none".to_owned());
        // Persist the outcome before the commit machinery, because a terminal
        // is the thing most worth explaining after a restart and the atomics
        // that count it do not survive one. `durable_outcome` decides what
        // deserves a row; this only carries it to the store.
        //
        // Not awaited and not fatal: `emit` spawns, and a telemetry write that
        // fails must not turn a terminal the viewer's client is waiting on
        // into an error. Losing the row is worse than not having it only if
        // the alternative is losing the terminal.
        {
            let metrics = crate::playback_control::action_metrics(
                &response.action,
                &response.delivery,
                &self.request,
                result.action_suppressed,
            );
            if let Some(event) = crate::playback_control::durable_outcome(
                &response.action,
                &response.delivery,
                &self.request,
                &metrics,
                server_time_unix_ms,
            ) {
                crate::telemetry::emit(Arc::clone(&self.store), event);
            }
        }
        let response_json = serde_json::to_string(&RetainedTerminalResponse {
            platform: result.platform,
            response: response.clone(),
        });
        let identity = (
            i64::try_from(self.request.sequence),
            self.request.fingerprint(),
        );
        let handoff = result.terminal_handoff.clone();
        let (Ok(response_json), (Ok(sequence), Some(request_fingerprint))) =
            (response_json, identity)
        else {
            let (receipt, sender) = crate::playback_control::TerminalCommitReceipt::pending();
            if let Some(handoff) = handoff {
                handoff.complete();
            }
            let _ = sender.send(Some(Err(())));
            return receipt;
        };
        let acknowledgement = MediaSessionTerminalAck {
            incarnation_id: self.route.incarnation_id.clone(),
            session_id: self.route.session_id.clone(),
            owner_node_id: self.route.owner_node_id.clone(),
            owner_epoch: self.route.owner_epoch,
            client_instance_id: self.request.client_instance_id.clone(),
            sequence,
            request_fingerprint,
            response_json,
            expires_at_ms: server_time_unix_ms
                .saturating_add(crate::playback_control::TERMINAL_ACK_REPLAY_TTL_MS),
            updated_at_ms: server_time_unix_ms,
        };
        let store = Arc::clone(&self.store);
        let faults = self.faults.clone();
        // A terminal exchange on a *draining* predecessor is not receipted,
        // and that is the whole handling.
        //
        // The receipt table holds one row per `session_id`, and on a draining
        // predecessor that row is already the commit's: the prepared commit
        // runs on this route and retains its reply here. Until the drain, no
        // second receipted exchange could reach a predecessor, because the
        // commit ended it in the same transaction. Now the viewer closing the
        // player sends `demand: end` to a session that is still live, and a
        // write that loses to the commit receipt reads back as "not durably
        // committed" — a 503 the client retries twice a second for the rest
        // of the window.
        //
        // Ending the row is a better answer than a receipt anyway. The
        // retained reply exists so a lost terminal answer can be replayed;
        // once this row is `ended`, a client that missed the reply asks again
        // and gets `410 session_ended` from the row itself, which is the same
        // terminal by a shorter path. So the drain ends here, early, and the
        // commit keeps the receipt slot it needs for its own replay.
        //
        // `end_media_session` rather than the owner-scoped CAS: the lease
        // boundary on this route was read when the request arrived and the
        // owner's tick renews every three seconds, so an exact-boundary CAS
        // would lose a race it has no reason to enter. The pointer delete
        // inside it is guarded on this exact incarnation, and the pointer
        // names the successor, so ending the predecessor cannot take it.
        let draining = self.route.drain_deadline_ms.is_some();
        let session_id = self.route.session_id.clone();
        crate::playback_control::TerminalCommitReceipt::retryable_until(
            acknowledgement.expires_at_ms,
            move |attempt| {
                let store = Arc::clone(&store);
                let acknowledgement = acknowledgement.clone();
                let response = response.clone();
                let handoff = handoff.clone();
                let faults = faults.clone();
                let session_id = session_id.clone();
                if let Some(handoff) = &handoff {
                    handoff.restart();
                }
                tokio::spawn(async move {
                    let persisted = if draining {
                        matches!(
                            store
                                .end_media_session(&session_id, "superseded", unix_ms())
                                .await,
                            Ok(Some(_))
                        )
                    } else {
                        match faults {
                            Some(faults) => {
                                persist_terminal_ack_with_faults(
                                    store,
                                    acknowledgement,
                                    Some(faults.as_ref()),
                                )
                                .await
                            }
                            None => persist_terminal_ack(store, acknowledgement).await,
                        }
                    };
                    if let Some(handoff) = handoff {
                        handoff.complete();
                    }
                    attempt.complete(if persisted { Ok(response) } else { Err(()) });
                });
            },
        )
    }
}

pub(crate) struct TerminalAckReplay {
    response: crate::playback_control::ControlResponseV1,
    pub(super) platform: Option<crate::playback_control::ClientPlatform>,
}

pub(crate) async fn terminal_ack_replay(
    state: &AppState,
    route: &MediaSessionRoute,
    request: &crate::playback_control::ControlRequestV1,
    deadline_unix_ms: i64,
) -> Result<Option<TerminalAckReplay>, ()> {
    if request.demand != crate::playback_control::PlaybackDemand::End {
        return Ok(None);
    }
    let Some(acknowledgement) = state
        .store
        .media_session_terminal_ack(&route.session_id, unix_ms())
        .await
        .map_err(|_| ())?
    else {
        return Ok(None);
    };
    if acknowledgement.incarnation_id != route.incarnation_id
        || acknowledgement.owner_node_id != route.owner_node_id
        || acknowledgement.owner_epoch != route.owner_epoch
        || acknowledgement.client_instance_id != request.client_instance_id
        || u64::try_from(acknowledgement.sequence).ok() != Some(request.sequence)
        || request.fingerprint().as_deref() != Some(acknowledgement.request_fingerprint.as_str())
    {
        return Ok(None);
    }
    let relay = crate::playback_control::ControlRelayRequest {
        session_id: route.session_id.clone(),
        generation: route.incarnation_id.clone(),
        expected_owner_node_id: route.owner_node_id.clone(),
        expected_owner_epoch: route.owner_epoch,
        deadline_unix_ms,
        control: request.clone(),
    };
    let retained = serde_json::from_str::<RetainedTerminalResponse>(&acknowledgement.response_json)
        .map(|retained| TerminalAckReplay {
            response: retained.response,
            platform: Some(retained.platform),
        })
        // Compatibility with terminal acknowledgements written by an earlier M3
        // build. New writes always retain platform independently of a retry's
        // optional capabilities.
        .or_else(|_| {
            serde_json::from_str::<crate::playback_control::ControlResponseV1>(
                &acknowledgement.response_json,
            )
            .map(|response| TerminalAckReplay {
                response,
                platform: request.capabilities.as_ref().map(|caps| caps.platform),
            })
        })
        .map_err(|_| ())?;
    retained
        .response
        .is_valid_for(&relay)
        .then_some(retained)
        .map(Some)
        .ok_or(())
}

pub(crate) async fn preparation_ack_replay(
    state: &AppState,
    route: &MediaSessionRoute,
    request: &crate::playback_control::ControlRequestV1,
    deadline_unix_ms: i64,
) -> Result<Option<TerminalAckReplay>, ()> {
    // Gated on the receipt, not on the route's state.
    //
    // The state check was the same fact by a different name — the commit
    // retires the predecessor in its own transaction, so `ended`/`superseded`
    // and "this commit already happened" were one condition. §4 has to break
    // that pairing: a predecessor a client may still be displaying cannot be
    // retired at commit, and the moment it is not, a client whose commit
    // response was lost would be answered by the live control plane instead of
    // by the exact durable answer — the one case §4 says must always replay.
    //
    // The receipt is a stricter fence than the state was. Its two writers are
    // the commit transaction and `record_media_session_terminal_ack`, which
    // ends the session in the same transaction, and every field of the
    // exchange is compared below: incarnation, owner, epoch, client instance,
    // sequence and request fingerprint. Only the byte-identical request that
    // produced a receipt can replay it, so a live serving session cannot have
    // a real exchange short-circuited.
    if !matches!(
        request.acknowledgement.as_ref().map(|ack| ack.state),
        Some(crate::playback_control::AcknowledgementState::Committed)
    ) {
        return Ok(None);
    }
    let Some(receipt) = state
        .store
        .media_session_terminal_ack(&route.session_id, unix_ms())
        .await
        .map_err(|_| ())?
    else {
        return Ok(None);
    };
    if receipt.incarnation_id != route.incarnation_id
        || receipt.owner_node_id != route.owner_node_id
        || receipt.owner_epoch != route.owner_epoch
        || receipt.client_instance_id != request.client_instance_id
        || u64::try_from(receipt.sequence).ok() != Some(request.sequence)
        || request.fingerprint().as_deref() != Some(receipt.request_fingerprint.as_str())
    {
        return Ok(None);
    }
    let retained =
        serde_json::from_str::<RetainedTerminalResponse>(&receipt.response_json).map_err(|_| ())?;
    let relay = crate::playback_control::ControlRelayRequest {
        session_id: route.session_id.clone(),
        generation: route.incarnation_id.clone(),
        expected_owner_node_id: route.owner_node_id.clone(),
        expected_owner_epoch: route.owner_epoch,
        deadline_unix_ms,
        control: request.clone(),
    };
    retained
        .response
        .is_valid_for(&relay)
        .then_some(TerminalAckReplay {
            response: retained.response,
            platform: Some(retained.platform),
        })
        .map(Some)
        .ok_or(())
}

pub(crate) fn terminal_ack_response(replay: TerminalAckReplay) -> Response {
    crate::playback_control::record(crate::playback_control::MetricOutcome::Replay);
    if let Some(platform) = replay.platform {
        crate::playback_control::record_platform(
            crate::playback_control::MetricOutcome::Replay,
            platform,
        );
    }
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(replay.response),
    )
        .into_response()
}

pub(super) async fn control_inner(
    state: AppState,
    session: String,
    body: Bytes,
    deadline_unix_ms: i64,
) -> Response {
    if uuid::Uuid::parse_str(&session).is_err() {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
        return control_error(
            StatusCode::NOT_FOUND,
            "session_gone",
            "no active or durable media session has this capability",
            None,
            None,
            None,
            None,
        );
    }
    let request = match serde_json::from_slice::<crate::playback_control::ControlRequestV1>(&body) {
        Ok(request) => request,
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Invalid);
            return control_error(
                StatusCode::BAD_REQUEST,
                "invalid_control",
                "the control body is not valid protocol v1 JSON",
                None,
                None,
                None,
                None,
            );
        }
    };
    let route = match state.media_sessions.control_route(&session).await {
        Ok(Some(route)) => route,
        Ok(None) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
            return control_error(
                StatusCode::NOT_FOUND,
                "session_gone",
                "no active or durable media session has this capability",
                None,
                None,
                None,
                None,
            );
        }
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the durable media route is temporarily unavailable",
                None,
                None,
                Some(500),
                None,
            );
        }
    };
    if let Some(refusal) = library_channel_control_refusal(&state, &route).await {
        return refusal;
    }
    let owner_epoch = match u64::try_from(route.owner_epoch)
        .ok()
        .filter(|epoch| *epoch > 0)
    {
        Some(epoch) => epoch,
        None => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the durable media route has an invalid owner epoch",
                Some(route.incarnation_id),
                None,
                Some(500),
                None,
            );
        }
    };
    let start = match control_start_response(&route) {
        Some(start) => start,
        None => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
            return control_error(
                StatusCode::NOT_FOUND,
                "session_gone",
                "playback control was not advertised for this session",
                None,
                None,
                None,
                None,
            );
        }
    };
    let recipe = match serde_json::from_str::<RemoteStartRequest>(&route.recipe_json) {
        Ok(recipe) => recipe,
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the durable delivery recipe is unreadable",
                Some(route.incarnation_id),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
    };
    if let Err(field) = request.validate(
        start.duration_ms,
        crate::playback_control::target_duration_ms(&recipe),
    ) {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Invalid);
        return control_error(
            StatusCode::BAD_REQUEST,
            "invalid_control",
            "a control field is outside the bounded v1 contract",
            Some(route.incarnation_id),
            Some(owner_epoch),
            None,
            Some(field),
        );
    }
    if request.generation != route.incarnation_id {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Stale);
        return control_error(
            StatusCode::CONFLICT,
            "stale_control",
            "the control generation is no longer current",
            Some(route.incarnation_id),
            Some(owner_epoch),
            None,
            None,
        );
    }
    if request.control_epoch != owner_epoch {
        crate::playback_control::record(crate::playback_control::MetricOutcome::OwnerChanged);
        return control_error(
            StatusCode::CONFLICT,
            "owner_changed",
            "the media session owner epoch changed",
            Some(route.incarnation_id),
            Some(owner_epoch),
            None,
            None,
        );
    }
    match terminal_ack_replay(&state, &route, &request, deadline_unix_ms).await {
        Ok(Some(replay)) => return terminal_ack_response(replay),
        Ok(None) => {}
        Err(()) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the terminal control acknowledgement is temporarily unavailable",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
    }
    match preparation_ack_replay(&state, &route, &request, deadline_unix_ms).await {
        Ok(Some(replay)) => return terminal_ack_response(replay),
        Ok(None) => {}
        Err(()) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the preparation acknowledgement replay is temporarily unavailable",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
    }
    if let Err(retry_after_ms) = state.media_sessions.admit_control(&session) {
        crate::playback_control::record(crate::playback_control::MetricOutcome::RateLimited);
        return control_error(
            StatusCode::TOO_MANY_REQUESTS,
            "control_rate_limited",
            "the ingress control budget is exhausted",
            None,
            None,
            Some(retry_after_ms),
            None,
        );
    }
    if route.state != "active" {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
        return control_error(
            StatusCode::GONE,
            "session_ended",
            "this media session has ended or been superseded",
            Some(route.incarnation_id),
            Some(owner_epoch),
            None,
            None,
        );
    }
    if let Some(refusal) = control_owner_refusal(&route, Some(owner_epoch)) {
        return refusal;
    }
    if route.owner_node_id != state.node_id {
        let relay = crate::playback_control::ControlRelayRequest {
            session_id: session.clone(),
            generation: route.incarnation_id.clone(),
            expected_owner_node_id: route.owner_node_id.clone(),
            expected_owner_epoch: route.owner_epoch,
            deadline_unix_ms,
            control: request,
        };
        return match state
            .media_sessions
            .control(&route.owner_node_id, &relay)
            .await
        {
            Ok(response) => response,
            Err(_) => {
                crate::playback_control::record(
                    crate::playback_control::MetricOutcome::Unavailable,
                );
                control_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "control_unavailable",
                    "the owning media worker did not answer the control exchange",
                    Some(route.incarnation_id),
                    Some(owner_epoch),
                    Some(500),
                    None,
                )
            }
        };
    }
    control_local(&state, &route, request, deadline_unix_ms).await
}

/// Revalidate a durable following purpose at every normal control exchange.
/// The session UUID is still the bearer capability, but it cannot keep a
/// deleted, disabled, or newly-hidden channel alive. Ordinary VOD response
/// JSON has no `library_channel` member and pays only one object lookup.
async fn library_channel_control_refusal(
    state: &AppState,
    route: &MediaSessionRoute,
) -> Option<Response> {
    let response = match serde_json::from_str::<serde_json::Value>(&route.response_json) {
        Ok(response) => response,
        Err(_) => return None, // The ordinary response parser reports this below.
    };
    let raw_purpose = response.get("library_channel")?;
    let purpose = match serde_json::from_value::<
        crate::http::library_channels::LibraryChannelPlaybackPurpose,
    >(raw_purpose.clone())
    {
        Ok(purpose) => purpose,
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return Some(control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the durable Library-channel purpose is unreadable",
                Some(route.incarnation_id.clone()),
                u64::try_from(route.owner_epoch).ok(),
                Some(500),
                None,
            ));
        }
    };
    let runtime_enabled = match state
        .store
        .get_setting(plurx_core::store::keys::LIBRARY_CHANNELS_ENABLED)
        .await
    {
        Ok(value) => plurx_core::store::stored_switch(value.as_deref(), false),
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return Some(control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "Library-channel authority is temporarily unavailable",
                Some(route.incarnation_id.clone()),
                u64::try_from(route.owner_epoch).ok(),
                Some(500),
                None,
            ));
        }
    };
    if !runtime_enabled {
        return Some(control_error(
            StatusCode::GONE,
            "channel_unavailable",
            "Library-channel playback was disabled",
            Some(route.incarnation_id.clone()),
            u64::try_from(route.owner_epoch).ok(),
            None,
            None,
        ));
    }
    let user = match state.store.get_user(route.user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            return Some(control_error(
                StatusCode::GONE,
                "channel_unavailable",
                "the Library-channel viewer no longer exists",
                Some(route.incarnation_id.clone()),
                u64::try_from(route.owner_epoch).ok(),
                None,
                None,
            ));
        }
        Err(_) => {
            return Some(control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "Library-channel authorization is temporarily unavailable",
                Some(route.incarnation_id.clone()),
                u64::try_from(route.owner_epoch).ok(),
                Some(500),
                None,
            ));
        }
    };
    match state
        .store
        .get_library_channel(user.id, false, &purpose.channel_id)
        .await
    {
        Ok(Some(channel)) if channel.enabled => None,
        Ok(_) => Some(control_error(
            StatusCode::GONE,
            "channel_unavailable",
            "the Library channel was deleted, disabled, or is no longer visible",
            Some(route.incarnation_id.clone()),
            u64::try_from(route.owner_epoch).ok(),
            None,
            None,
        )),
        Err(_) => Some(control_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "control_unavailable",
            "Library-channel authorization is temporarily unavailable",
            Some(route.incarnation_id.clone()),
            u64::try_from(route.owner_epoch).ok(),
            Some(500),
            None,
        )),
    }
}

pub(super) fn control_start_response(route: &MediaSessionRoute) -> Option<StartResponse> {
    serde_json::from_str::<StartResponse>(&route.response_json)
        .ok()
        .filter(|response| response.control.is_some())
}

#[cfg(test)]
fn staged_read_faults() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static FAULTS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    FAULTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

#[cfg(test)]
pub(super) fn fail_next_staged_read(incarnation_id: &str) {
    staged_read_faults()
        .lock()
        .expect("staged read faults")
        .insert(incarnation_id.to_owned());
}

#[cfg(test)]
fn preparation_settlement_delays(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Duration>> {
    static DELAYS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Duration>>,
    > = std::sync::OnceLock::new();
    DELAYS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
pub(super) fn delay_next_preparation_settlement(incarnation_id: &str, delay: Duration) {
    preparation_settlement_delays()
        .lock()
        .expect("preparation settlement delays")
        .insert(incarnation_id.to_owned(), delay);
}

/// Test-only seam at a preparation candidate's planning step.
///
/// `plan_preparation_candidate` is the last thing a candidate does before it
/// can reach either the ledger or the registry, so the window in which a
/// client's exchange finds neither is the one this widens far enough to drive
/// real exchanges through. The refusal half stands in for a candidate that
/// turns out to be unplannable, without needing a source row contrived to make
/// the planner fail for some unrelated reason.
#[cfg(test)]
fn preparation_planning_faults(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, (Duration, bool)>> {
    static FAULTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, (Duration, bool)>>,
    > = std::sync::OnceLock::new();
    FAULTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
pub(super) fn fault_preparation_planning(playback_id: &str, delay: Duration, refuse: bool) {
    preparation_planning_faults()
        .lock()
        .expect("preparation planning faults")
        .insert(playback_id.to_owned(), (delay, refuse));
}

#[cfg(test)]
pub(super) fn take_preparation_planning_fault(playback_id: &str) -> Option<(Duration, bool)> {
    preparation_planning_faults()
        .lock()
        .expect("preparation planning faults")
        .remove(playback_id)
}

/// Test-only seam immediately before `register_active_preparation`.
///
/// Production has no suspension point between a staging task's last await and
/// that registration, which is exactly why the supersession flag is read once
/// more *after* it. A test cannot land a supersession inside a window that does
/// not exist, so this makes one: the task parks here, the ordinary
/// `cancel_preparations_for_superseded_predecessor` runs, and the read after
/// registration is then the only thing in the process that can still see it.
#[cfg(test)]
fn preparation_registration_delays(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Duration>> {
    static DELAYS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Duration>>,
    > = std::sync::OnceLock::new();
    DELAYS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
pub(super) fn delay_preparation_registration(playback_id: &str, delay: Duration) {
    preparation_registration_delays()
        .lock()
        .expect("preparation registration delays")
        .insert(playback_id.to_owned(), delay);
}

#[cfg(test)]
pub(super) fn take_preparation_registration_delay(playback_id: &str) -> Option<Duration> {
    preparation_registration_delays()
        .lock()
        .expect("preparation registration delays")
        .remove(playback_id)
}

/// Test-only seam between a dispatch and the answer that same exchange gives.
///
/// The first `staging` clause covers a race the emit rule has with the task it
/// just spawned: the candidate can finish, and its guard drop, before the emit
/// rule reads the pending map. In production that window is a few instructions
/// wide and cannot be widened from outside. Arming this makes the exchange wait
/// for exactly that to have happened, which is the only way the clause is under
/// test rather than merely present.
#[cfg(test)]
fn dispatch_settle_waits() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static WAITS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    WAITS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

#[cfg(test)]
pub(super) fn wait_for_the_dispatched_candidate(playback_id: &str) {
    dispatch_settle_waits()
        .lock()
        .expect("dispatch settle waits")
        .insert(playback_id.to_owned());
}

#[cfg(test)]
fn take_dispatch_settle_wait(playback_id: &str) -> bool {
    dispatch_settle_waits()
        .lock()
        .expect("dispatch settle waits")
        .remove(playback_id)
}

/// Resolve the staged successor this predecessor may announce now.
///
/// The ledger is the wanted-work authority: a route at the publication
/// sentinel is not enough, because commit and abort both remove the ledger
/// row. The owner-local [`crate::playback_control::ControlState`] checks the
/// matching preparation slot once more when it accepts the exchange; this
/// read therefore proposes an action but cannot create a second authority.
async fn staged_successor_action(
    state: &AppState,
    predecessor: &MediaSessionRoute,
    staged: Option<plurx_core::domain::MediaSessionStagedGeneration>,
) -> Result<Option<crate::playback_control::PreparedSuccessorAction>, ()> {
    let Some(staged) = staged else {
        return Ok(None);
    };
    if staged.expected_predecessor_incarnation_id != predecessor.incarnation_id {
        tracing::warn!("staged playback generation names the wrong predecessor");
        return Err(());
    }
    if staged.deadline_ms <= unix_ms() {
        return Ok(None);
    }
    let successor = state
        .store
        .media_session_route_by_incarnation(&staged.staged_incarnation_id)
        .await
        .map_err(|error| {
            tracing::warn!(?error, "staged media-session route read failed");
        })?
        .ok_or_else(|| {
            tracing::warn!("staged playback generation has no media-session route");
        })?;
    if successor.incarnation_id != staged.staged_incarnation_id
        || successor.user_id != predecessor.user_id
        || successor.playback_id != predecessor.playback_id
        || successor.owner_node_id != state.node_id
        || successor.owner_epoch != 1
        || successor.state != "active"
        || successor.publication_ready_at_ms
            != plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
    {
        tracing::warn!("staged media-session route failed its authority checks");
        return Err(());
    }
    let start = control_start_response(&successor).ok_or_else(|| {
        tracing::warn!("staged media-session response is unreadable");
    })?;
    let recipe =
        serde_json::from_str::<RemoteStartRequest>(&successor.recipe_json).map_err(|_| {
            tracing::warn!("staged media-session recipe is unreadable");
        })?;
    if !recipe.is_valid()
        || recipe.incarnation_id != successor.incarnation_id
        || recipe.user_id != successor.user_id
        || recipe.request.playback_id != successor.playback_id
        || start.session_id != successor.session_id
        || start.media_origin_ms != Some(successor.media_origin_ms)
        || !crate::playback_control::is_node_relative_playlist(
            &start.playlist_url,
            &successor.session_id,
        )
    {
        tracing::warn!("staged media-session payload failed its identity checks");
        return Err(());
    }
    let prepared = crate::playback_control::PreparedSuccessorAction {
        staged_incarnation_id: staged.staged_incarnation_id,
        deadline_ms: staged.deadline_ms,
        session_id: successor.session_id,
        playlist_url: start.playlist_url,
        media_origin_ms: successor.media_origin_ms,
        effective_selection: crate::playback_control::EffectiveSelection::from_recipe(
            &recipe,
            start.height,
            start.delivered_dynamic_range,
        ),
    };
    if !crate::playback_control::prepared_payload_is_valid(
        None,
        &prepared.session_id,
        &prepared.playlist_url,
        prepared.media_origin_ms,
        &prepared.effective_selection,
    ) {
        tracing::warn!("staged media-session action payload is invalid");
        return Err(());
    }
    Ok(Some(prepared))
}

pub(crate) fn control_error(
    status: StatusCode,
    code: &'static str,
    message: impl Into<String>,
    generation: Option<String>,
    control_epoch: Option<u64>,
    retry_after_ms: Option<u32>,
    invalid_field: Option<&'static str>,
) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(crate::playback_control::ControlErrorBody {
            code: code.to_owned(),
            message: message.into(),
            generation,
            control_epoch,
            retry_after_ms,
            invalid_field: invalid_field.map(str::to_owned),
        }),
    )
        .into_response()
}

/// Whether this route can authorize control at all, and the answer if not.
///
/// The *condition* lives here with the answer on purpose. Two of the three
/// gates that call this — public ingress and the owner side of the relay —
/// are separate call sites that could drift, and only the ingress one is
/// reachable from a test (the relay's needs a signed internal request and
/// there is no harness). Keeping the predicate here leaves those sites with
/// no logic of their own to get wrong: the remaining failure mode is deleting
/// the call, not answering differently from the plane next door.
pub(in crate::http) fn control_owner_refusal(
    route: &MediaSessionRoute,
    owner_epoch: Option<u64>,
) -> Option<Response> {
    let now_unix_ms = unix_ms();
    (route.publication_ready_at_ms != 0 || route.lease_expires_at_ms <= now_unix_ms)
        .then(|| control_owner_answer(route, owner_epoch, now_unix_ms))
}

/// The control-plane answer for a route that is no longer authorizing control
/// — its publication handoff is pending, or its owner lease has stopped being
/// renewed.
///
/// There are **three** gates: public ingress, the owner side of the relay in
/// `internal_media_sessions::control_inner`, and `verify_authority`'s re-read
/// at the owner. They must not diverge, because the earlier ones return
/// before the later ones run and would otherwise decide the answer on their
/// own. All three reach this function, and the classification is the media
/// plane's, so neither plane can answer one route differently from the other.
///
/// `now_unix_ms` comes from the caller's own liveness test rather than being
/// read again here, so the two cannot land on opposite sides of the lease
/// boundary.
pub(in crate::http) fn control_owner_answer(
    route: &MediaSessionRoute,
    owner_epoch: Option<u64>,
    now_unix_ms: i64,
) -> Response {
    match crate::playback_control::classify_control_owner(route, now_unix_ms) {
        crate::playback_control::ControlStateError::OwnerLost => {
            control_owner_lost(route, owner_epoch)
        }
        _ => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Transition);
            control_error(
                StatusCode::TOO_EARLY,
                "owner_transition",
                "the media owner is not currently authorizing control; takeover is not settled",
                Some(route.incarnation_id.clone()),
                owner_epoch,
                Some(500),
                None,
            )
        }
    }
}

/// No successor can answer this session's control exchange, so the client is
/// told to stop rather than to retry every 500 ms forever. The retry hint is
/// deliberately absent: there is nothing to come back to, and all three
/// reporters read 425-with-a-hint as an instruction to keep going.
///
/// The wording does not assert that a node died. On a single-node install
/// every session is untakeoverable by construction, and the common cause
/// there is the node's own store writes stalling past the lease rather than
/// the node being gone — so the message says what is true in both cases: this
/// session cannot be recovered, reopen.
pub(in crate::http) fn control_owner_lost(
    route: &MediaSessionRoute,
    owner_epoch: Option<u64>,
) -> Response {
    crate::playback_control::record(crate::playback_control::MetricOutcome::OwnerLost);
    control_error(
        StatusCode::GONE,
        "owner_lost",
        "this media session's owner no longer holds it and nothing can take it over; \
         reopen playback",
        Some(route.incarnation_id.clone()),
        owner_epoch,
        None,
        None,
    )
}

/// Execute a control exchange after ingress (or the exact-write relay) has
/// proved the durable owner tuple. The manager repeats the tuple fence against
/// owner-local state before it can renew activity.
pub(crate) async fn control_local(
    state: &AppState,
    route: &MediaSessionRoute,
    request: crate::playback_control::ControlRequestV1,
    deadline_unix_ms: i64,
) -> Response {
    // Public ingress and the internal relay both own the absolute exchange
    // deadline. Keep admission and every nonterminal mutation in that caller
    // future; only an already-accepted End receives a detached continuation
    // below for its durable acknowledgement.
    //
    // `switched` on a draining predecessor releases it, and it is released
    // *here* — after the exchange, on the owner — for three reasons, each of
    // which was a defect in the first placement:
    //
    // Only the owner reaches this function, and it reaches it on both paths:
    // public ingress that owns the route falls through to it, and a relayed
    // request arrives at `internal_media_sessions::control_authorized`, which
    // calls it directly. `control_inner` is on neither of those for a remote
    // route, so a release placed there ran only when the client happened to
    // hit the owning node.
    //
    // After the exchange, so the exchange is answered. Ending the row first
    // meant the very packet reporting the switch was answered `410
    // session_ended` — every successful early release counted as `Gone`, and
    // anything else riding that request was dropped.
    //
    // After the actor's fences. `control_inner` checks the generation, the
    // owner epoch and the route state; the client-instance and monotonic
    // sequence fences live in `ControlState::accept`, inside this call. The
    // drain exists to protect the instance that committed, so a packet from a
    // second instance — or a replayed one with a stale sequence — must not be
    // able to pull it out from under that instance.
    let releases_drain = route.drain_deadline_ms.is_some()
        && request.acknowledgement.as_ref().map(|ack| ack.state)
            == Some(crate::playback_control::AcknowledgementState::Switched);
    let response = control_local_inner(state, route, request, deadline_unix_ms).await;
    // Only on an accepted exchange. A refused one proves nothing about what
    // reached a screen.
    if releases_drain && response.status().is_success() {
        if let Err(error) = state
            .store
            .end_media_session(&route.session_id, "superseded", unix_ms())
            .await
        {
            // Not fatal. The deadline and the cross-node sweep are both still
            // behind this, so a failed early release costs the rest of the
            // window, not correctness.
            tracing::debug!(%error, "a switched acknowledgement could not release the drain");
        }
    }
    response
}

pub(super) async fn control_local_inner(
    state: &AppState,
    route: &MediaSessionRoute,
    request: crate::playback_control::ControlRequestV1,
    deadline_unix_ms: i64,
) -> Response {
    control_local_with_settlement_capacity(
        state,
        route,
        request,
        deadline_unix_ms,
        preparation_settlement_slots(),
    )
    .await
}

pub(super) async fn control_local_with_settlement_capacity(
    state: &AppState,
    route: &MediaSessionRoute,
    request: crate::playback_control::ControlRequestV1,
    deadline_unix_ms: i64,
    slots: Arc<tokio::sync::Semaphore>,
) -> Response {
    let owner_epoch = match u64::try_from(route.owner_epoch)
        .ok()
        .filter(|epoch| *epoch > 0)
    {
        Some(epoch) => epoch,
        None => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the local media route has an invalid owner epoch",
                Some(route.incarnation_id.clone()),
                None,
                Some(500),
                None,
            );
        }
    };
    let start = match control_start_response(route) {
        Some(start) => start,
        None => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
            return control_error(
                StatusCode::NOT_FOUND,
                "session_gone",
                "playback control was not advertised for this session",
                None,
                None,
                None,
                None,
            );
        }
    };
    let recipe = match serde_json::from_str::<RemoteStartRequest>(&route.recipe_json) {
        Ok(recipe) => recipe,
        Err(_) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the durable delivery recipe is unreadable",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
    };
    if let Err(field) = request.validate(
        start.duration_ms,
        crate::playback_control::target_duration_ms(&recipe),
    ) {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Invalid);
        return control_error(
            StatusCode::BAD_REQUEST,
            "invalid_control",
            "a control field is outside the bounded v1 contract",
            Some(route.incarnation_id.clone()),
            Some(owner_epoch),
            None,
            Some(field),
        );
    }
    if request.generation != route.incarnation_id {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Stale);
        return control_error(
            StatusCode::CONFLICT,
            "stale_control",
            "the control generation is no longer current",
            Some(route.incarnation_id.clone()),
            Some(owner_epoch),
            None,
            None,
        );
    }
    if request.control_epoch != owner_epoch {
        crate::playback_control::record(crate::playback_control::MetricOutcome::OwnerChanged);
        return control_error(
            StatusCode::CONFLICT,
            "owner_changed",
            "the media session owner epoch changed",
            Some(route.incarnation_id.clone()),
            Some(owner_epoch),
            None,
            None,
        );
    }
    let can_settle_preparation = request
        .accepts(crate::playback_control::PREPARE_REPLACEMENT_ACTION)
        || request.acknowledgement.is_some()
        || owner_epoch > 1
        || request.demand == crate::playback_control::PlaybackDemand::End;
    let staged_read = if !can_settle_preparation {
        Ok(None)
    } else {
        let read = state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await;
        #[cfg(test)]
        let read = if staged_read_faults()
            .lock()
            .expect("staged read faults")
            .remove(&route.incarnation_id)
        {
            Err(plurx_core::error::StoreError::Database(
                "injected staged ledger read failure".into(),
            ))
        } else {
            read
        };
        read.map_err(|error| {
            tracing::warn!(?error, "staged playback generation read failed");
        })
    };
    let ledger_unavailable = staged_read.is_err();
    let staged_generation = staged_read.ok().flatten();
    // Staging is detached from the exchange that requested it, so only a
    // later exchange can see the durable result. End never announces new
    // work: its owner-local transaction aborts any slot the session held.
    let prepared_successor = if request.demand == crate::playback_control::PlaybackDemand::End {
        crate::playback_control::PreparedSuccessorObservation::Inactive
    } else if !request.accepts(crate::playback_control::PREPARE_REPLACEMENT_ACTION) {
        crate::playback_control::PreparedSuccessorObservation::NotRequested
    } else if ledger_unavailable {
        crate::playback_control::PreparedSuccessorObservation::Unavailable
    } else {
        match staged_successor_action(state, route, staged_generation.clone()).await {
            Ok(Some(successor)) => {
                crate::playback_control::PreparedSuccessorObservation::Ready(successor)
            }
            Ok(None) => crate::playback_control::PreparedSuccessorObservation::Absent,
            Err(()) => crate::playback_control::PreparedSuccessorObservation::Unavailable,
        }
    };
    let terminal_committer =
        (request.demand == crate::playback_control::PlaybackDemand::End).then(|| {
            Arc::new(DurableTerminalCommitter {
                store: Arc::clone(&state.store),
                route: route.clone(),
                start: start.clone(),
                recipe: recipe.clone(),
                request: request.clone(),
                faults: None,
            }) as Arc<dyn crate::playback_control::TerminalControlCommitter>
        });
    // Resolve the engine-local gate before actor acceptance. It must remain
    // available even on a request without an acknowledgement: an owner-epoch
    // rollover can itself produce the abort directive that transfers an
    // inherited preparation away from the now-fenced old executor.
    let Some(preparation_gate) = state
        .transcode
        .session_preparation_gate(&route.session_id)
        .await
    else {
        crate::playback_control::record(crate::playback_control::MetricOutcome::Transition);
        return control_error(
            StatusCode::TOO_EARLY,
            "owner_transition",
            "the durable route is active but its local worker is not yet available",
            Some(route.incarnation_id.clone()),
            Some(owner_epoch),
            Some(500),
            None,
        );
    };
    let preparation_status = state
        .transcode
        .hls_session_status_publication(&route.session_id)
        .await
        .and_then(|publication| publication.result.ok());
    // A missing ledger can mean maintenance already removed an Aborting
    // successor. Acknowledgements still need an owned executor to settle the
    // exact actor slot. Unknown reads must also reach frozen actor replay.
    let preparation_reservation = if staged_generation.is_some()
        || ledger_unavailable
        || request.acknowledgement.is_some()
        || owner_epoch > 1
        || request.demand == crate::playback_control::PlaybackDemand::End
    {
        let Some(reservation) =
            reserve_preparation_settlement(state, deadline_unix_ms, slots).await
        else {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "preparation settlement capacity or serving authority is unavailable",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        };
        Some(reservation)
    } else {
        None
    };
    let preparation_admission = Some(Arc::new(DurablePreparationSettlementAdmission {
        state: state.clone(),
        gate: preparation_gate,
        route: route.clone(),
        start: start.clone(),
        recipe: recipe.clone(),
        request: request.clone(),
        status: preparation_status,
        reservation: std::sync::Mutex::new(preparation_reservation),
        receipt: std::sync::Mutex::new(None),
    }));
    let preparation_admission_trait = preparation_admission.as_ref().map(|admission| {
        Arc::clone(admission) as Arc<dyn crate::playback_control::PreparationSettlementAdmission>
    });
    let result = match state
        .transcode
        .hls_session_control_with_terminal(
            crate::playback_control::LocalControlRequest {
                session_id: &route.session_id,
                generation: &route.incarnation_id,
                owner_node_id: &route.owner_node_id,
                owner_epoch,
                client_instance_id: &request.client_instance_id,
                sequence: request.sequence,
                snapshot: crate::playback_control::PlaybackDemandSnapshot::from(&request),
                prepared_successor,
            },
            deadline_unix_ms,
            terminal_committer,
            preparation_admission_trait,
        )
        .await
    {
        Some(Ok(result)) => result,
        Some(Err(crate::playback_control::ControlStateError::OwnerChanged)) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::OwnerChanged);
            return control_error(
                StatusCode::CONFLICT,
                "owner_changed",
                "the owner-local control epoch changed",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                None,
                None,
            );
        }
        Some(Err(crate::playback_control::ControlStateError::RateLimited(retry_after_ms))) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::RateLimited);
            return control_error(
                StatusCode::TOO_MANY_REQUESTS,
                "control_rate_limited",
                "the control sequence advanced faster than the per-session budget",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(retry_after_ms),
                None,
            );
        }
        Some(Err(crate::playback_control::ControlStateError::SessionEnded)) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
            return control_error(
                StatusCode::GONE,
                "session_ended",
                "the durable media session ended before control could renew it",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                None,
                None,
            );
        }
        Some(Err(crate::playback_control::ControlStateError::PauseExpired)) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Gone);
            return control_error(
                StatusCode::GONE,
                "pause_grace_expired",
                "the paused rolling presentation reached its finite grace; resume may open one replacement at the saved position",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                None,
                None,
            );
        }
        Some(Err(crate::playback_control::ControlStateError::OwnerTransition)) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Transition);
            return control_error(
                StatusCode::TOO_EARLY,
                "owner_transition",
                "the media owner changed while control was being admitted",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
        Some(Err(crate::playback_control::ControlStateError::OwnerLost)) => {
            return control_owner_lost(route, Some(owner_epoch));
        }
        Some(Err(crate::playback_control::ControlStateError::Unavailable)) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the owner could not revalidate durable control authority",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
        Some(Err(_)) => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Stale);
            return control_error(
                StatusCode::CONFLICT,
                "stale_control",
                "the generation, client instance, or sequence fence is stale",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                None,
                None,
            );
        }
        None => {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Transition);
            return control_error(
                StatusCode::TOO_EARLY,
                "owner_transition",
                "the durable route is active but its local worker is not yet available",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
    };
    let retained_preparation_response = if result.preparation_directive.is_some() {
        let settlement = match preparation_admission
            .as_ref()
            .and_then(|admission| admission.receipt())
        {
            Some(receipt) => receipt.wait_before(deadline_unix_ms).await,
            None => None,
        };
        match settlement {
            Some(PreparationSettlement::Committed(response)) => Some(*response),
            Some(PreparationSettlement::Aborted) => None,
            Some(PreparationSettlement::Rejected) => {
                crate::playback_control::record(crate::playback_control::MetricOutcome::Stale);
                return control_error(
                    StatusCode::CONFLICT,
                    "stale_control",
                    "the preparation acknowledgement lost its commit or deadline fence",
                    Some(route.incarnation_id.clone()),
                    Some(owner_epoch),
                    None,
                    None,
                );
            }
            Some(PreparationSettlement::Unavailable) | None => {
                crate::playback_control::record(
                    crate::playback_control::MetricOutcome::Unavailable,
                );
                return control_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "control_unavailable",
                    "the preparation acknowledgement was not durably settled",
                    Some(route.incarnation_id.clone()),
                    Some(owner_epoch),
                    Some(500),
                    None,
                );
            }
        }
    } else {
        None
    };
    // A response that came back from a durable settlement is the byte-exact
    // body that was stored, and `preparation_ack_replay` / `terminal_ack_replay`
    // serve that stored JSON without ever reaching the stamp below. Anything
    // this exchange adds to it afterwards makes the first answer and its own
    // replay differ, which is the one property §4 says must always hold.
    let durable_receipt_response =
        retained_preparation_response.is_some() || result.lease_state == "ended";
    let mut response = if let Some(response) = retained_preparation_response {
        response
    } else if result.lease_state == "ended" {
        if request.demand != crate::playback_control::PlaybackDemand::End {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "a terminal control result did not match the requested demand",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        }
        let Some(commit) = &result.terminal_commit else {
            crate::playback_control::record(crate::playback_control::MetricOutcome::Unavailable);
            return control_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "control_unavailable",
                "the terminal control continuation was not admitted",
                Some(route.incarnation_id.clone()),
                Some(owner_epoch),
                Some(500),
                None,
            );
        };
        match commit.wait().await {
            Ok(response) => response,
            Err(()) => {
                crate::playback_control::record(
                    crate::playback_control::MetricOutcome::Unavailable,
                );
                return control_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "control_unavailable",
                    "the terminal control acknowledgement was not durably committed",
                    Some(route.incarnation_id.clone()),
                    Some(owner_epoch),
                    Some(500),
                    None,
                );
            }
        }
    } else {
        let subtitle_window_seconds = state.subtitle_window_seconds().await;
        let subtitle_cache = subtitle_track_cache(
            state,
            &recipe,
            &request.selection.subtitle,
            &request,
            subtitle_window_seconds,
        )
        .await;
        local_control_response(
            route,
            &start,
            &recipe,
            &request,
            &result,
            unix_ms(),
            crate::playback_control::subtitle_readiness_value(
                &request.selection.subtitle,
                subtitle_cache,
            ),
        )
    };
    // The ask becomes durable before this exchange is reported accepted.
    //
    // §1's rule, and the window it closes is narrow but real: a client told
    // its new selection was taken, with nothing durable saying so, leaves
    // every later admission decision comparing against an ask that never
    // landed — and a restart or an owner change in that window loses the
    // request entirely, with the session continuing to serve the old
    // selection and nothing anywhere recording that anything was asked.
    //
    // Only an exchange whose ask the row does not already name pays for this.
    // A heartbeat repeats the same selection, so it writes nothing and cannot
    // fail here; the cost falls on the exchange that actually changed
    // something, which is the one that has something to lose.
    //
    // A failed write refuses the exchange rather than answering it. That is
    // the whole point of "before reported accepted": answering anyway would
    // be reporting a request taken that nothing has recorded, which is worse
    // than a client retrying one exchange.
    if let Some(desired) = result.selection.persist_desired.clone() {
        match state
            .store
            .record_desired_selection(
                route.user_id,
                &route.playback_id,
                &desired.digest,
                &desired.canonical_form,
                unix_ms(),
            )
            .await
        {
            Ok(_) => {
                state
                    .transcode
                    .record_desired_persisted(&route.session_id, &desired.digest)
                    .await;
            }
            Err(error) => {
                tracing::warn!(
                    session = %crate::transcode::session_log_id(&route.session_id),
                    "recording the viewer's selection failed: {error}"
                );
                crate::playback_control::record(
                    crate::playback_control::MetricOutcome::Unavailable,
                );
                return control_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "control_unavailable",
                    "this selection could not be recorded; retry shortly",
                    Some(route.incarnation_id.clone()),
                    Some(owner_epoch),
                    Some(500),
                    None,
                );
            }
        }
    }
    let outcome = match result.disposition {
        crate::playback_control::ControlDisposition::Accepted => {
            crate::playback_control::MetricOutcome::Accepted
        }
        crate::playback_control::ControlDisposition::Replay => {
            crate::playback_control::MetricOutcome::Replay
        }
    };
    crate::playback_control::record(outcome);
    crate::playback_control::record_platform(outcome, result.platform);
    let incumbent_waiting = result.disposition
        == crate::playback_control::ControlDisposition::Accepted
        && request.demand == crate::playback_control::PlaybackDemand::Active
        && matches!(
            request.render_state,
            crate::playback_control::RenderState::Waiting
                | crate::playback_control::RenderState::Stalled
        );
    if incumbent_waiting {
        // Preparation is speculative. A current-player wait outranks it even
        // when the client still has loaded media: synchronously take the exact
        // registry entry and let its detached cancellation owner abort the row,
        // actor slot, and worker without extending this exchange's deadline.
        cancel_preparations_for_incumbent_wait(&route.playback_id);
    }
    // The replacement seam. A viewer's quality change arrives as a new session
    // that replaces the old one, so the transition to measure is
    // predecessor-delivered against this session's delivered — both already
    // resolved, which is why nothing here reads the store or spawns.
    //
    // `reopen_reason: Some(Stall)` is excluded deliberately: that is the
    // client adapting to a failure, not a viewer asking for something, and
    // §3.3's own rule is that M6 prepares for a change the *client asked for*.
    let replaced = if recipe.request.reopen_reason.is_none() {
        remember_delivered_selection(
            &recipe.request.playback_id,
            &route.session_id,
            recipe.request.file_id,
            &request.selection,
            &response.effective_selection,
            crate::playback_control::GradeIntent::from_request(&recipe.request),
        )
    } else {
        None
    };
    if let Some((previous, previous_grade)) = replaced {
        crate::playback_control::record_preparation_observation(
            crate::playback_control::PreparationSeam::Replacement,
        );
        let delivered_view = crate::playback_control::RecipeView {
            selection: &previous,
            grade: previous_grade,
        };
        let proposed_view = crate::playback_control::RecipeView {
            selection: &response.effective_selection,
            grade: crate::playback_control::GradeIntent::from_request(&recipe.request),
        };
        let conditions = crate::playback_control::PreparationConditions {
            observed_download_bps: request.observed_download_bps,
            delivered_bps: response.delivery.delivered_bps,
        };
        let capabilities = result.selection.capabilities.clone();
        crate::playback_control::record_preparation_decision(
            result.platform,
            crate::playback_control::decide_preparation(
                delivered_view,
                proposed_view,
                capabilities.as_ref(),
                conditions,
            ),
        );
        crate::playback_control::record_preparation_counterfactual(
            result.platform,
            crate::playback_control::decide_preparation_after_client_release(
                delivered_view,
                proposed_view,
                conditions,
            ),
        );
    }
    // Not `selection.changed`: an ask that arrived while the preparation slot
    // was busy is "changed" for exactly one exchange and then never again, so
    // the successor the viewer actually asked for was built once, refused, and
    // forgotten. This stays true until the ask has been dispatched.
    let planned_relocation = if route.owner_node_id == state.node_id {
        state.serving.current_planned_outage().await
    } else {
        None
    };
    // Computed once for both the claim below and the answer at the end of the
    // exchange: they must be the same string, or a client would be told
    // `staging` about a candidate for an ask it has already left.
    let desired_digest = request.selection.desired().digest();
    let preparation_purpose = (!incumbent_waiting)
        .then(|| {
            planned_relocation
                .map(PreparationPurpose::PlannedRelocation)
                .or_else(|| {
                    result
                        .selection
                        .dispatch_preparation
                        .then_some(PreparationPurpose::SelectionChange)
                })
        })
        .flatten();
    // Not on an exchange answering from a durable receipt. That body says
    // `none` and cannot say anything else — it is the stored answer, and its
    // replay has to be the same bytes — so dispatching here would tell a client
    // `none` on the very exchange that started its successor, which is the one
    // thing this whole seam exists to prevent: the client reopens immediately
    // and orphans a successor that then holds the slot until it expires. The
    // planned-relocation purpose consults no slot state, so it would otherwise
    // fire on a commit acknowledgement or an End during a drain. The next
    // exchange composes its own body and dispatches there.
    if let Some(purpose) = preparation_purpose.filter(|_| !durable_receipt_response) {
        crate::playback_control::record_preparation_observation(
            crate::playback_control::PreparationSeam::InSession,
        );
        // The film time this exchange accepted, bounded by the film itself.
        //
        // `validate` is deliberately looser than that: it allows the duration
        // plus one target duration of slack, so a client reporting the final
        // segment's *end* is not refused, and it falls back to
        // `MAX_MEDIA_MILLIS` when the duration is unknown. Neither is a place a
        // successor may begin. Staging past the end would hold this playback's
        // one preparation slot, a durable row and an admission slot for a full
        // `PREPARATION_DEADLINE_MS` on a resume no viewer can ever reach — and
        // this value is now client-reported rather than derived from what the
        // server observed itself delivering, so the seam is where it earns a
        // bound. `.max(0)` cannot fire on a validated envelope; it is here so a
        // future caller that has not validated cannot serialize a negative
        // `start_seconds`.
        let accepted_film_time_ms = {
            let asked = request.seek_target_ms.unwrap_or(request.position_ms).max(0);
            match start.duration_ms {
                Some(duration) if duration > 0 => asked.min(duration),
                _ => asked,
            }
        };
        // Claimed before the spawn, not inside it: a task that has not been
        // polled yet is still work this playback is doing, and an exchange
        // that raced in between would otherwise be told `none`.
        let pending = PendingCandidateGuard::begin(&route.playback_id, &desired_digest);
        // Spawned, never awaited: see the function's own doc. The exchange has
        // spent its deadline by here and the response is already built.
        tokio::spawn(process_preparation_candidate(
            state.clone(),
            pending,
            PreparationCandidateInputs {
                session_id: route.session_id.clone(),
                route: route.clone(),
                recipe: recipe.clone(),
                planning_caps: retained_planning_caps(&route.response_json),
                planning_overrides: retained_planning_overrides(&route.response_json),
                selection: request.selection.clone(),
                observed_download_bps: request.observed_download_bps,
                delivered: response.effective_selection.clone(),
                delivered_bps: response.delivery.delivered_bps,
                capabilities: result.selection.capabilities.clone(),
                // The exchange's own accepted platform, the same value
                // `record_platform` just used — not the retained capability
                // document's, which can be absent on a session this build did
                // not start and would then leave the measurement
                // unattributable.
                platform: result.platform,
                accepted_film_time_ms,
                purpose,
            },
        ));
    }
    // Answered last, because it is the only field that depends on the action
    // this exchange ended up carrying. `offered` is not a guess about the slot:
    // it is a restatement of what `action` already is, which is what keeps the
    // two from ever disagreeing.
    //
    // `staging` covers three different truths that the client must treat
    // identically — this exchange just dispatched a candidate, a candidate
    // spawned by an earlier exchange is still doing its Store reads, or a
    // successor is registered and priming. A client that could distinguish them
    // would have nothing different to do with the distinction.
    //
    // Every one of the three is measured against *this exchange's*
    // `desired_digest`, which is what keeps `staging` honest across a change of
    // mind: work still winding down for the selection the viewer just left is
    // not work being done for the ask they are waiting on, and saying `staging`
    // about it would hold them past the point where reopening was the better
    // answer — and then make them reopen anyway, several seconds later.
    //
    // The first clause is not redundant with the second, though it looks it.
    // The guard is claimed just above with exactly this digest, so the marker
    // does normally answer for the dispatching exchange — but the task it was
    // claimed for is already spawned and may run to completion on another
    // thread before this line reads the map. Whether the exchange that
    // dispatched says `staging` is a fact about that exchange, not a fact about
    // a map another thread owns, and a client told `none` on the very exchange
    // that started its successor reopens immediately and orphans it.
    #[cfg(test)]
    if take_dispatch_settle_wait(&route.playback_id) {
        while pending_candidate_for_playback(&route.playback_id).is_some() {
            tokio::task::yield_now().await;
        }
    }
    // Only on a body this exchange composed. On a durable-receipt body the
    // field stays absent, which is the tri-state's documented "not evaluated
    // here" — the honest answer for a response written before this exchange
    // evaluated the slot, and never a decline.
    if !durable_receipt_response {
        response.delivery.preparation = Some(
            if matches!(
                response.action,
                crate::playback_control::ControlAction::Prepare { .. }
            ) {
                "offered"
            } else if preparation_purpose.is_some()
                || pending_candidate_for_playback(&route.playback_id)
                    .is_some_and(|pending| pending == desired_digest)
                || has_active_preparation_for_ask(&route.playback_id, &desired_digest)
            {
                "staging"
            } else {
                "none"
            }
            .to_owned(),
        );
    }
    tracing::debug!(
        session = %crate::transcode::session_log_id(&route.session_id),
        owner_epoch,
        sequence = result.accepted_sequence,
        replay = result.disposition == crate::playback_control::ControlDisposition::Replay,
        lease_timeout_ms = result.lease_timeout_ms,
        "playback control exchange"
    );
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(response),
    )
        .into_response()
}
