//! Source direct play: preparation by current Source authority and the actual
//! decision engine, and the admitted byte exchange. Admission, claim,
//! assignment, lease and End are the parent's Source session machinery.
use super::*;
use crate::http::{
    hls::PreparedSourcePlayback,
    sharing_direct_wire::{
        planned_body_length, source_direct_envelope, DirectByteRequest, SourceDirectStart,
        DIRECT_MIMES, DIRECT_PRESENTATION, PLANNED_LENGTH_HEADER,
    },
    stream::{file_range_head, plan_file_range, FileRangePlan},
};
use crate::transcode::source_actor::{direct::SourceDirectFile, SourceViewerActor};
use plurx_core::playback_principal::PlaybackPrincipal;

/// Prepared by current Source authority and the actual decision engine with
/// the player's real capabilities. No wire constructor; carries no admission.
pub(crate) struct PreparedSourceDirect {
    target: SourcePlaybackTarget,
    principal: PlaybackPrincipal,
    playback_id: String,
    request_id: String,
    fingerprint: String,
    invocation_fingerprint: Option<String>,
    file: plurx_core::domain::MediaFile,
    object_version: String,
    length: u64,
    mime: &'static str,
    recipe_json: String,
}
impl PreparedSourceDirect {
    pub(crate) fn matches_assignment(
        &self,
        assignment: &plurx_core::sharing_source_sessions::SourceDispatchAssignment,
    ) -> bool {
        let binding = assignment.binding();
        binding.principal() == &self.principal
            && binding.source_server_id() == self.target.server_id
            && binding.catalogue_epoch() == self.target.catalogue_epoch
            && binding.library_id() == &self.target.library_id
            && binding.item_id() == &self.target.item_id
            && binding.file_id() == &self.target.file_id
            && binding.file_revision() == &self.target.revision
            && binding.playback_id() == self.playback_id
            && binding.request_id() == self.request_id
            && self.invocation_fingerprint.as_deref() == Some(binding.request_fingerprint())
    }
    pub(crate) fn into_direct_file(self) -> SourceDirectFile {
        SourceDirectFile::new(
            self.file,
            self.object_version,
            self.length,
            self.mime,
            self.recipe_json,
        )
    }
}

/// The Source durable request identity of a direct start: a distinct domain
/// over the complete typed client recipe, so the same request ID can never
/// replay as HLS, and different capabilities are different intent.
fn direct_fingerprint(session: &CreateSession) -> Result<String, SourceStartFailure> {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"plurx.sharing-source-direct.v1\0");
    digest.update(serde_json::to_vec(session).map_err(|_| SourceStartFailure::Unavailable)?);
    Ok(format!("{:x}", digest.finalize()))
}

