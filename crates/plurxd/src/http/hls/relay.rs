use super::*;

pub(super) async fn relay_if_remote(
    state: &AppState,
    session_id: &str,
    resource: RelayResource,
    headers: RelayHeaders,
    request_deadline: Instant,
) -> Result<Option<Response>, ApiError> {
    let media_resource = !matches!(resource, RelayResource::Status | RelayResource::Delete);
    let resolution = if media_resource {
        state
            .media_sessions
            .media_route_resolution_before(session_id, &state.node_id, request_deadline)
            .await?
    } else {
        state
            .media_sessions
            .route_resolution_before(session_id, &state.node_id, request_deadline)
            .await?
    };
    let mut route = match resolution {
        DurableRouteResolution::Absent | DurableRouteResolution::ActiveLocal(_) => return Ok(None),
        DurableRouteResolution::OwnerTransition(_) | DurableRouteResolution::Terminal(_) => {
            let resolution = if media_resource {
                state
                    .media_sessions
                    .authoritative_media_route_resolution_before(
                        session_id,
                        &state.node_id,
                        request_deadline,
                    )
                    .await?
            } else {
                state
                    .media_sessions
                    .authoritative_route_resolution_before(
                        session_id,
                        &state.node_id,
                        request_deadline,
                    )
                    .await?
            };
            match resolution {
                DurableRouteResolution::Absent => return Err(ApiError::NotFound("hls session")),
                DurableRouteResolution::ActiveLocal(_) => return Ok(None),
                DurableRouteResolution::ActiveRemote(route) => route,
                DurableRouteResolution::OwnerTransition(lost) => {
                    return Err(owner_transition_answer(&lost))
                }
                DurableRouteResolution::Terminal(_) => return Err(media_session_ended()),
            }
        }
        DurableRouteResolution::ActiveRemote(route) => route,
    };
    for attempt in 0..2 {
        let deadline_unix_ms =
            resource_deadline_unix_ms(request_deadline).ok_or_else(response_publication_timeout)?;
        let response = state
            .media_sessions
            .relay(
                &route.owner_node_id,
                &RelayRequest {
                    session_id: session_id.to_owned(),
                    resource: resource.clone(),
                    deadline_unix_ms,
                    // The relay envelope has no peer capability version.
                    // Until both ends negotiate validator-aware Range, every
                    // relayed request downgrades to a complete representation.
                    headers: headers.for_unversioned_peer(),
                },
            )
            .await
            .map_err(|error| {
                ApiError::ServiceUnavailable(format!("media worker relay unavailable: {error:?}"))
            })?;
        let peer_status = response.status();
        if !relay_status_requires_reclassification(peer_status) {
            return Ok(Some(response));
        }
        let resolution = if media_resource {
            state
                .media_sessions
                .authoritative_media_route_resolution_before(
                    session_id,
                    &state.node_id,
                    request_deadline,
                )
                .await?
        } else {
            state
                .media_sessions
                .authoritative_route_resolution_before(session_id, &state.node_id, request_deadline)
                .await?
        };
        match resolution {
            DurableRouteResolution::Absent if peer_status == StatusCode::NOT_FOUND => {
                return Ok(Some(response));
            }
            DurableRouteResolution::Absent => return Err(ApiError::NotFound("hls session")),
            DurableRouteResolution::ActiveLocal(_) => return Ok(None),
            DurableRouteResolution::OwnerTransition(lost) => {
                return Err(owner_transition_answer(&lost))
            }
            DurableRouteResolution::Terminal(_) if peer_status == StatusCode::GONE => {
                return Ok(Some(response));
            }
            DurableRouteResolution::Terminal(_) => return Err(media_session_ended()),
            DurableRouteResolution::ActiveRemote(next_route) if attempt == 0 => {
                drop(response);
                route = next_route;
            }
            DurableRouteResolution::ActiveRemote(_) => return Err(media_owner_transition()),
        }
    }
    unreachable!("bounded relay reclassification returns on every branch")
}

pub(super) fn relay_status_requires_reclassification(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::CONFLICT | StatusCode::GONE
    )
}

/// Execute an authenticated relay on the owning worker without performing a
/// second route lookup that could proxy back to the ingress node.
pub(crate) async fn relay_local(state: &AppState, request: RelayRequest) -> Response {
    let Some(request_deadline) = inherited_resource_deadline(&request) else {
        return response_publication_timeout().into_response();
    };
    let (playlist_deadline, _) = playlist_request_deadlines_before(state, request_deadline);
    let result = match request.resource {
        RelayResource::Status => {
            status_local_before_with_relay(state, &request.session_id, request_deadline, false)
                .await
        }
        RelayResource::Playlist { native, subtitle } => {
            playlist_local_before(
                state,
                &request.session_id,
                PlaylistQuery {
                    native,
                    subtitle,
                    diagnostic: None,
                },
                playlist_deadline,
                request_deadline,
            )
            .await
        }
        RelayResource::Master {
            subtitle,
            diagnostic,
        } => {
            master_playlist_response_local_before(
                state,
                &request.session_id,
                PlaylistQuery {
                    native: None,
                    subtitle,
                    diagnostic,
                },
                playlist_deadline,
                request_deadline,
            )
            .await
        }
        RelayResource::VideoPlaylist => {
            video_playlist_local_before(
                state,
                &request.session_id,
                "video.m3u8",
                playlist_deadline,
                request_deadline,
            )
            .await
        }
        RelayResource::SubtitlePlaylist { index } => {
            subtitle_playlist_local_before(
                state,
                &request.session_id,
                index,
                playlist_deadline,
                request_deadline,
            )
            .await
        }
        RelayResource::SubtitleSegment { index, segment } => {
            subtitle_vtt_local_before(
                state,
                &request.session_id,
                index,
                &segment,
                request_deadline,
            )
            .await
        }
        RelayResource::Segment { segment } => {
            segment_local_before(
                state,
                &request.session_id,
                &segment,
                &request.headers,
                request_deadline,
            )
            .await
        }
        RelayResource::Delete => {
            // Mixed-version peers may still send this compatibility shape.
            // It must join the same durable first-writer transaction as the
            // public endpoint; a process-local 204 would allow the active row
            // to be takeover-claimed later.
            return release_with_slots(
                state.clone(),
                request.session_id.clone(),
                session_release_slots(),
                request_deadline,
                crate::vodserve::Terminal::Deleted,
                "released by client",
            )
            .await
            .into_response();
        }
    };
    match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
