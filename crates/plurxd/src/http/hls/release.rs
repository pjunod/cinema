use super::*;

/// DELETE /api/v1/hls/:session — the player is done with this stream.
///
/// Capability auth, like the playlist and segment routes: the session id *is*
/// the credential, and requiring a header would stop the browser sending this
/// with `keepalive` as a tab closes — which is the whole point. Without it a
/// finished stream keeps its encoder for the 60-second idle timeout plus a
/// reaper tick, holding a hardware slot nobody is watching.
///
/// Idempotent: deleting a session that has already gone is a success, because
/// the caller's intent ("this must not be running") is satisfied either way.
pub async fn delete(State(state): State<AppState>, AxPath(session): AxPath<String>) -> StatusCode {
    let deadline = response_publication_deadline();
    delete_with_slots(state, session, session_release_slots(), deadline).await
}

/// Run an authenticated operator terminal through the same exact, durable
/// release coordinator used by public capability DELETE. The first admitted
/// terminal intent owns telemetry when concurrent callers join one settlement.
pub(in crate::http) async fn release_with_terminal(
    state: AppState,
    session: String,
    terminal: crate::vodserve::Terminal,
    reason: &'static str,
) -> StatusCode {
    release_with_slots(
        state,
        session,
        session_release_slots(),
        response_publication_deadline(),
        terminal,
        reason,
    )
    .await
}

pub(super) async fn delete_with_slots(
    state: AppState,
    session: String,
    slots: Arc<tokio::sync::Semaphore>,
    deadline: Instant,
) -> StatusCode {
    release_with_slots(
        state,
        session,
        slots,
        deadline,
        crate::vodserve::Terminal::Deleted,
        "released by client",
    )
    .await
}

pub(super) async fn release_with_slots(
    state: AppState,
    session: String,
    slots: Arc<tokio::sync::Semaphore>,
    deadline: Instant,
    terminal: crate::vodserve::Terminal,
    reason: &'static str,
) -> StatusCode {
    let release_settlement = match state
        .media_sessions
        .begin_release_reconciliation_with_intent(
            &session,
            crate::media_sessions::ReleaseIntent { terminal, reason },
        )
        .await
    {
        ReleaseAdmission::Won(settlement) => settlement,
        ReleaseAdmission::Joined(settlement) => {
            return tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                settlement.wait(),
            )
            .await
            .unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
        }
        ReleaseAdmission::Full => return StatusCode::SERVICE_UNAVAILABLE,
    };
    // The task owns both the bounded settlement slot and the full durable
    // end/fence/cleanup transaction. Dropping the HTTP request or timing out
    // its wait only detaches this owner; it cannot cancel cleanup after the
    // Store has committed the terminal row. Ownership transfers immediately
    // after election: even cancellation while capacity is saturated cannot
    // strand an in-flight release fence.
    let settlement_task = tokio::spawn(async move {
        let permit = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            slots.acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) | Err(_) => {
                state
                    .media_sessions
                    .complete_release_settlement(
                        &session,
                        &release_settlement,
                        StatusCode::SERVICE_UNAVAILABLE,
                    )
                    .await;
                return StatusCode::SERVICE_UNAVAILABLE;
            }
        };
        let _permit = permit;
        let worker_state = state.clone();
        let worker_session = session.clone();
        let worker_settlement = Arc::clone(&release_settlement);
        match tokio::spawn(async move {
            release_session(
                worker_state,
                worker_session,
                worker_settlement,
                terminal,
                reason,
            )
            .await
        })
        .await
        {
            Ok(status) => status,
            Err(error) => {
                tracing::error!(
                    target: "plurxd::http::hls",
                    %error,
                    session = %crate::transcode::session_log_id(&session),
                    "media-session release transaction panicked; transferred to lifecycle reconciliation"
                );
                state
                    .media_sessions
                    .defer_release_reconciliation(&session, &release_settlement)
                    .await;
                StatusCode::SERVICE_UNAVAILABLE
            }
        }
    });
    match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), settlement_task).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            tracing::warn!(
                target: "plurxd::http::hls",
                %error, "media-session release settlement task failed"
            );
            StatusCode::SERVICE_UNAVAILABLE
        }
        // Dropping JoinHandle detaches the still-owned cleanup task.
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

