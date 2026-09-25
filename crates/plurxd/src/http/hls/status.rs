use super::*;

pub(super) async fn status_local_before(
    state: &AppState,
    session: &str,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    status_local_before_with_relay(state, session, request_deadline, true).await
}

pub(super) async fn status_local_before_with_relay(
    state: &AppState,
    session: &str,
    request_deadline: Instant,
    relay_new_owner: bool,
) -> Result<Response, ApiError> {
    // Durable authority is classified before touching process-local
    // telemetry. Random/ended/remote capabilities therefore cannot make a
    // lingering actor do work or leak whether it still exists.
    let resolution = state
        .media_sessions
        .authoritative_route_resolution_before(session, &state.node_id, request_deadline)
        .await
        .map_err(|_| response_publication_timeout())?;
    let mut route = match resolution {
        DurableRouteResolution::Absent => return Err(ApiError::NotFound("hls session")),
        DurableRouteResolution::Terminal(_) => return Err(media_session_ended()),
        DurableRouteResolution::OwnerTransition(lost) => {
            return Err(owner_transition_answer(&lost))
        }
        DurableRouteResolution::ActiveRemote(_) if !relay_new_owner => {
            return Err(ApiError::Conflict(
                "media owner changed during status lookup".to_owned(),
            ))
        }
        DurableRouteResolution::ActiveRemote(route) => route,
        DurableRouteResolution::ActiveLocal(admitted_route) => {
            #[cfg(test)]
            record_status_telemetry_lookup(session);
            let local_publication = tokio::time::timeout_at(
                tokio::time::Instant::from_std(request_deadline),
                state.transcode.hls_session_status_publication(session),
            )
            .await
            .map_err(|_| response_publication_timeout())?;
            match state
                .media_sessions
                .authoritative_route_resolution_before(session, &state.node_id, request_deadline)
                .await
                .map_err(|_| response_publication_timeout())?
            {
                DurableRouteResolution::ActiveLocal(current)
                    if same_route_authority(&admitted_route, &current) =>
                {
                    let publication = local_publication.ok_or_else(media_owner_transition)?;
                    authorize_attempt_status(
                        state,
                        session,
                        &publication.owner,
                        "status",
                        None,
                        request_deadline,
                    )
                    .await?;
                    return publication
                        .result
                        .ok()
                        .map(|info| Json(info).into_response())
                        .ok_or_else(media_owner_transition);
                }
                DurableRouteResolution::ActiveRemote(route) if relay_new_owner => route,
                DurableRouteResolution::ActiveRemote(_) => {
                    return Err(ApiError::Conflict(
                        "media owner changed during status lookup".to_owned(),
                    ));
                }
                DurableRouteResolution::Absent => {
                    return Err(ApiError::NotFound("hls session"));
                }
                DurableRouteResolution::Terminal(_) => return Err(media_session_ended()),
                DurableRouteResolution::OwnerTransition(lost) => {
                    return Err(owner_transition_answer(&lost));
                }
                // Reached from inside the outer `ActiveLocal` branch after
                // `same_route_authority` failed: this node was already the
                // owner and its incarnation or epoch moved under the read.
                // That is a genuine transition and stays retryable.
                DurableRouteResolution::ActiveLocal(_) => {
                    return Err(media_owner_transition());
                }
            }
        }
    };

    for attempt in 0..2 {
        let deadline_unix_ms =
            resource_deadline_unix_ms(request_deadline).ok_or_else(response_publication_timeout)?;
        let response = state
            .media_sessions
            .relay(
                &route.owner_node_id,
                &RelayRequest {
                    session_id: session.to_owned(),
                    resource: RelayResource::Status,
                    deadline_unix_ms,
                    headers: RelayHeaders::default(),
                },
            )
            .await
            .map_err(|error| {
                ApiError::ServiceUnavailable(format!(
                    "media worker status relay unavailable: {error:?}"
                ))
            })?;
        let peer_status = response.status();
        if !relay_status_requires_reclassification(peer_status) {
            return Ok(response);
        }
        let resolution = state
            .media_sessions
            .authoritative_route_resolution_before(session, &state.node_id, request_deadline)
            .await
            .map_err(|_| response_publication_timeout())?;
        match resolution {
            DurableRouteResolution::Absent if peer_status == StatusCode::NOT_FOUND => {
                return Ok(response);
            }
            DurableRouteResolution::Absent => return Err(ApiError::NotFound("hls session")),
            DurableRouteResolution::Terminal(_) if peer_status == StatusCode::GONE => {
                return Ok(response);
            }
            DurableRouteResolution::Terminal(_) => return Err(media_session_ended()),
            DurableRouteResolution::OwnerTransition(lost) => {
                return Err(owner_transition_answer(&lost))
            }
            // Reached from `ActiveRemote`: this node took ownership between
            // the relay and its reclassification. Also a genuine transition.
            DurableRouteResolution::ActiveLocal(_) => return Err(media_owner_transition()),
            DurableRouteResolution::ActiveRemote(next_route) if attempt == 0 => {
                drop(response);
                route = next_route;
            }
            DurableRouteResolution::ActiveRemote(_) => return Err(media_owner_transition()),
        }
    }
    unreachable!("bounded status relay reclassification returns on every branch")
}

fn same_route_authority(left: &MediaSessionRoute, right: &MediaSessionRoute) -> bool {
    left.incarnation_id == right.incarnation_id
        && left.owner_node_id == right.owner_node_id
        && left.owner_epoch == right.owner_epoch
}

#[cfg(test)]
pub(super) fn status_telemetry_observers(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Arc<std::sync::atomic::AtomicUsize>>>
{
    static OBSERVERS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Arc<std::sync::atomic::AtomicUsize>>>,
    > = std::sync::OnceLock::new();
    OBSERVERS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
fn record_status_telemetry_lookup(session: &str) {
    if let Some(observer) = status_telemetry_observers()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session)
        .cloned()
    {
        observer.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}