/// A never trusts B: recompute the decision with the player's real caps and
/// refuse unless it is direct play of this exact current file.
pub(super) async fn prepare_source_direct(
    state: &crate::state::AppState,
    headers: &HeaderMap,
    target: SourcePlaybackTarget,
    session: CreateSession,
) -> Result<PreparedSourceDirect, SourceStartFailure> {
    use plurx_core::{
        sharing_catalogue_details::CatalogueRevisionKey,
        store::sharing_catalogue_details::SourceDetailsRead,
    };
    let refused = SourceStartFailure::Unavailable;
    let playback_ok = !session.playback_id.trim().is_empty()
        && session.playback_id.len() <= 128
        && !session
            .playback_id
            .bytes()
            .any(|byte| matches!(byte, b'\r' | b'\n' | 0));
    if !crate::sharing::enabled(state.store.as_ref())
        .await
        .map_err(|_| refused)?
        || target.server_id.is_nil()
        || target.catalogue_epoch.is_nil()
        || !playback_ok
        || session
            .request_id
            .as_deref()
            .is_none_or(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
        || session.presentation.as_deref() != Some(DIRECT_PRESENTATION)
    {
        return Err(refused);
    }
    // Direct play delivers the original bytes: there is nothing to burn,
    // render or re-time on the Source.
    if session.subtitle_burn.is_some() || session.native_subtitles == Some(true) {
        return Err(SourceStartFailure::Unsupported);
    }
    let caps = session
        .caps
        .as_ref()
        .filter(|caps| caps.v == plurx_core::playback::DeviceCaps::VERSION && !caps.is_empty())
        .cloned()
        .ok_or(refused)?;
    super::super::stream::validate_device_caps(&caps).map_err(|_| refused)?;
    let (hash, grant) = super::super::shared_library::authority(state, headers)
        .await
        .map_err(|_| refused)?;
    let viewer = headers
        .get("cinemashare-viewer")
        .and_then(|value| value.to_str().ok())
        .ok_or(refused)?;
    let principal = PlaybackPrincipal::sharing(grant, viewer).map_err(|_| refused)?;
    let read_witness = || {
        state.store.source_item_file_witness(
            &hash,
            grant,
            target.item_id.clone(),
            target.file_id.clone(),
        )
    };
    let SourceDetailsRead::Authorized(witness) = read_witness().await.map_err(|_| refused)? else {
        return Err(refused);
    };
    if !witness.matches_source_file(
        target.server_id,
        target.catalogue_epoch,
        &target.library_id,
        &target.item_id,
        &target.file_id,
    ) {
        return Err(refused);
    }
    let envelope = state
        .store
        .source_catalogue_revision_key(target.server_id, target.catalogue_epoch)
        .await
        .map_err(|_| refused)?
        .ok_or(refused)?;
    let key = CatalogueRevisionKey::open(
        &state.sharing.key,
        plurx_core::sharing::SharingIdentity {
            server_id: target.server_id,
            catalogue_epoch: target.catalogue_epoch,
            created_at_ms: 0,
        },
        &envelope,
    )
    .map_err(|_| refused)?;
    if key.file_revision(&witness).map_err(|_| refused)? != target.revision {
        return Err(refused);
    }
    // The ordinary file store is entered only after the grant query produced
    // this exact Source tuple. Foreign B IDs never reach it.
    let file_id = target
        .file_id
        .as_str()
        .parse::<i64>()
        .map_err(|_| refused)?;
    let snapshot = state
        .store
        .playback_planning_snapshot(file_id, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await
        .map_err(|_| refused)?
        .ok_or(refused)?;
    let file = snapshot.file.clone();
    let q = super::super::stream::Caps {
        caps_v2: Some(caps),
        audio: session.audio,
        subtitle: session.subtitle,
        audio_offset_ms: session.audio_offset_ms,
        force: session.overrides.as_ref().and_then(|o| o.force.clone()),
        ..Default::default()
    };
    let decision = super::super::stream::decision_for_source_file(state, file.clone(), q)
        .await
        .map_err(|_| refused)?;
    if decision.decision.method != plurx_core::playback::PlaybackMethod::DirectPlay {
        return Err(SourceStartFailure::Unsupported);
    }
    let mime = super::super::stream::file_content_type(&file.path);
    if !DIRECT_MIMES.contains(&mime) {
        return Err(SourceStartFailure::Unsupported);
    }
    // The planned object: a regular file opened without following links whose
    // size and mtime are the scanner's. Every later open must be this object.
    let fence = crate::fragment_index_cluster::open_source_playback_fence(&file, None)
        .await
        .map_err(|_| refused)?;
    let length = fence.handle.metadata().map_err(|_| refused)?.len();
    let object_version = fence.object_version().to_owned();
    drop(fence);
    let SourceDetailsRead::Authorized(current) = read_witness().await.map_err(|_| refused)? else {
        return Err(refused);
    };
    if !current.matches_source_file(
        target.server_id,
        target.catalogue_epoch,
        &target.library_id,
        &target.item_id,
        &target.file_id,
    ) || key.file_revision(&current).map_err(|_| refused)? != target.revision
        || !crate::sharing::enabled(state.store.as_ref())
            .await
            .map_err(|_| refused)?
        || super::super::shared_library::authority(state, headers)
            .await
            .map_err(|_| refused)?
            != (hash, grant)
    {
        return Err(refused);
    }
    let fingerprint = direct_fingerprint(&session)?;
    let recipe_json = serde_json::to_string(
        &serde_json::json!({"presentation":DIRECT_PRESENTATION,"session":&session}),
    )
    .map_err(|_| refused)?;
    Ok(PreparedSourceDirect {
        target,
        principal,
        playback_id: session.playback_id.clone(),
        request_id: session.request_id.clone().ok_or(refused)?,
        fingerprint,
        invocation_fingerprint: None,
        file,
        object_version,
        length,
        mime,
        recipe_json,
    })
}

/// One prepared Source start of either presentation. The claim, assignment
/// and activation authority between preparation and the owner are shared.
pub(super) enum PreparedSourceStart {
    Hls(Box<PreparedSourcePlayback>),
    Direct(Box<PreparedSourceDirect>),
}
impl PreparedSourceStart {
    pub(super) fn bind_invocation(
        &mut self,
        binding: &plurx_core::sharing_source_sessions::SourceBindingHandle,
    ) -> Result<(), SourceStartFailure> {
        if self.principal() != binding.principal() || self.playback_id() != binding.playback_id() {
            return Err(SourceStartFailure::Unresolved);
        }
        match self {
            Self::Hls(prepared) => prepared
                .bind_source_invocation(binding)
                .map_err(|_| SourceStartFailure::Unresolved),
            Self::Direct(prepared) => {
                let target = &prepared.target;
                if binding.request_id() != prepared.request_id
                    || binding.source_server_id() != target.server_id
                    || binding.catalogue_epoch() != target.catalogue_epoch
                    || binding.library_id() != &target.library_id
                    || binding.item_id() != &target.item_id
                    || binding.file_id() != &target.file_id
                    || binding.file_revision() != &target.revision
                {
                    return Err(SourceStartFailure::Unresolved);
                }
                prepared.invocation_fingerprint = Some(binding.request_fingerprint().to_owned());
                Ok(())
            }
        }
    }
    pub(super) fn principal(&self) -> &PlaybackPrincipal {
        match self {
            Self::Hls(prepared) => prepared.principal(),
            Self::Direct(prepared) => &prepared.principal,
        }
    }
    pub(super) fn fingerprint(&self) -> &str {
        match self {
            Self::Hls(prepared) => prepared.fingerprint(),
            Self::Direct(prepared) => prepared
                .invocation_fingerprint
                .as_deref()
                .unwrap_or(&prepared.fingerprint),
        }
    }
    pub(super) fn playback_id(&self) -> &str {
        match self {
            Self::Hls(prepared) => &prepared.request().playback_id,
            Self::Direct(prepared) => &prepared.playback_id,
        }
    }
}
pub(super) async fn prepare_source_start(
    state: &crate::state::AppState,
    headers: &HeaderMap,
    reference: SourcePlaybackTarget,
    session: CreateSession,
) -> Result<PreparedSourceStart, SourceStartFailure> {
    if session.presentation.as_deref() == Some(DIRECT_PRESENTATION) {
        return Box::pin(prepare_source_direct(state, headers, reference, session))
            .await
            .map(|prepared| PreparedSourceStart::Direct(Box::new(prepared)));
    }
    let prepared = Box::pin(super::super::hls::prepare_source_playback(
        state, headers, reference, session,
    ))
    .await
    .map_err(|error| match error {
        // The shared planner's own refusal of a burn that would cost this
        // session its HDR grade: a definite answer, not an outage.
        ApiError::Unprocessable(_) => SourceStartFailure::Unsupported,
        _ => SourceStartFailure::Unavailable,
    })?;
    // The delivery policy is decided here, after a fresh claim, from the
    // Source's own prepared request (the player's real caps through the
    // shared planner) and its own scanned file facts. B is never trusted.
    match crate::transcode::source_actor::source_delivery_refusal(
        prepared.request(),
        prepared.file(),
        prepared.native_subtitles(),
    ) {
        Some(crate::transcode::source_actor::SourceDeliveryRefusal::DolbyVision) => {
            Err(SourceStartFailure::DolbyVisionUnsupported)
        }
        Some(crate::transcode::source_actor::SourceDeliveryRefusal::Unsupported) => {
            Err(SourceStartFailure::Unsupported)
        }
        None => Ok(PreparedSourceStart::Hls(Box::new(prepared))),
    }
}
pub(super) async fn start_prepared_worker(
    state: std::sync::Arc<crate::state::AppState>,
    assignment: plurx_core::sharing_source_sessions::SourceDispatchAssignment,
    activation: plurx_core::sharing_source_sessions::SourceSessionWriteAuthority,
    prepared: PreparedSourceStart,
    deadline: std::time::Instant,
) -> Result<SourceViewerActor, crate::transcode::source_actor::SourceWorkerNoAdmission> {
    let manager = std::sync::Arc::clone(&state.transcode);
    match prepared {
        PreparedSourceStart::Hls(prepared) => {
            manager
                .start_source_worker(state, assignment, activation, *prepared, deadline)
                .await
        }
        PreparedSourceStart::Direct(prepared) => {
            manager.start_source_direct_worker(state, assignment, activation, *prepared, deadline)
        }
    }
}

fn observe_direct_published(
    entry: &SourceStartEntry,
    owned: &SourceStartOwned,
    start: &SourceDirectStart,
) -> Result<(), SourceStartFailure> {
    let session_id = canonical_v4(&start.session_id).map_err(|_| SourceStartFailure::Unresolved)?;
    if start.control_epoch <= 0 {
        return Err(SourceStartFailure::Unresolved);
    }
    let actual = SourcePublishedLineage {
        incarnation_id: owned.assignment.binding().incarnation_id(),
        session_id,
        control_epoch: start.control_epoch,
    };
    let mut published = entry.published.lock().expect("actual Source publication");
    if published.as_ref().is_some_and(|old| old != &actual) {
        return Err(SourceStartFailure::Conflict);
    }
    *published = Some(actual);
    Ok(())
}

/// The published direct envelope for Start and live status, held by the
/// owner's response guard. Status renews the lease, never viewer activity.
pub(super) async fn published_reply(
    state: &crate::state::AppState,
    headers: &HeaderMap,
    entry: &SourceStartEntry,
    owned: &SourceStartOwned,
    target: &SourcePlaybackTarget,
    grant: Uuid,
    deadline: std::time::Instant,
) -> Result<axum::response::Response, ApiError> {
    let (start, guard) = owned
        .actor
        .open_direct_start(deadline)
        .await
        .map_err(|e| SourceStartFailure::from(e).response())?;
    let (_, current_grant) = current_reference(state, headers, target).await?;
    if current_grant != grant || entry.grant != grant {
        return Err(unavailable());
    }
    observe_direct_published(entry, owned, &start).map_err(SourceStartFailure::response)?;
    let response = super::super::shared_library::source_file_json(
        grant,
        target,
        source_direct_envelope(target, owned.assignment.binding().incarnation_id(), &start),
    )?;
    Ok(hold_start_body(response, guard))
}

fn parse_direct_request(
    bytes: &[u8],
    item: &str,
    file: &str,
    request: &str,
) -> Result<(SourceOperationInput, DirectByteRequest), ApiError> {
    if bytes.is_empty() || bytes.len() > 128 * 1024 {
        return Err(invalid());
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let mut value = super::super::sharing_decision_decode::bounded_decision_value(&mut decoder)
        .map_err(|_| invalid())?;
    decoder.end().map_err(|_| invalid())?;
    let object = value.as_object_mut().ok_or_else(invalid)?;
    let demand: DirectByteRequest =
        serde_json::from_value(object.remove("direct").ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
    demand.headers().map_err(|_| invalid())?;
    let operation = parse_operation_request(
        &serde_json::to_vec(&value).map_err(|_| invalid())?,
        item,
        file,
        request,
    )?;
    if operation.known.is_none()
        || operation.start.session.presentation.as_deref() != Some(DIRECT_PRESENTATION)
    {
        return Err(invalid());
    }
    Ok((operation, demand))
}

/// POST `/sharing/v1/items/{item}/files/{file}/sessions/{request}/direct`:
/// one admitted direct byte request on the exact published lineage. The
/// planner and header set are Local direct play's; the body is the owned
/// Source reader behind the owner's revocable response guard.
pub(super) async fn direct_bytes(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    headers: HeaderMap,
    axum::extract::Path((item, file, request)): axum::extract::Path<(String, String, String)>,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    #[cfg(test)] read_gate: Option<axum::Extension<std::sync::Arc<SourceReadJobGate>>>,
    body: axum::body::Body,
) -> Result<axum::response::Response, ApiError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    let bytes = axum::body::to_bytes(body, 128 * 1024)
        .await
        .map_err(|_| invalid())?;
    let (input, demand) = parse_direct_request(&bytes, &item, &file, &request)?;
    let viewer = viewer_hash(&headers)?;
    let (hash, grant) = current_reference(&state, &headers, &input.start.reference).await?;
    let entry = state
        .transcode
        .source_http_starts
        .current_entry(grant, &hash, &viewer, &input.start)
        .map_err(SourceStartFailure::response)?;
    entry
        .validate_known(input.known.as_ref())
        .map_err(SourceStartFailure::response)?;
    let owned = entry
        .wait(deadline)
        .await
        .map_err(SourceStartFailure::response)?;
    if !owned.actor.is_direct() {
        return Err(SourceStartFailure::Unsupported.response());
    }
    let opened = owned
        .actor
        .open_direct(deadline)
        .await
        .map_err(|e| SourceStartFailure::from(e).response())?;
    let (_, current_grant) = current_reference(&state, &headers, &input.start.reference).await?;
    if current_grant != grant {
        return Err(unavailable());
    }
    let (file, length, mime, guard) = opened.into_parts();
    let guard = std::sync::Arc::new(guard);
    let method = demand.method();
    let plan = plan_file_range(&demand.headers().map_err(|_| invalid())?, &method, length);
    let (status, planned) = file_range_head(plan, length, mime);
    let body_length = planned_body_length(plan, &method, length);
    let body = match plan {
        FileRangePlan::Partial { start, .. } => Some(start),
        FileRangePlan::Full if body_length > 0 => Some(0),
        _ => None,
    };
    let body = match body {
        Some(start) => {
            source_file_body(
                tokio::fs::File::from_std(file),
                std::sync::Arc::clone(&guard),
                body_length,
                Some(start),
                #[cfg(test)]
                read_gate.map(|gate| gate.0),
            )
            .await?
        }
        None => {
            drop(file);
            axum::body::Body::empty()
        }
    };
    let mut response = axum::response::Response::new(body);
    *response.status_mut() = status;
    let known = input.known.as_ref().ok_or_else(invalid)?;
    let mut fields = vec![
        (
            axum::http::HeaderName::from_static("cinemashare-reference"),
            serde_json::to_string(&input.start.reference).map_err(|_| invalid())?,
        ),
        (
            axum::http::HeaderName::from_static("cinemashare-request-id"),
            input.start.request_id.to_string(),
        ),
        (
            axum::http::HeaderName::from_static("cinemashare-incarnation-id"),
            known.incarnation_id.to_string(),
        ),
        (
            axum::http::HeaderName::from_static("cinemashare-session-id"),
            known.session_id.to_string(),
        ),
        (
            axum::http::HeaderName::from_static("cinemashare-control-epoch"),
            known.control_epoch.to_string(),
        ),
        (axum::http::header::CONTENT_LENGTH, body_length.to_string()),
    ];
    for (name, value) in planned {
        if name == axum::http::header::CONTENT_LENGTH {
            fields.push((
                axum::http::HeaderName::from_static(PLANNED_LENGTH_HEADER),
                value,
            ));
        } else {
            fields.push((name, value));
        }
    }
    for (name, value) in fields {
        response
            .headers_mut()
            .insert(name, value.parse().map_err(|_| unavailable())?);
    }
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    super::super::shared_library::attach_source_file_authority(
        &mut response,
        grant,
        &input.start.reference,
    );
    Ok(super::super::shared_library::guard_source_response(
        state,
        connection.map(|c| c.0),
        hold_source_body(response, guard),
    )
    .await)
}

#[cfg(test)]
#[path = "shared_source_direct_tests.rs"]
mod tests;