pub(super) fn session_release_slots() -> Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    Arc::clone(
        SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(SESSION_RELEASE_CAPACITY))),
    )
}

pub(super) async fn release_session(
    state: AppState,
    session: String,
    settlement: Arc<ReleaseSettlement>,
    terminal: crate::vodserve::Terminal,
    reason: &'static str,
) -> StatusCode {
    // Close publication before Store latency without choosing the local VOD
    // tombstone cause. The Store CAS below records the first writer; phase two
    // projects that exact durable winner.
    state
        .transcode
        .begin_session_publication_fence(&session)
        .await;
    #[cfg(test)]
    if let Some(pause) = release_pause_for(&session) {
        pause.wait().await;
        pause.wait().await;
    }
    // Never put an HTTP deadline around this mutation. SQLite blocking work
    // and a submitted Raft proposal may commit after caller cancellation, so
    // this detached capacity-owned task retains the attempt until the Store
    // returns a definitive result.
    let route = match end_media_session_for_release(&state, &session, terminal).await {
        Ok(route) => route,
        Err(error) => {
            tracing::warn!(
                target: "plurxd::http::hls",
                %error,
                session = %crate::transcode::session_log_id(&session),
                "durable media-session release is commit-unknown; transferred to lifecycle reconciliation"
            );
            state
                .media_sessions
                .defer_release_reconciliation(&session, &settlement)
                .await;
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };
    match route {
        Some(route) => {
            // Publish the returned exact terminal owner before any local or
            // remote stop await. A negative cache entry would look absent to
            // ingress and reopen the still-live local fallback in this gap.
            let projected_terminal =
                crate::vodserve::Terminal::from_durable_reason(route.terminal_reason.as_deref())
                    .unwrap_or(terminal);
            let projected_reason = projected_terminal.control_reason();
            state
                .transcode
                .begin_session_terminal(&session, projected_terminal, projected_reason)
                .await;
            // R3. A viewer releasing their own stream is the one event that
            // tells the server nobody will read its retired objects again —
            // but only for a client whose teardown ordering has been audited.
            // The class comes from the durable recipe this coordinator just
            // resolved, so a remote owner, an idempotent replay and an owner
            // takeover all read the same answer the create wrote.
            //
            // Restricted to a client-initiated release: an admin stop, a
            // revocation or a supersession is not the viewer saying they are
            // finished with the bytes.
            if projected_terminal == crate::vodserve::Terminal::Deleted {
                let class = exact_release_class(&route);
                if let Some(deadline) = state
                    .transcode
                    .accept_exact_session_release(&session, class)
                    .await
                {
                    tracing::debug!(
                        target: "plurxd::http::hls",
                        session = %crate::transcode::session_log_id(&session),
                        class = class.label(),
                        in_ms = deadline.saturating_duration_since(Instant::now()).as_millis(),
                        "retired object promise shortened by an exact same-viewer release"
                    );
                }
            }
            let Some(durable_release) = state
                .media_sessions
                .complete_release_with_route(route.clone())
                .await
            else {
                state
                    .media_sessions
                    .defer_release_reconciliation(&session, &settlement)
                    .await;
                return StatusCode::SERVICE_UNAVAILABLE;
            };
            state
                .transcode
                .complete_session_release_durable(&durable_release);
            #[cfg(test)]
            if let Some(pause) = release_after_tombstone_pause_for(&session) {
                pause.wait().await;
                pause.wait().await;
            }
            // A remote worker must acknowledge its local terminal generation
            // before the shared DELETE settles. The acknowledgement is fast:
            // physical reap is already transferred to the worker's bounded
            // lifecycle owner. If transport is unavailable, retain the exact
            // owner route and settlement for lifecycle retry.
            if !durable_release.terminal_projection_complete()
                && route.owner_node_id != state.node_id
            {
                let mut projected = false;
                for attempt in 0..REMOTE_RELEASE_ATTEMPTS {
                    match stop_owned_session_because(
                        &state,
                        &route,
                        projected_terminal,
                        projected_reason,
                    )
                    .await
                    {
                        Ok(()) => {
                            projected = true;
                            break;
                        }
                        Err(error) => {
                            tracing::warn!(
                                target: "plurxd::http::hls",
                                error = ?error,
                                session = %crate::transcode::session_log_id(&session),
                                owner = %route.owner_node_id,
                                attempt = attempt + 1,
                                "exact owner terminal projection attempt failed"
                            );
                            if attempt + 1 < REMOTE_RELEASE_ATTEMPTS {
                                tokio::time::sleep(REMOTE_RELEASE_RETRY_DELAY).await;
                            }
                        }
                    }
                }
                if !projected {
                    state
                        .media_sessions
                        .defer_release_reconciliation(&session, &settlement)
                        .await;
                    return StatusCode::SERVICE_UNAVAILABLE;
                }
            }
            if !durable_release.terminal_projection_complete() {
                let Some(projected_release) = state
                    .media_sessions
                    .complete_terminal_projection(
                        &route,
                        MediaSessionProjectionCompletion::PredecessorAcknowledged,
                    )
                    .await
                    .filter(|proof| proof.terminal_projection_complete())
                else {
                    state
                        .media_sessions
                        .defer_release_reconciliation(&session, &settlement)
                        .await;
                    return StatusCode::SERVICE_UNAVAILABLE;
                };
                state
                    .transcode
                    .complete_session_release_durable(&projected_release);
            }
        }
        None => {
            // Confirmed durable absence is idempotent success. The elected
            // owner already tombstoned VOD or actor-fenced rolling and
            // transferred physical cleanup before submitting the Store
            // mutation, so lifting the temporary route fence cannot reopen a
            // process-local fallback here.
            state
                .transcode
                .begin_session_terminal(&session, terminal, reason)
                .await;
            let durable_release = state.media_sessions.complete_release_absent(&session).await;
            state
                .transcode
                .complete_session_release_durable(&durable_release);
        }
    }
    state
        .media_sessions
        .complete_release_settlement(&session, &settlement, StatusCode::NO_CONTENT)
        .await;
    StatusCode::NO_CONTENT
}

async fn end_media_session_for_release(
    state: &AppState,
    session: &str,
    terminal: crate::vodserve::Terminal,
) -> Result<Option<MediaSessionRoute>, StoreError> {
    #[cfg(test)]
    if take_injected_release_error(session) {
        return Err(StoreError::Database(
            "injected commit-unknown media-session release".to_owned(),
        ));
    }
    state
        .store
        .end_media_session(session, terminal.durable_reason(), unix_ms())
        .await
}

#[cfg(test)]
fn injected_release_errors() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static ERRORS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    ERRORS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

#[cfg(test)]
pub(super) fn inject_release_error(session: &str) {
    injected_release_errors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session.to_owned());
}

#[cfg(test)]
fn take_injected_release_error(session: &str) -> bool {
    injected_release_errors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(session)
}

#[cfg(test)]
pub(super) fn release_pauses(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Barrier>>> {
    static PAUSES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Barrier>>>,
    > = std::sync::OnceLock::new();
    PAUSES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
fn release_pause_for(session: &str) -> Option<Arc<tokio::sync::Barrier>> {
    release_pauses()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session)
        .cloned()
}

#[cfg(test)]
pub(super) fn release_after_tombstone_pauses(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Barrier>>> {
    static PAUSES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Barrier>>>,
    > = std::sync::OnceLock::new();
    PAUSES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
fn release_after_tombstone_pause_for(session: &str) -> Option<Arc<tokio::sync::Barrier>> {
    release_after_tombstone_pauses()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session)
        .cloned()
}
