//! Private Source HLS start coordination. Wire data is never producer authority.
use super::{
    error::ApiError,
    hls::{CreateSession, SourcePlaybackTarget},
};
use axum::http::{HeaderMap, StatusCode};
use serde_json::Value;
use uuid::Uuid;

#[path = "shared_source_direct.rs"]
pub(crate) mod direct;

struct SourceStartInput {
    reference: SourcePlaybackTarget,
    session: CreateSession,
    request_id: Uuid,
    // The canonical complete client recipe is retained for exact retry identity;
    // the actual engine's normalized fingerprint is separately bound at prepare.
    canonical_recipe: Vec<u8>,
}
#[derive(Default)]
pub(crate) struct SourceStartRegistry {
    entries: std::sync::Mutex<Vec<std::sync::Arc<SourceStartEntry>>>,
    settled: std::sync::Mutex<std::collections::VecDeque<std::sync::Arc<SourceStartEntry>>>,
}
struct SourceStartEntry {
    identity: SourceStartIdentity,
    grant: Uuid,
    viewer: String,
    authenticated_hashes: std::sync::Mutex<Vec<String>>,
    published: std::sync::Mutex<Option<SourcePublishedLineage>>,
    ending: std::sync::Mutex<Option<std::sync::Arc<SourceEndOwner>>>,
    changed: tokio::sync::Notify,
    result: std::sync::Mutex<Option<Result<SourceStartOwned, SourceStartFailure>>>,
    task: SourceStartTask,
}
// These are retained observations of this exact task, not wire authority or a
// negative-admission proof. In particular a joined task or failed Store write
// cannot prove that a historical/competing assignment has no physical owner.
#[derive(Default)]
struct SourceStartTask {
    stage: std::sync::Mutex<SourceStartTaskStage>,
    joined: std::sync::Mutex<Option<SourceStartTaskJoined>>,
}
#[derive(Default)]
enum SourceStartTaskStage {
    #[default]
    Registered,
    Preparing,
    Prepared,
    ReadingIntent {
        planned_incarnation: Uuid,
    },
    IntentReady {
        planned_incarnation: Uuid,
        _intent: plurx_core::sharing_source_sessions::SourceSessionIntent,
    },
    Claiming {
        planned_incarnation: Uuid,
        // Preserve the actual opaque prepared intent across an unknown claim
        // outcome, even though the HTTP layer cannot turn it into authority.
        _intent: plurx_core::sharing_source_sessions::SourceSessionIntent,
    },
    Acquired(plurx_core::sharing_source_sessions::SourceBindingHandle),
    Assigning(plurx_core::sharing_source_sessions::SourceBindingHandle),
    Assigned(plurx_core::sharing_source_sessions::SourceDispatchAssignment),
    Activating(plurx_core::sharing_source_sessions::SourceDispatchAssignment),
    InvokingFactory(plurx_core::sharing_source_sessions::SourceDispatchAssignment),
}
enum SourceStartTaskJoined {
    Returned,
    PanickedOrCancelled,
}
impl SourceStartTaskStage {
    fn incarnation(&self) -> Option<Uuid> {
        match self {
            Self::Registered | Self::Preparing | Self::Prepared => None,
            Self::ReadingIntent {
                planned_incarnation,
            }
            | Self::IntentReady {
                planned_incarnation,
                ..
            }
            | Self::Claiming {
                planned_incarnation,
                ..
            } => Some(*planned_incarnation),
            Self::Acquired(binding) | Self::Assigning(binding) => Some(binding.incarnation_id()),
            Self::Assigned(assignment)
            | Self::Activating(assignment)
            | Self::InvokingFactory(assignment) => Some(assignment.binding().incarnation_id()),
        }
    }
}
impl SourceStartEntry {
    fn retain_stage(&self, stage: SourceStartTaskStage) {
        *self.task.stage.lock().expect("Source owned start stage") = stage;
    }
    fn start_owned_task(
        self: &std::sync::Arc<Self>,
        state: std::sync::Arc<crate::state::AppState>,
        headers: HeaderMap,
        input: SourceStartInput,
        deadline: std::time::Instant,
    ) {
        // The detached supervisor owns the actual JoinHandle. Waiter loss
        // cannot drop this task or replace its retained claim/assignment stage.
        // Publish an outcome only after this exact worker future has joined.
        let entry = std::sync::Arc::clone(self);
        let worker_entry = std::sync::Arc::clone(self);
        let grant = self.grant;
        let worker = tokio::spawn(async move {
            Box::pin(own_start(
                state,
                headers,
                input,
                grant,
                deadline,
                worker_entry,
            ))
            .await
        });
        tokio::spawn(async move {
            let (result, joined) = match worker.await {
                Ok(result) => (result, SourceStartTaskJoined::Returned),
                Err(_) => (
                    Err(SourceStartFailure::Unresolved),
                    SourceStartTaskJoined::PanickedOrCancelled,
                ),
            };
            if let Ok(owned) = &result {
                debug_assert_eq!(
                    entry
                        .task
                        .stage
                        .lock()
                        .expect("Source start stage")
                        .incarnation(),
                    Some(owned.assignment.binding().incarnation_id())
                );
            }
            *entry.task.joined.lock().expect("actual Source task join") = Some(joined);
            *entry.result.lock().expect("Source HTTP outcome") = Some(result);
            entry.changed.notify_waiters();
        });
    }
}
struct SourceStartIdentity {
    owner_key: String,
    request_id: Uuid,
    reference: SourcePlaybackTarget,
    recipe_hash: [u8; 32],
}
/// The typed refusal of a shared Dolby Vision delivery, on the Source and on
/// B's own Start validation alike.
pub(crate) const SHARING_START_DOLBY_VISION_UNSUPPORTED: &str =
    "sharing_start_dolby_vision_unsupported";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceStartFailure {
    Unavailable,
    Capacity,
    Conflict,
    Unresolved,
    Unsupported,
    /// A Dolby Vision delivery this Source does not build yet: refused with
    /// its own typed code before any claim, so a viewer is told why.
    DolbyVisionUnsupported,
    Deadline,
}
impl SourceStartRegistry {
    // This object is installed on one actual manager. No global pointer map,
    // rotating credential hash or user-supplied authority keys a retained task.
    fn register(
        &self,
        grant: Uuid,
        viewer: &str,
        input: &SourceStartInput,
        authenticated_hash: &str,
    ) -> Result<(std::sync::Arc<SourceStartEntry>, bool), SourceStartFailure> {
        use sha2::Digest;
        let principal = plurx_core::playback_principal::PlaybackPrincipal::sharing(grant, viewer)
            .map_err(|_| SourceStartFailure::Unavailable)?;
        let identity = SourceStartIdentity {
            owner_key: principal.owner_key(),
            request_id: input.request_id,
            reference: input.reference.clone(),
            recipe_hash: sha2::Sha256::digest(&input.canonical_recipe).into(),
        };
        let mut entries = self.entries.lock().expect("Source HTTP starts");
        // Bookkeeping capacity follows in-process ownership. An entry leaves
        // it once nothing here still works for it: a joined start that failed
        // before any actor existed, or an actor that has finished. Its receipt
        // stays in the bounded settled cache for replay and End lookup.
        let mut settled = self.settled.lock().expect("Source settled HTTP receipts");
        entries.retain(|entry| {
            if entry.actual_finished() {
                settled.push_back(std::sync::Arc::clone(entry));
                while settled.len() > 64 {
                    settled.pop_front();
                }
                false
            } else {
                true
            }
        });
        if let Some(entry) = entries.iter().chain(settled.iter()).find(|entry| {
            entry.identity.owner_key == identity.owner_key
                && entry.identity.request_id == identity.request_id
        }) {
            if entry.identity.reference != identity.reference
                || entry.identity.recipe_hash != identity.recipe_hash
            {
                return Err(SourceStartFailure::Conflict);
            }
            entry.remember_authenticated_hash(authenticated_hash)?;
            return Ok((std::sync::Arc::clone(entry), false));
        }
        if entries.len() >= 8 {
            return Err(SourceStartFailure::Capacity);
        }
        let entry = std::sync::Arc::new(SourceStartEntry {
            identity,
            grant,
            viewer: viewer.to_owned(),
            authenticated_hashes: std::sync::Mutex::new(Vec::new()),
            published: std::sync::Mutex::new(None),
            ending: std::sync::Mutex::new(None),
            changed: tokio::sync::Notify::new(),
            result: std::sync::Mutex::new(None),
            task: SourceStartTask::default(),
        });
        entry.remember_authenticated_hash(authenticated_hash)?;
        entries.push(std::sync::Arc::clone(&entry));
        Ok((entry, true))
    }
}

#[derive(Clone, PartialEq, Eq)]
struct SourcePublishedLineage {
    incarnation_id: Uuid,
    session_id: Uuid,
    control_epoch: i64,
}
#[derive(Clone, serde::Serialize)]
struct SourceEndReceipt {
    reference: SourcePlaybackTarget,
    request_id: Uuid,
    incarnation_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    control_epoch: Option<i64>,
    state: &'static str,
    confirmation_id: Uuid,
    settled: bool,
}
struct SourceEndOwner {
    changed: tokio::sync::Notify,
    result: std::sync::Mutex<Option<Result<SourceEndReceipt, SourceStartFailure>>>,
}
impl SourceEndOwner {
    async fn wait(
        &self,
        deadline: std::time::Instant,
    ) -> Result<SourceEndReceipt, SourceStartFailure> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(result) = self.result.lock().expect("Source End outcome").clone() {
                return result;
            }
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), changed)
                .await
                .map_err(|_| SourceStartFailure::Deadline)?;
        }
    }
}
impl SourceStartEntry {
    fn actual_settled(&self) -> bool {
        self.result
            .lock()
            .expect("Source HTTP outcome")
            .as_ref()
            .is_some_and(|result| {
                result
                    .as_ref()
                    .is_ok_and(|owned| owned.actor.settlement_status() == Some(Ok(())))
            })
    }
    /// Nothing in this process still owns work for this start. The outcome
    /// is published only after the worker joined, so a failure handed no
    /// actor to this entry; durable claim and assignment rows it may have
    /// left are reclaimed by their own lease expiry. A started actor counts
    /// once it has finished settlement, successfully or not.
    fn actual_finished(&self) -> bool {
        self.result
            .lock()
            .expect("Source HTTP outcome")
            .as_ref()
            .is_some_and(|result| match result {
                Ok(owned) => owned.actor.settlement_final(),
                Err(_) => true,
            })
    }
    fn remember_authenticated_hash(&self, hash: &str) -> Result<(), SourceStartFailure> {
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(SourceStartFailure::Unavailable);
        }
        let mut hashes = self
            .authenticated_hashes
            .lock()
            .expect("Source authenticated hashes");
        if !hashes.iter().any(|h| h == hash) {
            if hashes.len() >= 8 {
                return Err(SourceStartFailure::Capacity);
            }
            hashes.push(hash.to_owned());
        }
        Ok(())
    }
    fn observe_published(
        &self,
        owned: &SourceStartOwned,
        response: &super::hls::StartResponse,
    ) -> Result<(), SourceStartFailure> {
        let session_id =
            canonical_v4(&response.session_id).map_err(|_| SourceStartFailure::Unresolved)?;
        let control = response
            .control
            .as_ref()
            .ok_or(SourceStartFailure::Unresolved)?;
        let incarnation_id = owned.assignment.binding().incarnation_id();
        let control_epoch = i64::try_from(control.control_epoch)
            .ok()
            .filter(|e| *e > 0)
            .ok_or(SourceStartFailure::Unresolved)?;
        if control.generation != incarnation_id.to_string() {
            return Err(SourceStartFailure::Unresolved);
        }
        let actual = SourcePublishedLineage {
            incarnation_id,
            session_id,
            control_epoch,
        };
        let mut published = self.published.lock().expect("actual Source publication");
        if published.as_ref().is_some_and(|old| old != &actual) {
            return Err(SourceStartFailure::Conflict);
        }
        *published = Some(actual);
        Ok(())
    }
    fn validate_known(
        &self,
        known: Option<&SourcePublishedLineage>,
    ) -> Result<(), SourceStartFailure> {
        if let Some(known) = known {
            let actual = self.published.lock().expect("actual Source publication");
            if actual.as_ref() != Some(known) {
                return Err(SourceStartFailure::Conflict);
            }
        }
        Ok(())
    }
    fn end(self: &std::sync::Arc<Self>) -> std::sync::Arc<SourceEndOwner> {
        let mut ending = self.ending.lock().expect("Source End owner");
        if let Some(owner) = ending.as_ref() {
            // The actor retains its actual physical proof and retries failed
            // SQL settlement. A prior error can recover only when that same
            // actor now reports actual terminal success. Pending/unknown and
            // successful receipts keep their exact original owner.
            let failed = matches!(
                *owner.result.lock().expect("Source End outcome"),
                Some(Err(_))
            );
            if !failed || !self.actual_settled() {
                return std::sync::Arc::clone(owner);
            }
        }
        let owner = std::sync::Arc::new(SourceEndOwner {
            changed: tokio::sync::Notify::new(),
            result: std::sync::Mutex::new(None),
        });
        *ending = Some(std::sync::Arc::clone(&owner));
        let entry = std::sync::Arc::clone(self);
        let task = std::sync::Arc::clone(&owner);
        // Insertion and detached spawn precede every wait/retirement await.
        // Disconnect loses only the HTTP waiter, never this actual obligation.
        tokio::spawn(async move {
            let result = async {
                let owned = entry
                    .wait(std::time::Instant::now() + std::time::Duration::from_secs(305))
                    .await?;
                owned
                    .actor
                    .retire()
                    .await
                    .map_err(SourceStartFailure::from)?;
                if owned.actor.settlement_status() != Some(Ok(())) {
                    return Err(SourceStartFailure::Unresolved);
                }
                let published = entry
                    .published
                    .lock()
                    .expect("actual Source publication")
                    .clone();
                // This nonce identifies one actual terminal receipt. It is
                // created once, after physical/body/SQL settlement, never from
                // an absent worker, timeout or client-supplied acknowledgement.
                Ok(SourceEndReceipt {
                    reference: entry.identity.reference.clone(),
                    request_id: entry.identity.request_id,
                    incarnation_id: owned.assignment.binding().incarnation_id(),
                    session_id: published.as_ref().map(|p| p.session_id),
                    control_epoch: published.as_ref().map(|p| p.control_epoch),
                    state: "settled",
                    confirmation_id: Uuid::new_v4(),
                    settled: true,
                })
            }
            .await;
            *task.result.lock().expect("Source End outcome") = Some(result);
            task.changed.notify_waiters();
        });
        owner
    }
}
impl SourceStartRegistry {
    fn current_entry(
        &self,
        grant: Uuid,
        hash: &str,
        viewer: &str,
        input: &SourceStartInput,
    ) -> Result<std::sync::Arc<SourceStartEntry>, SourceStartFailure> {
        use sha2::Digest;
        let recipe_hash: [u8; 32] = sha2::Sha256::digest(&input.canonical_recipe).into();
        let entries = self.entries.lock().expect("Source HTTP starts");
        let settled = self.settled.lock().expect("Source settled HTTP receipts");
        let entry = entries
            .iter()
            .chain(settled.iter())
            .find(|entry| {
                entry.grant == grant
                    && entry.viewer == viewer
                    && entry.identity.request_id == input.request_id
            })
            .ok_or(SourceStartFailure::Unresolved)?;
        if entry.identity.reference != input.reference || entry.identity.recipe_hash != recipe_hash
        {
            return Err(SourceStartFailure::Conflict);
        }
        entry.remember_authenticated_hash(hash)?;
        Ok(std::sync::Arc::clone(entry))
    }
    fn cleanup_entry(
        &self,
        hash: &str,
        viewer: &str,
        input: &SourceStartInput,
    ) -> Result<std::sync::Arc<SourceStartEntry>, SourceStartFailure> {
        use sha2::Digest;
        let recipe_hash: [u8; 32] = sha2::Sha256::digest(&input.canonical_recipe).into();
        let entries = self.entries.lock().expect("Source HTTP starts");
        let settled = self.settled.lock().expect("Source settled HTTP receipts");
        let entry = entries
            .iter()
            .chain(settled.iter())
            .find(|entry| {
                entry.viewer == viewer
                    && entry.identity.request_id == input.request_id
                    && entry
                        .authenticated_hashes
                        .lock()
                        .expect("Source authenticated hashes")
                        .iter()
                        .any(|h| h == hash)
            })
            .ok_or(SourceStartFailure::Unresolved)?;
        if entry.identity.reference != input.reference || entry.identity.recipe_hash != recipe_hash
        {
            return Err(SourceStartFailure::Conflict);
        }
        Ok(std::sync::Arc::clone(entry))
    }
}
struct SourceOperationInput {
    start: SourceStartInput,
    known: Option<SourcePublishedLineage>,
}
fn canonical_v4(value: &str) -> Result<Uuid, ApiError> {
    let id = Uuid::parse_str(value).map_err(|_| invalid())?;
    if id.get_version_num() != 4 || id.to_string() != value {
        return Err(invalid());
    }
    Ok(id)
}
fn parse_operation_request(
    bytes: &[u8],
    item: &str,
    file: &str,
    request: &str,
) -> Result<SourceOperationInput, ApiError> {
    if bytes.is_empty() || bytes.len() > 128 * 1024 {
        return Err(invalid());
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = super::sharing_decision_decode::bounded_decision_value(&mut decoder)
        .map_err(|_| invalid())?;
    decoder.end().map_err(|_| invalid())?;
    let object = value.as_object().ok_or_else(invalid)?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "reference" | "session" | "incarnation_id" | "session_id" | "control_epoch"
        )
    }) {
        return Err(invalid());
    }
    let recipe=serde_json::to_vec(&serde_json::json!({"reference":object.get("reference").ok_or_else(invalid)?,"session":object.get("session").ok_or_else(invalid)?})).map_err(|_|invalid())?;
    let start = parse_start_request(&recipe, item, file)?;
    if canonical_v4(request)? != start.request_id {
        return Err(invalid());
    }
    let known = match (
        object.get("incarnation_id"),
        object.get("session_id"),
        object.get("control_epoch"),
    ) {
        (None, None, None) => None,
        (Some(inc), Some(sid), Some(epoch)) => Some(SourcePublishedLineage {
            incarnation_id: canonical_v4(inc.as_str().ok_or_else(invalid)?)?,
            session_id: canonical_v4(sid.as_str().ok_or_else(invalid)?)?,
            control_epoch: epoch.as_i64().filter(|e| *e > 0).ok_or_else(invalid)?,
        }),
        _ => return Err(invalid()),
    };
    Ok(SourceOperationInput { start, known })
}
// Live telemetry/control always requires the exact already-published lineage.
// Unlike End, neither operation may fall back to retained cleanup identity.
fn parse_live_operation(
    bytes: &[u8],
    item: &str,
    file: &str,
    request: &str,
    with_control: bool,
) -> Result<
    (
        SourceOperationInput,
        Option<crate::playback_control::ControlRequestV1>,
    ),
    ApiError,
> {
    if bytes.is_empty() || bytes.len() > 128 * 1024 {
        return Err(invalid());
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = super::sharing_decision_decode::bounded_decision_value(&mut decoder)
        .map_err(|_| invalid())?;
    decoder.end().map_err(|_| invalid())?;
    let mut object = value.as_object().cloned().ok_or_else(invalid)?;
    let control = if with_control {
        Some(
            serde_json::from_value(object.remove("control").ok_or_else(invalid)?)
                .map_err(|_| invalid())?,
        )
    } else {
        None
    };
    let operation = parse_operation_request(
        &serde_json::to_vec(&object).map_err(|_| invalid())?,
        item,
        file,
        request,
    )?;
    let known = operation.known.as_ref().ok_or_else(invalid)?;
    if control
        .as_ref()
        .is_some_and(|control: &crate::playback_control::ControlRequestV1| {
            control.generation != known.incarnation_id.to_string()
                || i64::try_from(control.control_epoch).ok() != Some(known.control_epoch)
        })
    {
        return Err(invalid());
    }
    Ok((operation, control))
}
async fn live_operation_owner(
    state: &crate::state::AppState,
    headers: &HeaderMap,
    input: &SourceOperationInput,
    deadline: std::time::Instant,
) -> Result<(std::sync::Arc<SourceStartEntry>, SourceStartOwned), ApiError> {
    let viewer = viewer_hash(headers)?;
    let (hash, grant) = current_reference(state, headers, &input.start.reference).await?;
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
    Ok((entry, owned))
}
async fn live_operation_response(
    state: crate::state::AppState,
    headers: &HeaderMap,
    input: SourceOperationInput,
    entry: &SourceStartEntry,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    content: (
        &'static str,
        Value,
        crate::transcode::source_actor::SourceResponseGuard,
    ),
) -> Result<axum::response::Response, ApiError> {
    let (field, value, guard) = content;
    let (_, grant) = current_reference(&state, headers, &input.start.reference).await?;
    if grant != entry.grant {
        return Err(unavailable());
    }
    entry
        .validate_known(input.known.as_ref())
        .map_err(SourceStartFailure::response)?;
    let known = input.known.ok_or_else(invalid)?;
    let mut envelope = serde_json::json!({
        "reference": input.start.reference, "request_id": input.start.request_id,
        "incarnation_id": known.incarnation_id, "session_id": known.session_id,
        "control_epoch": known.control_epoch,
    });
    envelope[field] = value;
    let response =
        super::shared_library::source_file_json(grant, &input.start.reference, envelope)?;
    Ok(super::shared_library::guard_source_response(
        state,
        connection.map(|c| c.0),
        hold_start_body(response, guard),
    )
    .await)
}
async fn vod_status(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    headers: HeaderMap,
    axum::extract::Path((item, file, request)): axum::extract::Path<(String, String, String)>,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    body: axum::body::Body,
) -> Result<axum::response::Response, ApiError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(9);
    let bytes = axum::body::to_bytes(body, 128 * 1024)
        .await
        .map_err(|_| invalid())?;
    let (input, _) = parse_live_operation(&bytes, &item, &file, &request, false)?;
    let (entry, owned) = live_operation_owner(&state, &headers, &input, deadline).await?;
    let (status, guard) = owned
        .actor
        .open_status(deadline)
        .await
        .map_err(SourceStartFailure::from)
        .map_err(SourceStartFailure::response)?
        .into_parts();
    live_operation_response(
        state,
        &headers,
        input,
        &entry,
        connection,
        (
            "status",
            serde_json::to_value(status).map_err(|_| unavailable())?,
            guard,
        ),
    )
    .await
}
async fn control(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    headers: HeaderMap,
    axum::extract::Path((item, file, request)): axum::extract::Path<(String, String, String)>,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    body: axum::body::Body,
) -> Result<axum::response::Response, ApiError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    let bytes = axum::body::to_bytes(body, 128 * 1024)
        .await
        .map_err(|_| invalid())?;
    let (input, control) = parse_live_operation(&bytes, &item, &file, &request, true)?;
    let request = control.ok_or_else(invalid)?;
    let (entry, owned) = live_operation_owner(&state, &headers, &input, deadline).await?;
    // A dropped HTTP waiter cannot discard an accepted actor exchange or its
    // nested observations. The exact owned task retains the physical guard.
    let command = request.clone();
    let task = tokio::spawn(Box::pin(async move {
        owned.actor.control(command, deadline).await
    }));
    let opened = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), task)
        .await
        .map_err(|_| SourceStartFailure::Deadline.response())?
        .map_err(|_| SourceStartFailure::Unresolved.response())?
        .map_err(SourceStartFailure::from)
        .map_err(SourceStartFailure::response)?;
    let (response, guard) = opened.into_response(&request);
    // An authenticated exchange the actor definitively refused is answered
    // with its closed refusal code, never collapsed into a transport status:
    // B must tell an ended session from a stale fence or a rate limit.
    let (field, value) = match response {
        Ok(response) => (
            "response",
            serde_json::to_value(response).map_err(|_| unavailable())?,
        ),
        Err(error) => (
            "refusal",
            serde_json::to_value(
                super::sharing_playback_wire::SharedControlRefusal::from_state(error),
            )
            .map_err(|_| unavailable())?,
        ),
    };
    live_operation_response(
        state,
        &headers,
        input,
        &entry,
        connection,
        (field, value, guard),
    )
    .await
}
async fn status(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    headers: HeaderMap,
    axum::extract::Path((item, file, request)): axum::extract::Path<(String, String, String)>,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    body: axum::body::Body,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(9);
    let bytes = axum::body::to_bytes(body, 128 * 1024)
        .await
        .map_err(|_| invalid())?;
    let input = parse_operation_request(&bytes, &item, &file, &request)?;
    let viewer = viewer_hash(&headers)?;
    let credential = super::sharing::credential(&headers)?;
    let hash =
        plurx_core::sharing::secret_hash(plurx_core::sharing::SecretDomain::Grant, &credential);
    let current = current_reference(&state, &headers, &input.start.reference).await;
    let entry = match current.as_ref() {
        Ok((current_hash, current_grant)) => state.transcode.source_http_starts.current_entry(
            *current_grant,
            current_hash,
            &viewer,
            &input.start,
        ),
        Err(_) => state
            .transcode
            .source_http_starts
            .cleanup_entry(&hash, &viewer, &input.start),
    }
    .map_err(SourceStartFailure::response)?;
    entry
        .validate_known(input.known.as_ref())
        .map_err(SourceStartFailure::response)?;
    let owned = entry
        .wait(deadline)
        .await
        .map_err(SourceStartFailure::response)?;
    if let (Ok((_, current_grant)), true) = (current.as_ref(), owned.actor.is_direct()) {
        if let Ok(response) = direct::published_reply(
            &state,
            &headers,
            &entry,
            &owned,
            &input.start.reference,
            *current_grant,
            deadline,
        )
        .await
        {
            return Ok(super::shared_library::guard_source_response(
                state,
                connection.map(|c| c.0),
                response,
            )
            .await);
        }
    }
    if current.is_ok() {
        if let Ok((response, guard)) = owned.actor.open_start_response(deadline).await {
            entry
                .observe_published(&owned, &response)
                .map_err(SourceStartFailure::response)?;
            // The same actual full Start envelope/guard is used for live status;
            // neither stored JSON nor cleanup authority can establish readiness.
            let (_, current_grant) =
                current_reference(&state, &headers, &input.start.reference).await?;
            if current_grant != entry.grant {
                return Err(unavailable());
            }
            let response = super::shared_library::source_file_json(
                entry.grant,
                &input.start.reference,
                serde_json::json!({"reference":input.start.reference,"incarnation_id":owned.assignment.binding().incarnation_id(),"response":response}),
            )?;
            return Ok(super::shared_library::guard_source_response(
                state,
                connection.map(|c| c.0),
                hold_start_body(response, guard),
            )
            .await);
        }
    }
    // Revoked grants get no live metadata, resources or readiness. The exact
    // retained cleanup identity may observe only genuine terminal settlement.
    let published = entry
        .published
        .lock()
        .expect("actual Source publication")
        .clone();
    let mut facts = serde_json::json!({"reference":entry.identity.reference,"request_id":entry.identity.request_id,"incarnation_id":owned.assignment.binding().incarnation_id(),"state":if owned.actor.settlement_status()==Some(Ok(())){"settled"}else{"unresolved"}});
    if let Some(published) = published {
        facts["session_id"] = serde_json::json!(published.session_id);
        facts["control_epoch"] = serde_json::json!(published.control_epoch);
    }
    Ok(axum::Json(facts).into_response())
}
async fn end(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    headers: HeaderMap,
    axum::extract::Path((item, file, request)): axum::extract::Path<(String, String, String)>,
    body: axum::body::Body,
) -> Result<axum::Json<SourceEndReceipt>, ApiError> {
    let bytes = axum::body::to_bytes(body, 128 * 1024)
        .await
        .map_err(|_| invalid())?;
    let input = parse_operation_request(&bytes, &item, &file, &request)?;
    let viewer = viewer_hash(&headers)?;
    let credential = super::sharing::credential(&headers)?;
    let hash =
        plurx_core::sharing::secret_hash(plurx_core::sharing::SecretDomain::Grant, &credential);
    // Cleanup authenticates only the exact previously owned obligation. It
    // grants no resource/status content permission and reads no expired grant.
    let entry = state
        .transcode
        .source_http_starts
        .cleanup_entry(&hash, &viewer, &input.start)
        .map_err(SourceStartFailure::response)?;
    entry
        .validate_known(input.known.as_ref())
        .map_err(SourceStartFailure::response)?;
    let owner = entry.end();
    let receipt = owner
        .wait(std::time::Instant::now() + std::time::Duration::from_secs(305))
        .await
        .map_err(SourceStartFailure::response)?;
    Ok(axum::Json(receipt))
}

#[derive(Clone)]
struct SourceStartOwned {
    actor: crate::transcode::source_actor::SourceViewerActor,
    assignment: plurx_core::sharing_source_sessions::SourceDispatchAssignment,
}
impl From<crate::transcode::source_actor::SourceWorkerError> for SourceStartFailure {
    fn from(value: crate::transcode::source_actor::SourceWorkerError) -> Self {
        use crate::transcode::source_actor::SourceWorkerError as E;
        match value {
            E::Unavailable => Self::Unavailable,
            E::Capacity => Self::Capacity,
            E::Conflict => Self::Conflict,
            E::Deadline => Self::Deadline,
            E::Unresolved => Self::Unresolved,
            E::Unsupported => Self::Unsupported,
        }
    }
}
impl SourceStartFailure {
    fn response(self) -> ApiError {
        let (status, code) = match self {
            Self::Conflict => (StatusCode::CONFLICT, "sharing_start_conflict"),
            Self::Capacity => (StatusCode::TOO_MANY_REQUESTS, "sharing_start_capacity"),
            Self::Unsupported => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "sharing_start_unsupported",
            ),
            Self::DolbyVisionUnsupported => (
                StatusCode::UNPROCESSABLE_ENTITY,
                SHARING_START_DOLBY_VISION_UNSUPPORTED,
            ),
            Self::Unresolved => (StatusCode::SERVICE_UNAVAILABLE, "sharing_start_unresolved"),
            Self::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "sharing_start_unavailable"),
            Self::Deadline => (StatusCode::SERVICE_UNAVAILABLE, "sharing_start_deadline"),
        };
        ApiError::typed(status, code, "Shared start is unavailable")
    }
}
impl SourceStartEntry {
    async fn wait(
        &self,
        deadline: std::time::Instant,
    ) -> Result<SourceStartOwned, SourceStartFailure> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(result) = self.result.lock().expect("Source HTTP outcome").clone() {
                return result;
            }
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), changed)
                .await
                .map_err(|_| SourceStartFailure::Deadline)?;
        }
    }
}
pub(crate) fn peer_router(state: crate::state::AppState) -> axum::Router<crate::state::AppState> {
    axum::Router::new()
        .route(
            "/sharing/v1/items/{item}/files/{file}/sessions",
            axum::routing::post(start),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state,
            super::shared_library::source_content_guard,
        ))
        .merge(
            axum::Router::new()
                .route(
                    "/sharing/v1/items/{item}/files/{file}/sessions/{request}/end",
                    axum::routing::post(end),
                )
                .route(
                    "/sharing/v1/items/{item}/files/{file}/sessions/{request}/status",
                    axum::routing::post(status),
                )
                .route(
                    "/sharing/v1/items/{item}/files/{file}/sessions/{request}/vod-status",
                    axum::routing::post(vod_status),
                )
                .route(
                    "/sharing/v1/items/{item}/files/{file}/sessions/{request}/control",
                    axum::routing::post(control),
                )
                .route(
                    "/sharing/v1/items/{item}/files/{file}/sessions/{request}/resources",
                    axum::routing::post(resources),
                )
                .route(
                    "/sharing/v1/items/{item}/files/{file}/sessions/{request}/direct",
                    axum::routing::post(direct::direct_bytes),
                ),
        )
}
#[cfg(test)]
#[derive(Default)]
pub(crate) struct SourceReadJobGate {
    pub(crate) entered: std::sync::atomic::AtomicBool,
    pub(crate) complete: std::sync::atomic::AtomicBool,
    pub(crate) file_closed: std::sync::atomic::AtomicBool,
    released: std::sync::Mutex<bool>,
    release: std::sync::Condvar,
}
#[cfg(test)]
impl SourceReadJobGate {
    fn park(&self) -> std::io::Result<()> {
        let mut released = self.released.lock().expect("read job gate");
        self.entered
            .store(true, std::sync::atomic::Ordering::SeqCst);
        while !*released {
            let (next, timeout) = self
                .release
                .wait_timeout(released, std::time::Duration::from_secs(30))
                .expect("read gate wait");
            released = next;
            if timeout.timed_out() && !*released {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "test read gate abandoned",
                ));
            }
        }
        Ok(())
    }
    pub(crate) fn release(&self) {
        *self.released.lock().expect("read gate") = true;
        self.release.notify_all();
    }
}
struct SourceReadFile {
    file: Option<std::fs::File>,
    #[cfg(test)]
    gate: Option<std::sync::Arc<SourceReadJobGate>>,
}
impl Drop for SourceReadFile {
    fn drop(&mut self) {
        // The owned OS file is really closed before the barrier credit is
        // returned by the enclosing reader's later guard-field drop.
        drop(self.file.take());
        #[cfg(test)]
        if let Some(gate) = &self.gate {
            gate.file_closed
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

// An owned read job retains its actual Source barrier independently of the
// waiting Body future. File drops before guard when the last owner goes away.
struct SourceFileReader {
    file: std::sync::Mutex<SourceReadFile>,
    guard: std::sync::Arc<crate::transcode::source_actor::SourceResponseGuard>,
}
static SOURCE_READ_JOBS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(8)));
async fn source_file_body(
    file: tokio::fs::File,
    guard: std::sync::Arc<crate::transcode::source_actor::SourceResponseGuard>,
    len: u64,
    start: Option<u64>,
    #[cfg(test)] gate: Option<std::sync::Arc<SourceReadJobGate>>,
) -> Result<axum::body::Body, ApiError> {
    let conversion_guard = std::sync::Arc::clone(&guard);
    // Tokio may already own an outstanding blocking operation. The conversion
    // task is detached from waiter cancellation and holds the real barrier until
    // into_std has actually joined it; dropping a JoinHandle is not a join.
    let conversion = tokio::spawn(async move {
        let mut file = file.into_std().await;
        if let Some(start) = start {
            // A seek is an lseek on the owned descriptor: no disk wait.
            std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(start))?;
        }
        Ok::<_, std::io::Error>(std::sync::Arc::new(SourceFileReader {
            file: std::sync::Mutex::new(SourceReadFile {
                file: Some(file),
                #[cfg(test)]
                gate,
            }),
            guard: conversion_guard,
        }))
    });
    let reader = tokio::select! {biased;
        ()=guard.cancelled()=>return Err(unavailable()),
        result=conversion=>result.map_err(|_|unavailable())?.map_err(|_|unavailable())?,
    };
    let stream = futures_util::stream::try_unfold(
        (reader, len),
        |(reader, remaining)| async move {
            if remaining == 0 {
                return Ok(None);
            }
            // Wait for a read slot rather than failing a live body: a long
            // direct body and a segment share these slots. Retirement still
            // ends the wait at once.
            let permit = tokio::select! {biased;
                ()=reader.guard.cancelled()=>return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,"Source body retired")),
                permit=std::sync::Arc::clone(&SOURCE_READ_JOBS).acquire_owned()=>permit.map_err(|_| std::io::Error::other("Source read capacity"))?,
            };
            let job_reader = std::sync::Arc::clone(&reader);
            let size = remaining.min(64 * 1024) as usize;
            let job = tokio::task::spawn_blocking(move || {
                use std::io::Read;
                let _permit = permit;
                let mut bytes = vec![0; size];
                #[cfg(test)]
                let gate = job_reader.file.lock().expect("Source reader").gate.clone();
                #[cfg(test)]
                if let Some(gate) = &gate {
                    gate.park()?;
                }
                let n = job_reader
                    .file
                    .lock()
                    .expect("Source file reader")
                    .file
                    .as_mut()
                    .expect("owned FD")
                    .read(&mut bytes)?;
                #[cfg(test)]
                if let Some(gate) = &gate {
                    gate.complete
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                bytes.truncate(n);
                Ok::<_, std::io::Error>(bytes)
            });
            let bytes = tokio::select! {biased;
                ()=reader.guard.cancelled()=>return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,"Source body retired")),
                result=job=>result.map_err(std::io::Error::other)??,
            };
            if bytes.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Source resource truncated",
                ));
            }
            let remaining = remaining - bytes.len() as u64;
            Ok(Some((bytes::Bytes::from(bytes), (reader, remaining))))
        },
    );
    Ok(axum::body::Body::from_stream(stream))
}
struct SourceResourceInput {
    operation: SourceOperationInput,
    resource: plurx_core::sharing_resources::SharingHlsResource,
}
fn parse_resource_request(
    bytes: &[u8],
    item: &str,
    file: &str,
    request: &str,
) -> Result<SourceResourceInput, ApiError> {
    if bytes.is_empty() || bytes.len() > 128 * 1024 {
        return Err(invalid());
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let mut value = super::sharing_decision_decode::bounded_decision_value(&mut decoder)
        .map_err(|_| invalid())?;
    decoder.end().map_err(|_| invalid())?;
    let object = value.as_object_mut().ok_or_else(invalid)?;
    let resource = object.remove("resource").ok_or_else(invalid)?;
    let resource = plurx_core::sharing_resources::SharingHlsResource::parse(
        resource.as_str().ok_or_else(invalid)?,
    )
    .map_err(|_| invalid())?;
    let operation = parse_operation_request(
        &serde_json::to_vec(&value).map_err(|_| invalid())?,
        item,
        file,
        request,
    )?;
    if operation.known.is_none() {
        return Err(invalid());
    }
    Ok(SourceResourceInput {
        operation,
        resource,
    })
}
async fn resources(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    headers: HeaderMap,
    axum::extract::Path((item, file, request)): axum::extract::Path<(String, String, String)>,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    #[cfg(test)] read_gate: Option<axum::Extension<std::sync::Arc<SourceReadJobGate>>>,
    body: axum::body::Body,
) -> Result<axum::response::Response, ApiError> {
    use plurx_core::sharing_resources::{validate_sharing_playlist, SharingHlsResourceKind};
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    let bytes = axum::body::to_bytes(body, 128 * 1024)
        .await
        .map_err(|_| invalid())?;
    let input = parse_resource_request(&bytes, &item, &file, &request)?;
    let viewer = viewer_hash(&headers)?;
    let (hash, grant) =
        current_reference(&state, &headers, &input.operation.start.reference).await?;
    let entry = state
        .transcode
        .source_http_starts
        .current_entry(grant, &hash, &viewer, &input.operation.start)
        .map_err(SourceStartFailure::response)?;
    entry
        .validate_known(input.operation.known.as_ref())
        .map_err(SourceStartFailure::response)?;
    let owned = entry
        .wait(deadline)
        .await
        .map_err(SourceStartFailure::response)?;
    let opened = owned
        .actor
        .open_resource(&input.resource, deadline)
        .await
        .map_err(|e| SourceStartFailure::from(e).response())?;
    let (_, current_grant) =
        current_reference(&state, &headers, &input.operation.start.reference).await?;
    if current_grant != grant {
        return Err(unavailable());
    }
    let (payload, guard) = opened.into_parts();
    let guard = std::sync::Arc::new(guard);
    let (body, len, etag, mime) = match payload {
        crate::transcode::source_actor::SourceResourcePayload::Playlist(bytes) => {
            validate_sharing_playlist(&input.resource, &bytes).map_err(|_| unavailable())?;
            let len = bytes.len() as u64;
            (
                axum::body::Body::from(bytes),
                len,
                None,
                "application/vnd.apple.mpegurl",
            )
        }
        crate::transcode::source_actor::SourceResourcePayload::SubtitleText(bytes) => {
            if !matches!(
                input.resource.kind(),
                SharingHlsResourceKind::SubtitleSegment { .. }
            ) {
                return Err(unavailable());
            }
            let len = bytes.len() as u64;
            (axum::body::Body::from(bytes), len, None, "text/vtt")
        }
        crate::transcode::source_actor::SourceResourcePayload::File(ready) => {
            let mime = if input.resource.kind() == SharingHlsResourceKind::Init {
                "video/mp4"
            } else {
                "video/iso.segment"
            };
            (
                source_file_body(
                    ready.file,
                    std::sync::Arc::clone(&guard),
                    ready.len,
                    None,
                    #[cfg(test)]
                    read_gate.map(|gate| gate.0),
                )
                .await?,
                ready.len,
                Some(ready.etag),
                mime,
            )
        }
    };
    let mut response = axum::response::Response::new(body);
    let known = input.operation.known.as_ref().ok_or_else(invalid)?;
    let echoed = [
        (
            "cinemashare-reference",
            serde_json::to_string(&input.operation.start.reference).map_err(|_| invalid())?,
        ),
        (
            "cinemashare-request-id",
            input.operation.start.request_id.to_string(),
        ),
        (
            "cinemashare-incarnation-id",
            known.incarnation_id.to_string(),
        ),
        ("cinemashare-session-id", known.session_id.to_string()),
        ("cinemashare-control-epoch", known.control_epoch.to_string()),
        ("cinemashare-resource", input.resource.as_str().to_owned()),
        ("content-length", len.to_string()),
        ("content-type", mime.to_owned()),
    ];
    for (key, value) in echoed {
        response
            .headers_mut()
            .insert(key, value.parse().map_err(|_| unavailable())?);
    }
    if let Some(etag) = etag {
        response
            .headers_mut()
            .insert("etag", etag.parse().map_err(|_| unavailable())?);
    }
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    super::shared_library::attach_source_file_authority(
        &mut response,
        grant,
        &input.operation.start.reference,
    );
    Ok(super::shared_library::guard_source_response(
        state,
        connection.map(|c| c.0),
        hold_source_body(response, guard),
    )
    .await)
}

async fn start(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    headers: HeaderMap,
    axum::extract::Path((item, file)): axum::extract::Path<(String, String)>,
    body: axum::body::Body,
) -> Result<axum::response::Response, ApiError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(305);
    let bytes = axum::body::to_bytes(body, 128 * 1024)
        .await
        .map_err(|_| invalid())?;
    let input = parse_start_request(&bytes, &item, &file)?;
    validate_initial_source_start(&input.session).map_err(SourceStartFailure::response)?;
    let viewer = viewer_hash(&headers)?;
    let (authenticated_hash, grant) = current_reference(&state, &headers, &input.reference).await?;
    let target = input.reference.clone();
    let (entry, new) = state
        .transcode
        .source_http_starts
        .register(grant, &viewer, &input, &authenticated_hash)
        .map_err(SourceStartFailure::response)?;
    if new {
        // No await separates insertion and spawning the owned task. Disconnect
        // drops only a waiter; durable/physical work stays owned by this entry.
        let state = std::sync::Arc::new(state.clone());
        entry.start_owned_task(state, headers.clone(), input, deadline);
    }
    let owned = entry
        .wait(deadline)
        .await
        .map_err(SourceStartFailure::response)?;
    if owned.actor.is_direct() {
        return direct::published_reply(&state, &headers, &entry, &owned, &target, grant, deadline)
            .await;
    }
    let (response, guard) = owned
        .actor
        .open_start_response(deadline)
        .await
        .map_err(|e| SourceStartFailure::from(e).response())?;
    let (_, current_grant) = current_reference(&state, &headers, &target).await?;
    if current_grant != grant {
        return Err(unavailable());
    }
    entry
        .observe_published(&owned, &response)
        .map_err(SourceStartFailure::response)?;
    let response = super::shared_library::source_file_json(
        grant,
        &target,
        serde_json::json!({"reference":target,"incarnation_id":owned.assignment.binding().incarnation_id(),"response":response}),
    )?;
    Ok(hold_start_body(response, guard))
}
fn hold_start_body(
    response: axum::response::Response,
    guard: crate::transcode::source_actor::SourceResponseGuard,
) -> axum::response::Response {
    hold_source_body(response, std::sync::Arc::new(guard))
}
fn hold_source_body(
    response: axum::response::Response,
    guard: std::sync::Arc<crate::transcode::source_actor::SourceResponseGuard>,
) -> axum::response::Response {
    use futures_util::{FutureExt, StreamExt};
    let (mut parts, body) = response.into_parts();
    parts.extensions.insert(std::sync::Arc::clone(&guard));
    let stream = futures_util::stream::try_unfold(
        (body.into_data_stream(), guard),
        |(mut stream, guard)| async move {
            let next = tokio::select! {biased;
                ()=guard.cancelled()=>return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,"Source body retired")),
                next=stream.next()=>next,
            };
            // A parked physical read can outlive the first guard check. Poll a
            // fresh held-file/retirement check again before handing DATA out.
            if guard.cancelled().now_or_never().is_some() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Source body retired",
                ));
            }
            match next {
                Some(bytes) => Ok(Some((
                    bytes.map_err(std::io::Error::other)?,
                    (stream, guard),
                ))),
                None => Ok(None),
            }
        },
    );
    let body = axum::body::Body::from_stream(stream);
    axum::response::Response::from_parts(parts, body)
}
fn validate_initial_source_start(session: &CreateSession) -> Result<(), SourceStartFailure> {
    // No Source-safe predecessor/control factory exists yet. In particular a
    // foreign receiver UUID must never enter ordinary Local recovery lookup.
    // Retain the original complete recipe; refuse unsupported intent instead
    // of stripping fields or adapting it into an unrelated Local request.
    if session.previous_session_id.is_some()
        || session.reopen_reason.is_some()
        || session.control_sequence.is_some()
        || session.intent.is_some()
    {
        return Err(SourceStartFailure::Unsupported);
    }
    Ok(())
}
async fn own_start(
    state: std::sync::Arc<crate::state::AppState>,
    headers: HeaderMap,
    input: SourceStartInput,
    grant: Uuid,
    deadline: std::time::Instant,
    entry: std::sync::Arc<SourceStartEntry>,
) -> Result<SourceStartOwned, SourceStartFailure> {
    use plurx_core::sharing_source_sessions::{
        SourceClaimOutcome, SourceIntentRead, SourceSessionRequest, SourceWriteAuthorityRead,
    };
    validate_initial_source_start(&input.session)?;
    let reference = input.reference.clone();
    entry.retain_stage(SourceStartTaskStage::Preparing);
    // The presentation branches only here: direct play recomputes the actual
    // decision and refuses anything but direct play of this exact file.
    let prepared = Box::pin(direct::prepare_source_start(
        &state,
        &headers,
        reference.clone(),
        input.session,
    ))
    .await?;
    entry.retain_stage(SourceStartTaskStage::Prepared);
    let (hash, current_grant) = current_reference(&state, &headers, &reference)
        .await
        .map_err(|_| SourceStartFailure::Unavailable)?;
    if grant != current_grant || std::time::Instant::now() >= deadline {
        return Err(SourceStartFailure::Unavailable);
    }
    let now = crate::state::clock_ms();
    let remaining = deadline
        .saturating_duration_since(std::time::Instant::now())
        .as_millis()
        .min(305000) as i64;
    let planned_incarnation = Uuid::new_v4();
    entry.retain_stage(SourceStartTaskStage::ReadingIntent {
        planned_incarnation,
    });
    let intent = state
        .store
        .prepare_source_session_intent(
            SourceSessionRequest {
                principal: prepared.principal().clone(),
                request_id: input.request_id.to_string(),
                request_fingerprint: prepared.fingerprint().into(),
                playback_id: prepared.playback_id().to_owned(),
                incarnation_id: planned_incarnation,
                now_ms: now,
                claim_expires_at_ms: now + remaining,
                credential_hash: hash,
                item_id: reference.item_id.clone(),
                file_id: reference.file_id.clone(),
                file_revision: reference.revision.clone(),
            },
            &state.sharing.key,
        )
        .await
        .map_err(|_| SourceStartFailure::Unresolved)?;
    let intent = match intent {
        SourceIntentRead::Ready(value) => value,
        SourceIntentRead::Unavailable => return Err(SourceStartFailure::Unavailable),
        SourceIntentRead::Capacity => return Err(SourceStartFailure::Capacity),
    };
    entry.retain_stage(SourceStartTaskStage::IntentReady {
        planned_incarnation,
        _intent: (*intent).clone(),
    });
    let members = state
        .membership
        .observe_source_admission_members()
        .await
        .map_err(|_| SourceStartFailure::Unresolved)?
        .ok_or(SourceStartFailure::Unavailable)?;
    entry.retain_stage(SourceStartTaskStage::Claiming {
        planned_incarnation,
        _intent: (*intent).clone(),
    });
    let binding = match state
        .store
        .claim_source_media_session(&intent, &members)
        .await
        .map_err(|_| SourceStartFailure::Unresolved)?
    {
        SourceClaimOutcome::Acquired(binding) => binding,
        // A committed old claim/replay without this retained HTTP owner is an
        // unresolved restart outcome, never evidence of no physical activation.
        SourceClaimOutcome::InFlight(_)
        | SourceClaimOutcome::Resolved { .. }
        | SourceClaimOutcome::Retired(_) => return Err(SourceStartFailure::Unresolved),
        SourceClaimOutcome::Conflict => return Err(SourceStartFailure::Conflict),
        SourceClaimOutcome::Unavailable => return Err(SourceStartFailure::Unavailable),
        SourceClaimOutcome::Capacity(_) => return Err(SourceStartFailure::Capacity),
    };
    entry.retain_stage(SourceStartTaskStage::Acquired(binding.clone()));
    let members = state
        .membership
        .observe_source_admission_members()
        .await
        .map_err(|_| SourceStartFailure::Unresolved)?
        .ok_or(SourceStartFailure::Unresolved)?;
    entry.retain_stage(SourceStartTaskStage::Assigning(binding.clone()));
    let assignment = state
        .store
        .assign_source_dispatch(&binding, &state.sharing.key, &members)
        .await
        .map_err(|_| SourceStartFailure::Unresolved)?
        .ok_or(SourceStartFailure::Unresolved)?;
    entry.retain_stage(SourceStartTaskStage::Assigned(assignment.clone()));
    let members = state
        .membership
        .observe_source_admission_members()
        .await
        .map_err(|_| SourceStartFailure::Unresolved)?
        .ok_or(SourceStartFailure::Unresolved)?;
    entry.retain_stage(SourceStartTaskStage::Activating(assignment.clone()));
    let activation = match state
        .store
        .prepare_source_activation_authority(&assignment, &state.sharing.key, &members)
        .await
        .map_err(|_| SourceStartFailure::Unresolved)?
    {
        SourceWriteAuthorityRead::Ready(value) => *value,
        SourceWriteAuthorityRead::Unavailable => return Err(SourceStartFailure::Unresolved),
        SourceWriteAuthorityRead::Capacity => return Err(SourceStartFailure::Capacity),
    };
    entry.retain_stage(SourceStartTaskStage::InvokingFactory(assignment.clone()));
    let actor = direct::start_prepared_worker(
        std::sync::Arc::clone(&state),
        assignment.clone(),
        activation,
        prepared,
        deadline,
    )
    .await
    .map_err(SourceStartFailure::from)?;
    Ok(SourceStartOwned { actor, assignment })
}

// Read-only current authentication can precede owner insertion. It owns no
// durable or physical obligation and binds a stable grant across rotation.
async fn current_reference(
    state: &crate::state::AppState,
    headers: &HeaderMap,
    target: &SourcePlaybackTarget,
) -> Result<(String, Uuid), ApiError> {
    use plurx_core::{
        sharing::SharingIdentity, sharing_catalogue_details::CatalogueRevisionKey,
        store::sharing_catalogue_details::SourceDetailsRead,
    };
    let (hash, grant) = super::shared_library::authority(state, headers).await?;
    let SourceDetailsRead::Authorized(witness) = state
        .store
        .source_item_file_witness(&hash, grant, target.item_id.clone(), target.file_id.clone())
        .await
        .map_err(|_| unavailable())?
    else {
        return Err(unavailable());
    };
    if !witness.matches_source_file(
        target.server_id,
        target.catalogue_epoch,
        &target.library_id,
        &target.item_id,
        &target.file_id,
    ) {
        return Err(unavailable());
    }
    let envelope = state
        .store
        .source_catalogue_revision_key(target.server_id, target.catalogue_epoch)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    let key = CatalogueRevisionKey::open(
        &state.sharing.key,
        SharingIdentity {
            server_id: target.server_id,
            catalogue_epoch: target.catalogue_epoch,
            created_at_ms: 0,
        },
        &envelope,
    )
    .map_err(|_| unavailable())?;
    if key.file_revision(&witness).map_err(|_| unavailable())? != target.revision {
        return Err(unavailable());
    }
    Ok((hash, grant))
}
fn unavailable() -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "sharing_playback_authority_unavailable",
        "Shared playback authority is unavailable",
    )
}

fn invalid() -> ApiError {
    ApiError::typed(
        StatusCode::BAD_REQUEST,
        "sharing_invalid_request",
        "Invalid shared start request",
    )
}
fn parse_start_request(bytes: &[u8], item: &str, file: &str) -> Result<SourceStartInput, ApiError> {
    if bytes.is_empty() || bytes.len() > 128 * 1024 {
        return Err(invalid());
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = super::sharing_decision_decode::bounded_decision_value(&mut decoder)
        .map_err(|_| invalid())?;
    decoder.end().map_err(|_| invalid())?;
    let reference: SourcePlaybackTarget =
        serde_json::from_value(value.get("reference").cloned().ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
    super::validate_source_start_request(bytes, &reference).map_err(|_| invalid())?;
    if plurx_core::sharing::SourceId::parse(item).map_err(|_| invalid())? != reference.item_id
        || plurx_core::sharing::SourceId::parse(file).map_err(|_| invalid())? != reference.file_id
    {
        return Err(invalid());
    }
    let session_value = value.get("session").ok_or_else(invalid)?;
    let session: CreateSession =
        serde_json::from_value(session_value.clone()).map_err(|_| invalid())?;
    let typed = serde_json::to_value(&session).map_err(|_| invalid())?;
    if !closed_provided_fields(session_value, &typed) {
        return Err(invalid());
    }
    let request_id = Uuid::parse_str(session.request_id.as_deref().ok_or_else(invalid)?)
        .map_err(|_| invalid())?;
    Ok(SourceStartInput {
        reference,
        session,
        request_id,
        canonical_recipe: bytes.to_vec(),
    })
}
// Ordinary CreateSession supports omitted optional fields and explicit numeric
// JSON integers for floating time fields. Close every provided field against
// the actual complete serializer without changing that existing contract.
fn closed_provided_fields(input: &Value, typed: &Value) -> bool {
    match (input, typed) {
        (Value::Object(input), Value::Object(typed)) => input.iter().all(|(name, value)| {
            typed
                .get(name)
                .is_some_and(|field| closed_provided_fields(value, field))
        }),
        (Value::Array(input), Value::Array(typed)) => {
            input.len() == typed.len()
                && input
                    .iter()
                    .zip(typed)
                    .all(|(a, b)| closed_provided_fields(a, b))
        }
        (Value::Number(input), Value::Number(typed)) if typed.is_f64() => {
            input.as_f64() == typed.as_f64()
        }
        _ => input == typed,
    }
}
fn viewer_hash(headers: &HeaderMap) -> Result<String, ApiError> {
    let mut values = headers.get_all("cinemashare-viewer").iter();
    let value = values
        .next()
        .and_then(|v| v.to_str().ok())
        .ok_or_else(invalid)?;
    if values.next().is_some()
        || value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid());
    }
    Ok(value.to_owned())
}

#[cfg(test)]
pub(crate) struct RealSourceStartFixture {
    pub state: std::sync::Arc<crate::state::AppState>,
    pub reference: SourcePlaybackTarget,
    pub headers: HeaderMap,
    pub request: Vec<u8>,
    pub grant: Uuid,
    invitation: SourceFixtureInvitation,
    selected: plurx_core::cluster::migration::SelectedStore,
    _directory: tempfile::TempDir,
}
#[cfg(test)]
impl RealSourceStartFixture {
    /// Diagnostic count of Source media sessions still holding a slot. Row
    /// presence is not physical evidence; the B End receipt is.
    pub(crate) async fn active_source_sessions(&self) -> i64 {
        let client = self
            .selected
            .local_client()
            .expect("actual Source selected voter client");
        let rows = client
            .query_consistent(
                "SELECT CAST(COUNT(*) AS TEXT) AS payload FROM media_sessions WHERE state=$1",
                hiqlite::params!("active"),
            )
            .await
            .expect("actual Source read-only session count");
        assert_eq!(rows.len(), 1);
        let mut rows = rows;
        let count: String = rows[0].get("payload");
        count.parse().expect("decimal count")
    }
}
#[cfg(test)]
struct SourceFixtureInvitation {
    identity: plurx_core::sharing::SharingIdentity,
    id: Uuid,
    secret: plurx_core::secrets::Secret,
    expires_at_ms: i64,
}
#[cfg(test)]
impl RealSourceStartFixture {
    /// Encode the actual invitation that produced this grant with the caller's
    /// real runtime TLS endpoint. This grants no alternate approval or readiness.
    pub fn invitation_for(
        &self,
        endpoint: plurx_core::sharing::Endpoint,
    ) -> Result<plurx_core::sharing::Invitation, plurx_core::error::StoreError> {
        endpoint.validate()?;
        Ok(plurx_core::sharing::Invitation {
            identity: self.invitation.identity.clone(),
            name: self.state.server_name.clone(),
            endpoints: vec![endpoint],
            id: self.invitation.id,
            secret: plurx_core::secrets::Secret::from_cleartext(self.invitation.secret.expose()),
            expires_at_ms: self.invitation.expires_at_ms,
        })
    }
    pub async fn shutdown(self) {
        self.selected
            .shutdown()
            .await
            .expect("actual voter shutdown");
    }
}
/// PGS display sets from the fuzz corpus's real `mkpgs` capture, retimed to
/// show one bitmap from `start_ms` to `end_ms`.
#[cfg(test)]
fn source_fixture_pgs(start_ms: u32, end_ms: u32) -> Vec<u8> {
    let fixture = include_bytes!("../../../../fuzz/corpus/inspect_sup/mkpgs-1920x1080.sup");
    let mut sup = Vec::new();
    let mut cursor = 0;
    while cursor + 13 <= fixture.len() {
        assert_eq!(&fixture[cursor..cursor + 2], b"PG");
        let pts = u32::from_be_bytes(fixture[cursor + 2..cursor + 6].try_into().expect("PTS"));
        let len = usize::from(u16::from_be_bytes(
            fixture[cursor + 11..cursor + 13].try_into().expect("PGS length"),
        )) + 13;
        if pts == 90_000 || pts == 630_000 {
            let mut segment = fixture[cursor..cursor + len].to_vec();
            let at = if pts == 90_000 { start_ms } else { end_ms };
            segment[2..6].copy_from_slice(&(at * 90).to_be_bytes());
            sup.extend(segment);
        }
        cursor += len;
    }
    sup
}
#[cfg(test)]
pub(crate) fn real_source_start_fixture(
) -> std::pin::Pin<Box<dyn std::future::Future<Output = RealSourceStartFixture> + Send>> {
    real_source_start_fixture_with(SourceFixtureMode::Copy, None)
}
#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum SourceFixtureMode {
    Copy,
    Encoded,
    NativeCopy,
    NativeEncoded,
    /// The Copy fixture's MP4 started as direct play.
    Direct,
    /// A Matroska with one embedded SubRip track, started as a copy that
    /// burns it (a burn always encodes).
    BurnText,
    /// A Matroska with one embedded PGS track, started as a 144-row encode
    /// that burns it.
    BurnBitmap,
    /// A PQ-tagged HEVC Main10 source started by an SDR-only player: the
    /// Source tone-maps.
    Hdr10Sdr,
    /// A 1080p PQ-tagged HEVC Main10 source started by a player that presents
    /// PQ on HEVC Main10 and asks HDR10, on a node with the HDR10 passthrough
    /// proof: the Source encodes Main10 PQ.
    Hdr10,
    /// The Encoded fixture scanned as Dolby Vision: refused before a claim.
    DolbyVisionEncoded,
}
#[cfg(test)]
pub(crate) fn real_source_start_fixture_with(
    mode: SourceFixtureMode,
    recipient_server_id: Option<Uuid>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = RealSourceStartFixture> + Send>> {
    Box::pin(build_real_source_start_fixture(mode, recipient_server_id))
}
#[cfg(test)]
async fn build_real_source_start_fixture(
    mode: SourceFixtureMode,
    recipient_server_id: Option<Uuid>,
) -> RealSourceStartFixture {
    use plurx_core::{
        cluster::migration::select_daemon_store,
        config::Config,
        domain::{ItemKind, LibraryKind, NewItem, NewLibrary},
        sharing::{new_secret, secret_hash, InvitationRecord, SecretDomain, ShareClaim, SourceId},
        sharing_catalogue_details::CatalogueRevisionKey,
        store::{keys, sharing_catalogue_details::SourceDetailsRead},
    };
    use std::sync::Arc;
    let directory = crate::test_tempdir().expect("real Source fixture");
    let mut config = Config::default();
    config.storage.data_dir = directory.path().join("database");
    let raft = std::net::TcpListener::bind("127.0.0.1:0").expect("Raft port");
    let api = std::net::TcpListener::bind("127.0.0.1:0").expect("API port");
    config.cluster.raft_bind = raft.local_addr().expect("Raft address");
    config.cluster.api_bind = api.local_addr().expect("API address");
    config.cluster.advertise_host = "localhost".into();
    drop((raft, api));
    let mut selected = Box::pin(select_daemon_store(&config))
        .await
        .expect("actual one voter");
    selected
        .store
        .put_setting(keys::SHARING_ENABLED, "true")
        .await
        .expect("saved choice");
    // Run the real pre-serving factory before constructing State/listeners.
    // No manual candidate DDL or synthetic capability rows admit this fixture.
    assert!(Box::pin(selected.prepare_source_schema_before_serving())
        .await
        .expect("actual startup factory"));
    let client = selected.local_client().expect("local voter client");
    let mut state = Arc::new(crate::http::source_actor_test_state());
    let state_mut = Arc::get_mut(&mut state).expect("sole initial State");
    state_mut.store = Arc::clone(&selected.store);
    state_mut.membership = selected.membership_manager();
    state_mut.node_id = selected.identity.node_id.clone();
    state_mut.catalogue = selected.catalogue_reader();
    state_mut.replication = selected.replication_monitor();
    state_mut.sharing = Arc::new(crate::sharing::SharingManager::new(
        Arc::clone(&selected.credential_key),
        config.storage.data_dir.clone(),
        config.sharing.clone(),
    ));
    let store = Arc::clone(&state.store);
    store
        .put_setting(keys::SW_POOL_THREADS, "4")
        .await
        .expect("actual software budget");
    let identity = store
        .sharing_identity(crate::state::clock_ms())
        .await
        .expect("identity");
    let library = store
        .create_library(&NewLibrary {
            name: "Actual HTTP Source".into(),
            kind: LibraryKind::Movies,
            paths: vec![directory.path().to_owned()],
            anime: false,
        })
        .await
        .expect("library")
        .id;
    let item = store
        .insert_item(&NewItem {
            library_id: library,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Actual HTTP Source".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let native = matches!(
        mode,
        SourceFixtureMode::NativeCopy | SourceFixtureMode::NativeEncoded
    );
    let burn = matches!(
        mode,
        SourceFixtureMode::BurnText | SourceFixtureMode::BurnBitmap
    );
    let hdr = matches!(mode, SourceFixtureMode::Hdr10Sdr | SourceFixtureMode::Hdr10);
    let encoded = matches!(
        mode,
        SourceFixtureMode::Encoded
            | SourceFixtureMode::NativeEncoded
            | SourceFixtureMode::BurnText
            | SourceFixtureMode::BurnBitmap
            | SourceFixtureMode::Hdr10Sdr
            | SourceFixtureMode::Hdr10
            | SourceFixtureMode::DolbyVisionEncoded
    );
    let (width, height) = if matches!(mode, SourceFixtureMode::Hdr10) {
        (1920, 1080)
    } else {
        (320, 180)
    };
    let file = directory.path().join(if native || burn {
        "source.mkv"
    } else {
        "source.mp4"
    });
    // 180 rows so both recipes stay inside the v1 control contract
    // (heights 144..=2160): Copy names 180 and Encoded a real 144 encode.
    // A client controls with the rung it started, so a sub-144 fixture
    // could start but never be controlled.
    let mut generate = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin());
    generate.args(["-v", "error", "-f", "lavfi", "-i"]);
    if hdr {
        // Real HEVC Main10 with PQ/BT.2020 signalled in the bitstream, so the
        // scan, the held probe and the Source's grade all read actual facts.
        generate
            .arg(format!("testsrc2=s={width}x{height}:r=24"))
            .args([
                "-t",
                "2",
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p10le",
                "-color_primaries",
                "bt2020",
                "-color_trc",
                "smpte2084",
                "-colorspace",
                "bt2020nc",
                "-x265-params",
                "log-level=error:hdr10=1:repeat-headers=1",
                "-tag:v",
                "hvc1",
                "-y",
            ]);
    } else {
        generate.arg("color=s=320x180:r=24").args([
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-y",
        ]);
    }
    let generated = generate
        .arg(&file)
        .output()
        .await
        .expect("actual FFmpeg");
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    if native {
        let caption = directory.path().join("actual.srt");
        std::fs::write(
            &caption,
            "1\n00:00:00,200 --> 00:00:01,800\nActual HTTP Source caption\n\n",
        )
        .expect("actual subtitle input");
        let alternative = directory.path().join("alternative.srt");
        std::fs::write(
            &alternative,
            "1\n00:00:00,200 --> 00:00:01,800\nAlternative HTTP Source caption\n\n",
        )
        .expect("actual alternative subtitle input");
        let muxed = directory.path().join("captioned.mkv");
        let result = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin())
            .arg("-i")
            .arg(&file)
            .arg("-i")
            .arg(&caption)
            .arg("-i")
            .arg(&alternative)
            .args([
                "-map", "0:v:0", "-map", "1:s:0", "-map", "2:s:0", "-c:v", "copy", "-c:s",
                "subrip", "-y",
            ])
            .arg(&muxed)
            .output()
            .await
            .expect("actual subtitle mux");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        std::fs::rename(muxed, &file).expect("actual captioned Source");
    }
    if burn {
        let muxed = directory.path().join("burnable.mkv");
        let mut mux = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin());
        mux.args(["-v", "error", "-i"]).arg(&file);
        if matches!(mode, SourceFixtureMode::BurnText) {
            let caption = directory.path().join("burn.srt");
            std::fs::write(
                &caption,
                "1\n00:00:00,000 --> 00:00:02,000\nBURNED SHARED CAPTION\n\n",
            )
            .expect("actual burn subtitle");
            mux.arg("-i").arg(&caption).args(["-c:s", "subrip"]);
        } else {
            let sup = directory.path().join("burn.sup");
            std::fs::write(&sup, source_fixture_pgs(100, 1900)).expect("actual PGS display sets");
            mux.args(["-f", "sup", "-i"]).arg(&sup).args(["-c:s", "copy"]);
        }
        let result = mux
            .args(["-map", "0:v:0", "-map", "1:s:0", "-c:v", "copy", "-y"])
            .arg(&muxed)
            .output()
            .await
            .expect("actual burn mux");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        std::fs::rename(muxed, &file).expect("actual burnable Source");
    }
    let metadata = std::fs::metadata(&file).expect("actual file");
    let mtime = metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    let probe = tokio::process::Command::new(crate::ffmpeg::ffprobe_bin())
        .args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-show_chapters",
            "-of",
            "json",
        ])
        .arg(&file)
        .output()
        .await
        .expect("actual probe");
    assert!(probe.status.success());
    let native_tracks = if native || burn {
        let observed: serde_json::Value =
            serde_json::from_slice(&probe.stdout).expect("actual native scan JSON");
        let tracks: Vec<_> = observed["streams"]
            .as_array()
            .expect("actual streams")
            .iter()
            .filter(|stream| stream["codec_type"] == "subtitle")
            .collect();
        let (count, codec) = match mode {
            SourceFixtureMode::BurnText => (1, "subrip"),
            SourceFixtureMode::BurnBitmap => (1, "hdmv_pgs_subtitle"),
            _ => (2, "subrip"),
        };
        assert_eq!(tracks.len(), count, "actual generated embedded track count");
        assert!(
            tracks.iter().all(|track| track["codec_name"] == codec),
            "facts must match actual ffprobe"
        );
        Some(
            tracks
                .iter()
                .enumerate()
                .map(|(index, track)| plurx_core::domain::SubtitleStream {
                    index: index as i64,
                    codec: track["codec_name"]
                        .as_str()
                        .expect("actual codec")
                        .to_owned(),
                    language: track["tags"]["language"].as_str().map(str::to_owned),
                    title: track["tags"]["title"].as_str().map(str::to_owned),
                    default: track["disposition"]["default"].as_i64() == Some(1),
                    forced: track["disposition"]["forced"].as_i64() == Some(1),
                    hearing_impaired: track["disposition"]["hearing_impaired"].as_i64() == Some(1),
                })
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    client.execute("INSERT INTO files(id,item_id,path,size,mtime,duration_ms,container,video_codec,width,height,bit_depth,bitrate,probe_json,scanned_at) VALUES(1,$1,$2,$3,$4,2000,'mp4','h264',320,180,8,100000,$5,$6)",hiqlite::params!(item,file.to_string_lossy().to_string(),metadata.len() as i64,mtime,String::from_utf8(probe.stdout).expect("scan JSON"),crate::state::clock_ms()/1000)).await.expect("actual scanned file facts");
    if let Some(native_tracks) = native_tracks {
        let tracks = serde_json::to_string(&native_tracks).expect("actual embedded subtitle facts");
        client
            .execute(
                "UPDATE files SET container='matroska',subtitle_streams=$1 WHERE id=1",
                hiqlite::params!(tracks),
            )
            .await
            .expect("actual scanned Source subtitle");
    }
    if hdr {
        // The scan's facts for the generated HEVC Main10 PQ source.
        client
            .execute(
                "UPDATE files SET video_codec='hevc',width=$1,height=$2,bit_depth=10,hdr='hdr10' WHERE id=1",
                hiqlite::params!(width, height),
            )
            .await
            .expect("actual scanned HDR10 facts");
    }
    if matches!(mode, SourceFixtureMode::DolbyVisionEncoded) {
        client
            .execute("UPDATE files SET hdr='dolby_vision' WHERE id=1", hiqlite::params!())
            .await
            .expect("Dolby Vision scan facts");
    }
    let now = crate::state::clock_ms();
    let grant = Uuid::new_v4();
    let invitation = Uuid::new_v4();
    let invitation_secret = new_secret().expect("actual invitation secret");
    let invitation_hash = secret_hash(SecretDomain::Invitation, &invitation_secret);
    let secret = new_secret().expect("credential");
    let hash = secret_hash(SecretDomain::Grant, &secret);
    store
        .create_share_invitation(InvitationRecord {
            id: invitation,
            token_hash: invitation_hash.clone(),
            library_ids: vec![library],
            created_at_ms: now,
            expires_at_ms: now + 60000,
        })
        .await
        .expect("invite");
    store
        .claim_share(ShareClaim {
            invitation_id: invitation,
            invitation_hash,
            claim_id: Uuid::new_v4(),
            grant_id: grant,
            recipient_server_id: recipient_server_id.unwrap_or_else(Uuid::new_v4),
            recipient_name: "Actual B".into(),
            credential_hash: hash.clone(),
            now_ms: now + 1,
        })
        .await
        .expect("claim");
    store
        .approve_share(grant, 1, now + 2)
        .await
        .expect("explicit approval");
    let SourceDetailsRead::Authorized(witness) = store
        .source_item_file_witness(
            &hash,
            grant,
            SourceId::parse(&item.to_string()).expect("item"),
            SourceId::parse("1").expect("file"),
        )
        .await
        .expect("actual witness")
    else {
        panic!("authorized Source witness")
    };
    let envelope = store
        .source_catalogue_revision_key(identity.server_id, identity.catalogue_epoch)
        .await
        .expect("stored key")
        .expect("factory key");
    let key = CatalogueRevisionKey::open(&state.sharing.key, identity.clone(), &envelope)
        .expect("actual factory key");
    let reference = SourcePlaybackTarget {
        server_id: identity.server_id,
        catalogue_epoch: identity.catalogue_epoch,
        library_id: SourceId::parse(&library.to_string()).expect("library"),
        item_id: SourceId::parse(&item.to_string()).expect("item"),
        file_id: SourceId::parse("1").expect("file"),
        revision: key.file_revision(&witness).expect("actual revision"),
    };
    if !native && !encoded {
        let media = store.get_file(1).await.expect("file read").expect("file");
        let crate::fragindex::IndexOutcome::Built(index) = Box::pin(crate::fragindex::build(
            &media,
            plurx_core::transcode::CopyVideoOptions::new(false, false),
            directory.path(),
            std::time::Duration::from_secs(30),
        ))
        .await
        else {
            panic!("actual fragment scan")
        };
        store
            .put_fragment_index(1, &index)
            .await
            .expect("actual fragment scan committed");
    }
    Arc::get_mut(&mut state)
        .expect("sole State before listeners")
        .transcode = Arc::new(
        crate::transcode::TranscodeManager::new(
            Arc::clone(&store),
            directory.path().join("workers"),
            plurx_core::transcode::EncoderCaps::default(),
            plurx_core::transcode::Pipeline::Cpu,
        )
        // The boot proof the HDR10 rung needs; the planner still decides the
        // grade from the player's caps.
        .with_hdr10_passthrough(matches!(mode, SourceFixtureMode::Hdr10)),
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("CinemaShare {}", secret.expose())
            .parse()
            .expect("auth"),
    );
    headers.insert(
        "cinemashare-viewer",
        "c".repeat(64).parse().expect("viewer"),
    );
    let request=serde_json::to_vec(&serde_json::json!({"reference":reference,"session":{"playback_id":"actual-http-client","request_id":Uuid::new_v4(),"copy":true,"height":180,"quality_auto":false,"presentation":"vod","caps":{"v":2,"video":[{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"]}}})).expect("canonical complete recipe");
    let mut recipe: serde_json::Value =
        serde_json::from_slice(&request).expect("full fixture recipe");
    if encoded {
        recipe["session"]["copy"] = serde_json::json!(false);
        recipe["session"]["height"] = serde_json::json!(144);
    }
    if native {
        recipe["session"]["native_subtitles"] = serde_json::json!(true);
        recipe["session"]["subtitle"] = serde_json::json!(0);
    }
    match mode {
        SourceFixtureMode::BurnText => {
            // A copy ask that burns: the Source encodes at source height.
            recipe["session"]["copy"] = serde_json::json!(true);
            recipe["session"]["height"] = serde_json::json!(180);
            recipe["session"]["subtitle_burn"] = serde_json::json!(0);
        }
        SourceFixtureMode::BurnBitmap => {
            recipe["session"]["subtitle_burn"] = serde_json::json!(0);
        }
        SourceFixtureMode::Hdr10 => {
            recipe["session"]["height"] = serde_json::json!(1080);
            recipe["session"]["hdr10"] = serde_json::json!(true);
            recipe["session"]["caps"] = serde_json::json!({"v":2,"video":[{"codec":"hevc","profiles":["main","main10"],"max_height":2160,"present":["sdr","pq"]},{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"],"display":{"hdr":true}});
        }
        _ => {}
    }
    if matches!(mode, SourceFixtureMode::Direct) {
        let session = recipe["session"].as_object_mut().expect("session");
        session.insert("presentation".into(), serde_json::json!("direct"));
        for field in ["copy", "height", "quality_auto"] {
            session.remove(field);
        }
    }
    let request = serde_json::to_vec(&recipe).expect("canonical full fixture recipe");
    RealSourceStartFixture {
        state,
        reference,
        headers,
        request,
        grant,
        invitation: SourceFixtureInvitation {
            identity,
            id: invitation,
            secret: invitation_secret,
            expires_at_ms: now + 60000,
        },
        selected,
        _directory: directory,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    pub(super) async fn actual_resource_request(
        address: std::net::SocketAddr,
        h2: bool,
        url: &str,
        headers: HeaderMap,
        body: Vec<u8>,
    ) -> axum::response::Response {
        if !h2 {
            let client = reqwest::Client::builder()
                .http1_only()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("H1 client");
            let response = client
                .post(url)
                .headers(headers)
                .header("connection", "close")
                .body(body)
                .send()
                .await
                .expect("H1 response");
            let status = response.status();
            let headers = response.headers().clone();
            let bytes = response.bytes().await.expect("actual bytes");
            let mut response = axum::response::Response::new(axum::body::Body::from(bytes));
            *response.status_mut() = status;
            *response.headers_mut() = headers;
            return response;
        }
        let socket = tokio::net::TcpStream::connect(address)
            .await
            .expect("H2 socket");
        let (mut sender, driver) =
            hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                .handshake::<_, axum::body::Body>(hyper_util::rt::TokioIo::new(socket))
                .await
                .expect("H2 handshake");
        let driver = tokio::spawn(driver);
        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri(url)
            .body(axum::body::Body::from(body))
            .expect("H2 request");
        *request.headers_mut() = headers;
        let response = sender.send_request(request).await.expect("H2 response");
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(axum::body::Body::new(body), 4 * 1024 * 1024)
            .await
            .expect("actual bounded resource bytes");
        drop(sender);
        driver.abort();
        let _ = driver.await;
        axum::response::Response::from_parts(parts, axum::body::Body::from(bytes))
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sharing_source_http_end_recovers_only_after_actual_sql_settlement_retry() {
        Box::pin(actual_source_end_sql_recovery()).await;
    }
    async fn actual_source_end_sql_recovery() {
        use std::time::{Duration, Instant};
        let fixture = real_source_start_fixture().await;
        let response = start(
            axum::extract::State((*fixture.state).clone()),
            fixture.headers.clone(),
            axum::extract::Path((
                fixture.reference.item_id.as_str().to_owned(),
                fixture.reference.file_id.as_str().to_owned(),
            )),
            axum::body::Body::from(fixture.request.clone()),
        )
        .await
        .expect("actual Source Start");
        axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("actual body finished");
        let entry = fixture
            .state
            .transcode
            .source_http_starts
            .entries
            .lock()
            .expect("entry")
            .first()
            .cloned()
            .expect("actual entry");
        let owned = entry
            .wait(Instant::now() + Duration::from_secs(1))
            .await
            .expect("actual owned actor");
        let client = fixture
            .selected
            .local_client()
            .expect("actual selected one-voter client");
        // Inject a real transactional failure at the actual release write. No
        // Source actor flags/proofs or physical settlement results are forged.
        client.execute("CREATE TRIGGER fixture_source_release_failure BEFORE UPDATE OF reservation_state ON sharing_source_session_bindings WHEN NEW.reservation_state='released' BEGIN SELECT RAISE(ABORT,'fixture release temporarily unavailable'); END",hiqlite::params![]).await.expect("actual transient SQL failure fixture");
        let first = entry.end();
        assert_eq!(
            first
                .wait(Instant::now() + Duration::from_secs(10))
                .await
                .err(),
            Some(SourceStartFailure::Unresolved)
        );
        assert_eq!(
            owned.actor.settlement_status(),
            Some(Err(
                crate::transcode::source_actor::SourceWorkerError::Unresolved
            ))
        );
        assert!(
            std::sync::Arc::ptr_eq(&first, &entry.end()),
            "failed proof remains retained before actual successful retry"
        );
        client
            .execute(
                "DROP TRIGGER fixture_source_release_failure",
                hiqlite::params![],
            )
            .await
            .expect("restore actual SQL release");
        tokio::time::timeout(Duration::from_secs(15), async {
            while owned.actor.settlement_status() != Some(Ok(())) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("same actor physically/SQL settles through owned retry");
        let recovered = entry.end();
        assert!(!std::sync::Arc::ptr_eq(&first, &recovered));
        let receipt = recovered
            .wait(Instant::now() + Duration::from_secs(5))
            .await
            .expect("actual recovered receipt");
        assert!(receipt.settled);
        assert_eq!(
            receipt.incarnation_id,
            owned.assignment.binding().incarnation_id()
        );
        assert_eq!(receipt.confirmation_id.get_version_num(), 4);
        let replay = entry.end();
        assert!(std::sync::Arc::ptr_eq(&recovered, &replay));
        assert_eq!(
            replay
                .wait(Instant::now() + Duration::from_secs(1))
                .await
                .expect("stable confirmation")
                .confirmation_id,
            receipt.confirmation_id
        );
        fixture.shutdown().await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sharing_source_http_failed_claim_and_assignment_retain_joined_exact_task_stages() {
        Box::pin(actual_source_failed_task_stages()).await;
    }
    async fn actual_source_failed_task_stages() {
        use std::time::{Duration, Instant};
        for assignment_failure in [false, true] {
            let fixture = real_source_start_fixture().await;
            let client = fixture.selected.local_client().expect("actual voter");
            let trigger = if assignment_failure {
                "CREATE TRIGGER fixture_source_task_failure BEFORE UPDATE OF dispatch_generation ON sharing_source_session_bindings WHEN NEW.dispatch_generation=1 BEGIN SELECT RAISE(ABORT,'fixture assignment write failure'); END"
            } else {
                "CREATE TRIGGER fixture_source_task_failure BEFORE INSERT ON sharing_source_session_bindings BEGIN SELECT RAISE(ABORT,'fixture claim write failure'); END"
            };
            client
                .execute(trigger, hiqlite::params![])
                .await
                .expect("actual transactional failure");
            let invoke = || {
                start(
                    axum::extract::State((*fixture.state).clone()),
                    fixture.headers.clone(),
                    axum::extract::Path((
                        fixture.reference.item_id.as_str().to_owned(),
                        fixture.reference.file_id.as_str().to_owned(),
                    )),
                    axum::body::Body::from(fixture.request.clone()),
                )
            };
            let first = invoke()
                .await
                .expect_err("failed actual write stays unresolved");
            use axum::response::IntoResponse;
            assert_eq!(
                first.into_response().status(),
                StatusCode::SERVICE_UNAVAILABLE
            );
            let entry = fixture
                .state
                .transcode
                .source_http_starts
                .entries
                .lock()
                .expect("retained task")
                .first()
                .cloned()
                .expect("actual owner entry");
            assert!(matches!(
                *entry.task.joined.lock().expect("actual joined task"),
                Some(SourceStartTaskJoined::Returned)
            ));
            let incarnation = {
                let stage = entry.task.stage.lock().expect("exact stage");
                if assignment_failure {
                    assert!(
                        matches!(*stage, SourceStartTaskStage::Assigning(_)),
                        "retain the acquired g0 binding across failed assignment"
                    );
                } else {
                    assert!(
                        matches!(*stage, SourceStartTaskStage::Claiming { .. }),
                        "retain the actual intent and planned incarnation across unknown claim"
                    );
                }
                stage.incarnation().expect("actual planned/claimed lineage")
            };
            assert_eq!(incarnation.get_version_num(), 4);
            assert_eq!(
                entry
                    .wait(Instant::now() + Duration::from_secs(1))
                    .await
                    .err(),
                Some(SourceStartFailure::Unresolved)
            );
            let end = entry.end();
            assert_eq!(
                end.wait(Instant::now() + Duration::from_secs(1))
                    .await
                    .err(),
                Some(SourceStartFailure::Unresolved),
                "joined failure is not a no-admission/settlement proof"
            );
            client
                .execute(
                    "DROP TRIGGER fixture_source_task_failure",
                    hiqlite::params![],
                )
                .await
                .expect("restore real Store writes");
            assert_eq!(
                invoke()
                    .await
                    .expect_err("exact retry must not redispatch unknown outcome")
                    .into_response()
                    .status(),
                StatusCode::SERVICE_UNAVAILABLE
            );
            assert_eq!(
                entry
                    .task
                    .stage
                    .lock()
                    .expect("retained immutable stage")
                    .incarnation(),
                Some(incarnation)
            );
            // The joined failure owns no in-process work, so it leaves start
            // capacity; its receipt stays in the settled cache, where the exact
            // retry above found it instead of dispatching again.
            let registry = &fixture.state.transcode.source_http_starts;
            assert!(registry.entries.lock().expect("start capacity").is_empty());
            let after = registry
                .settled
                .lock()
                .expect("settled receipts")
                .iter()
                .find(|settled| std::sync::Arc::ptr_eq(settled, &entry))
                .cloned()
                .expect("same entry");
            assert!(std::sync::Arc::ptr_eq(&entry, &after));
            assert!(
                std::sync::Arc::ptr_eq(&end, &entry.end()),
                "no failed task receipt can overwrite End ownership"
            );
            fixture.shutdown().await;
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sharing_source_http_lost_start_end_uses_actual_assignment_without_inventing_session() {
        Box::pin(actual_lost_start_end()).await;
    }
    async fn actual_lost_start_end() {
        use std::time::{Duration, Instant};
        use tokio::io::AsyncWriteExt;
        let fixture = real_source_start_fixture().await;
        let hold = fixture
            .state
            .transcode
            .test_hold_source_software_capacity()
            .expect("real SW4 admission held");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(crate::serve_http(
            listener,
            super::super::sharing::peer_router((*fixture.state).clone()),
            async move {
                let _ = stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        let path = format!(
            "/sharing/v1/items/{}/files/{}/sessions",
            fixture.reference.item_id.as_str(),
            fixture.reference.file_id.as_str()
        );
        let mut socket = tokio::net::TcpStream::connect(address)
            .await
            .expect("initial TCP");
        let head=format!("POST {path} HTTP/1.1\r\nHost: fixture\r\nAuthorization: {}\r\nCinemaShare-Viewer: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",fixture.headers["authorization"].to_str().expect("auth"),fixture.headers["cinemashare-viewer"].to_str().expect("viewer"),fixture.request.len());
        socket.write_all(head.as_bytes()).await.expect("head");
        socket
            .write_all(&fixture.request)
            .await
            .expect("full recipe");
        let entry = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let entry = fixture
                    .state
                    .transcode
                    .source_http_starts
                    .entries
                    .lock()
                    .expect("entries")
                    .first()
                    .cloned();
                if let Some(entry) = entry {
                    if entry.result.lock().expect("owner").is_some() {
                        break entry;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual assignment/actor owned");
        let owned = entry
            .wait(Instant::now() + Duration::from_secs(1))
            .await
            .expect("actual actor");
        assert!(entry.published.lock().expect("publication").is_none());
        drop(socket);
        fixture
            .state
            .store
            .revoke_share(fixture.grant, crate::state::clock_ms())
            .await
            .expect("revoke");
        fixture
            .state
            .store
            .put_setting(plurx_core::store::keys::SHARING_ENABLED, "false")
            .await
            .expect("off");
        let recipe: Value = serde_json::from_slice(&fixture.request).expect("original");
        let request = recipe["session"]["request_id"].as_str().expect("request");
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("cleanup client");
        let url = format!("http://{address}{path}/{request}/end");
        let body = fixture.request.clone();
        let headers = fixture.headers.clone();
        let first_client = client.clone();
        let first_url = url.clone();
        let first = tokio::spawn(async move {
            first_client
                .post(first_url)
                .headers(headers)
                .header("connection", "close")
                .body(body)
                .send()
                .await
                .expect("lost Start End")
                .bytes()
                .await
                .expect("receipt")
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while entry.ending.lock().expect("End").is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached End owned");
        // Drop the real occupied admission; the actor must prove its actual
        // admitted/reserved no-spawn retirement, never infer it from no response.
        drop(hold);
        let receipt = tokio::time::timeout(Duration::from_secs(15), first)
            .await
            .expect("actual no-spawn settlement")
            .expect("End task");
        let value: Value = serde_json::from_slice(&receipt).expect("closed receipt");
        assert_eq!(
            value["incarnation_id"],
            json!(owned.assignment.binding().incarnation_id())
        );
        assert!(value.get("session_id").is_none());
        assert!(value.get("control_epoch").is_none());
        assert_eq!(value["settled"], true);
        assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
        assert!(
            fixture
                .state
                .store
                .media_session_route_by_incarnation(
                    &owned.assignment.binding().incarnation_id().to_string()
                )
                .await
                .expect("route read")
                .is_none(),
            "actual never-published worker did not acquire a fake session"
        );
        assert_eq!(
            client
                .post(url)
                .headers(fixture.headers.clone())
                .header("connection", "close")
                .body(fixture.request.clone())
                .send()
                .await
                .expect("exact retry")
                .bytes()
                .await
                .expect("same receipt"),
            receipt
        );
        let _ = stop.send(());
        server.await.expect("server").expect("shutdown");
        drop((owned, client));
        fixture.shutdown().await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sharing_source_http_real_h1_h2_playlist_init_and_segment_echo_exact_lineage() {
        Box::pin(actual_source_resource_delivery(SourceFixtureMode::Copy)).await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sharing_source_http_real_encoded_h1_h2_playlist_init_and_segment() {
        Box::pin(actual_source_resource_delivery(SourceFixtureMode::Encoded)).await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sharing_source_http_native_copy_h1_h2_vtt_and_held_body_end() {
        Box::pin(actual_source_resource_delivery(
            SourceFixtureMode::NativeCopy,
        ))
        .await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sharing_source_http_native_encoded_h1_h2_vtt_and_held_body_end() {
        Box::pin(actual_source_resource_delivery(
            SourceFixtureMode::NativeEncoded,
        ))
        .await;
    }
    async fn actual_source_resource_delivery(mode: SourceFixtureMode) {
        use std::time::{Duration, Instant};
        let fixture = real_source_start_fixture_with(mode, None).await;
        let native = matches!(
            mode,
            SourceFixtureMode::NativeCopy | SourceFixtureMode::NativeEncoded
        );
        let mut unsupported: Value = serde_json::from_slice(&fixture.request).expect("recipe");
        unsupported["session"]["previous_session_id"] = json!(Uuid::new_v4());
        let denied = start(
            axum::extract::State((*fixture.state).clone()),
            fixture.headers.clone(),
            axum::extract::Path((
                fixture.reference.item_id.as_str().to_owned(),
                fixture.reference.file_id.as_str().to_owned(),
            )),
            axum::body::Body::from(
                serde_json::to_vec(&unsupported).expect("complete prior recipe"),
            ),
        )
        .await
        .expect_err("foreign predecessor denied before Source preparation");
        use axum::response::IntoResponse;
        assert_eq!(
            denied.into_response().status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert!(fixture
            .state
            .transcode
            .source_http_starts
            .entries
            .lock()
            .expect("registry")
            .is_empty());
        let response = start(
            axum::extract::State((*fixture.state).clone()),
            fixture.headers.clone(),
            axum::extract::Path((
                fixture.reference.item_id.as_str().to_owned(),
                fixture.reference.file_id.as_str().to_owned(),
            )),
            axum::body::Body::from(fixture.request.clone()),
        )
        .await
        .expect("actual Start");
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("full DTO");
        let decoded = super::super::decode_source_start_response(&bytes, &fixture.reference)
            .expect("strict actual Start");
        let mut recipe: Value = serde_json::from_slice(&fixture.request).expect("recipe");
        let request = recipe["session"]["request_id"]
            .as_str()
            .expect("request")
            .to_owned();
        recipe["incarnation_id"] = json!(decoded.incarnation_id());
        recipe["session_id"] = json!(decoded.response().session_id);
        recipe["control_epoch"] = json!(
            decoded
                .response()
                .control
                .as_ref()
                .expect("control")
                .control_epoch
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(crate::serve_http(
            listener,
            super::super::sharing::peer_router((*fixture.state).clone()),
            async move {
                let _ = stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        let url = format!(
            "http://{address}/sharing/v1/items/{}/files/{}/sessions/{request}/resources",
            fixture.reference.item_id.as_str(),
            fixture.reference.file_id.as_str()
        );
        for h2 in [false, true] {
            recipe["resource"] = json!(if native { "video.m3u8" } else { "index.m3u8" });
            let response = actual_resource_request(
                address,
                h2,
                &url,
                fixture.headers.clone(),
                serde_json::to_vec(&recipe).expect("request"),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()["cinemashare-session-id"],
                decoded.response().session_id
            );
            assert_eq!(
                response.headers()["cinemashare-incarnation-id"],
                decoded.incarnation_id().to_string()
            );
            assert_eq!(
                response.headers()["cinemashare-reference"],
                serde_json::to_string(&fixture.reference).expect("full ref")
            );
            let playlist = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
                .await
                .expect("real playlist bytes");
            plurx_core::sharing_resources::validate_sharing_playlist(
                &plurx_core::sharing_resources::SharingHlsResource::parse(if native {
                    "video.m3u8"
                } else {
                    "index.m3u8"
                })
                .expect("path"),
                &playlist,
            )
            .expect("closed actual playlist");
            let playlist = std::str::from_utf8(&playlist).expect("playlist text");
            let segment = playlist
                .lines()
                .find(|line| !line.starts_with('#') && !line.is_empty())
                .expect("real media URI");
            for resource in ["init.mp4", segment] {
                recipe["resource"] = json!(resource);
                let response = actual_resource_request(
                    address,
                    h2,
                    &url,
                    fixture.headers.clone(),
                    serde_json::to_vec(&recipe).expect("request"),
                )
                .await;
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(response.headers()["cinemashare-resource"], resource);
                let len = response.headers()["content-length"]
                    .to_str()
                    .expect("len")
                    .parse::<usize>()
                    .expect("exact len");
                assert!(response.headers().contains_key("etag"));
                let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
                    .await
                    .expect("actual streamed file bytes");
                assert_eq!(bytes.len(), len);
                assert!(bytes.len() > 8);
                assert!(
                    bytes.windows(4).any(|part| part
                        == if resource == "init.mp4" {
                            b"ftyp"
                        } else {
                            b"moof"
                        }),
                    "actual FFmpeg MP4 object"
                );
            }
            if native {
                for resource in [
                    "master.m3u8?subtitle=0",
                    "subs/0/index.m3u8",
                    "subs/0/seg00000.vtt",
                    "subs/1/index.m3u8",
                    "subs/1/seg00000.vtt",
                ] {
                    recipe["resource"] = json!(resource);
                    let response = actual_resource_request(
                        address,
                        h2,
                        &url,
                        fixture.headers.clone(),
                        serde_json::to_vec(&recipe).expect("native request"),
                    )
                    .await;
                    assert_eq!(response.status(), StatusCode::OK, "native {resource}");
                    assert_eq!(response.headers()["cinemashare-resource"], resource);
                    let mime = response.headers()["content-type"]
                        .to_str()
                        .expect("actual MIME")
                        .to_owned();
                    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
                        .await
                        .expect("actual native bytes");
                    if resource.ends_with(".vtt") {
                        assert_eq!(mime, "text/vtt");
                        let text = std::str::from_utf8(&bytes).expect("actual VTT UTF8");
                        assert!(text.starts_with("WEBVTT"));
                        assert!(text.contains("X-TIMESTAMP-MAP"));
                        assert!(text.contains(if resource.starts_with("subs/1/") {
                            "Alternative HTTP Source caption"
                        } else {
                            "Actual HTTP Source caption"
                        }));
                    } else {
                        if resource.starts_with("master.") {
                            assert!(
                                std::str::from_utf8(&bytes)
                                    .expect("actual master")
                                    .contains("subs/1/index.m3u8"),
                                "wrong selected track exists and is actually advertised"
                            );
                        }
                        assert_eq!(mime, "application/vnd.apple.mpegurl");
                        plurx_core::sharing_resources::validate_sharing_playlist(
                            &plurx_core::sharing_resources::SharingHlsResource::parse(resource)
                                .expect("closed native path"),
                            &bytes,
                        )
                        .expect("closed actual native playlist");
                    }
                }
                for resource in [
                    "master.m3u8?subtitle=1",
                    "subs/2/index.m3u8",
                    "subs/2/seg00000.vtt",
                ] {
                    recipe["resource"] = json!(resource);
                    assert_eq!(
                        actual_resource_request(
                            address,
                            h2,
                            &url,
                            fixture.headers.clone(),
                            serde_json::to_vec(&recipe).expect("wrong frozen selector")
                        )
                        .await
                        .status(),
                        StatusCode::UNPROCESSABLE_ENTITY
                    );
                }
            }
            recipe["resource"] = json!("../init.mp4");
            assert_eq!(
                actual_resource_request(
                    address,
                    h2,
                    &url,
                    fixture.headers.clone(),
                    serde_json::to_vec(&recipe).expect("bad URI")
                )
                .await
                .status(),
                StatusCode::BAD_REQUEST
            );
        }
        let entry = fixture
            .state
            .transcode
            .source_http_starts
            .entries
            .lock()
            .expect("entry")
            .first()
            .cloned()
            .expect("entry");
        let owned = entry
            .wait(Instant::now() + Duration::from_secs(1))
            .await
            .expect("owner");
        if native {
            actual_vtt_writer_end(&fixture, &entry, &owned, &recipe, &request).await;
        }
        tokio::time::timeout(Duration::from_secs(15), owned.actor.retire())
            .await
            .expect("actual bodies/writers settle")
            .expect("retirement");
        assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
        let _ = stop.send(());
        server.await.expect("server").expect("shutdown");
        drop(owned);
        fixture.shutdown().await;
    }
    async fn actual_vtt_writer_end(
        fixture: &RealSourceStartFixture,
        entry: &std::sync::Arc<SourceStartEntry>,
        owned: &SourceStartOwned,
        recipe: &Value,
        request_id: &str,
    ) {
        use std::{
            sync::{atomic::Ordering, Arc},
            time::Duration,
        };
        let probe = Arc::new(AcceptedWriterProbe::default());
        probe.gate.store(true, Ordering::SeqCst);
        probe.vtt.store(true, Ordering::SeqCst);
        *probe.actor.lock().expect("actual actor") = Some(owned.actor.clone());
        let (capture, captured) = tokio::sync::oneshot::channel();
        let capture = Arc::new(std::sync::Mutex::new(Some(capture)));
        let app = super::super::sharing::peer_router((*fixture.state).clone()).layer(
            axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let capture = Arc::clone(&capture);
                    async move {
                        if request.uri().path().ends_with("/resources") {
                            capture
                                .lock()
                                .expect("capture")
                                .take()
                                .expect("one actual VTT")
                                .send(
                                    request
                                        .extensions()
                                        .get::<crate::SharingConnectionCancellation>()
                                        .expect("actual accepted transport")
                                        .clone(),
                                )
                                .ok();
                        }
                        next.run(request).await
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("VTT listener");
        let address = listener.local_addr().expect("address");
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(crate::serve_http(
            GatedStartListener {
                listener,
                probe: Arc::clone(&probe),
            },
            app,
            async move {
                let _ = stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        let path = format!(
            "/sharing/v1/items/{}/files/{}/sessions/{request_id}",
            fixture.reference.item_id.as_str(),
            fixture.reference.file_id.as_str()
        );
        let mut recipe = recipe.clone();
        recipe["resource"] = json!("subs/0/seg00000.vtt");
        let socket = tokio::net::TcpStream::connect(address)
            .await
            .expect("real VTT TCP");
        let (mut sender, driver) =
            hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                .handshake::<_, axum::body::Body>(hyper_util::rt::TokioIo::new(socket))
                .await
                .expect("actual H2");
        let driver = tokio::spawn(driver);
        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri(format!("http://fixture{path}/resources"))
            .body(axum::body::Body::from(
                serde_json::to_vec(&recipe).expect("actual VTT request"),
            ))
            .expect("request");
        *request.headers_mut() = fixture.headers.clone();
        let send = tokio::spawn(async move { sender.send_request(request).await });
        let connection = captured.await.expect("actual accepted VTT connection");
        tokio::time::timeout(Duration::from_secs(10), async {
            while !probe.blocked.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual VTT DATA at blocked accepted writer");
        assert!(probe.queued_start.load(Ordering::SeqCst));
        assert!(!probe.dropped.load(Ordering::SeqCst));
        assert!(!connection.closed().is_closed());
        assert_eq!(owned.actor.settlement_status(), None);
        recipe.as_object_mut().expect("recipe").remove("resource");
        let end_url = format!("http://{address}{path}/end");
        let headers = fixture.headers.clone();
        let ending = tokio::spawn(async move {
            reqwest::Client::builder()
                .http1_only()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("End client")
                .post(end_url)
                .headers(headers)
                .header("connection", "close")
                .body(serde_json::to_vec(&recipe).expect("exact End recipe"))
                .send()
                .await
                .expect("actual End response")
        });
        let response = tokio::time::timeout(Duration::from_secs(15), ending)
            .await
            .expect("actual VTT writer and physical settlement")
            .expect("End task");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(entry
            .ending
            .lock()
            .expect("retained actual End owner")
            .is_some());
        assert!(
            connection.closed().is_closed(),
            "actual VTT accepted writer joined before End receipt"
        );
        assert!(probe.dropped.load(Ordering::SeqCst));
        assert!(
            !probe.settled_before_drop.load(Ordering::SeqCst),
            "Source must not settle while real VTT IO is still owned"
        );
        assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
        let receipt: Value = response.json().await.expect("actual terminal receipt");
        assert_eq!(receipt["settled"], true);
        assert_eq!(receipt["request_id"], request_id);
        assert_eq!(
            receipt["incarnation_id"],
            owned.assignment.binding().incarnation_id().to_string()
        );
        send.abort();
        let _ = send.await;
        driver.abort();
        let _ = driver.await;
        let _ = stop.send(());
        server.await.expect("VTT server").expect("shutdown");
    }
    #[test]
    fn sharing_source_first_start_refuses_foreign_predecessor_and_control_recipe_before_prepare() {
        let value = fixture();
        assert!(validate_initial_source_start(&parse(&value).expect("initial").session).is_ok());
        for (key, field) in [
            ("previous_session_id", json!(Uuid::new_v4())),
            ("control_sequence", json!(0)),
            ("reopen_reason", json!("stall")),
        ] {
            let mut unsupported = value.clone();
            unsupported["session"][key] = field;
            let input = parse(&unsupported).expect("complete recipe remains parseable for cleanup");
            assert_eq!(
                validate_initial_source_start(&input.session),
                Err(SourceStartFailure::Unsupported)
            );
            assert!(
                input
                    .canonical_recipe
                    .windows(key.len())
                    .any(|bytes| bytes == key.as_bytes()),
                "original recipe not stripped"
            );
        }
    }
    #[test]
    fn sharing_source_operations_close_full_recipe_lineage_resource_and_body_bounds() {
        for id in ["0", "9223372036854775807"] {
            let mut value = fixture();
            value["reference"]["item_id"] = json!(id);
            value["reference"]["file_id"] = json!(id);
            let request = value["session"]["request_id"]
                .as_str()
                .expect("request")
                .to_owned();
            let encode = |value: &Value| serde_json::to_vec(value).expect("bounded fixture");
            assert!(parse_operation_request(&encode(&value), id, id, &request).is_ok());
            value["incarnation_id"] = json!(Uuid::new_v4());
            assert!(
                parse_operation_request(&encode(&value), id, id, &request).is_err(),
                "partial known tuple refused"
            );
            value["session_id"] = json!(Uuid::new_v4());
            value["control_epoch"] = json!(i64::MAX);
            assert!(parse_operation_request(&encode(&value), id, id, &request).is_ok());
            value["resource"] = json!("init.mp4");
            assert!(parse_resource_request(&encode(&value), id, id, &request).is_ok());
            for path in [
                "https://source/private",
                "../init.mp4",
                "%2e%2e/init.mp4",
                "/init.mp4",
                "init.mp4#fragment",
            ] {
                value["resource"] = json!(path);
                assert!(parse_resource_request(&encode(&value), id, id, &request).is_err());
            }
            value["resource"] = json!("init.mp4");
            value["control_epoch"] = json!(0);
            assert!(parse_resource_request(&encode(&value), id, id, &request).is_err());
            value["control_epoch"] = json!(1);
            value["session_id"] = json!(Uuid::nil());
            assert!(parse_resource_request(&encode(&value), id, id, &request).is_err());
            value["session_id"] = json!(Uuid::new_v4());
            value["unknown"] = json!(true);
            assert!(parse_resource_request(&encode(&value), id, id, &request).is_err());
            value.as_object_mut().expect("object").remove("unknown");
            assert!(
                parse_resource_request(&encode(&value), id, id, &Uuid::new_v4().to_string())
                    .is_err()
            );
            let duplicate = format!(
                "{{\"resource\":\"init.mp4\",{}",
                String::from_utf8(encode(&value))
                    .expect("UTF8")
                    .trim_start_matches('{')
            );
            assert!(
                parse_resource_request(duplicate.as_bytes(), id, id, &request).is_err(),
                "duplicate resource refuses before authority"
            );
            assert!(parse_resource_request(&vec![b' '; 128 * 1024 + 1], id, id, &request).is_err());
            value.as_object_mut().expect("object").remove("session_id");
            value
                .as_object_mut()
                .expect("object")
                .remove("incarnation_id");
            value
                .as_object_mut()
                .expect("object")
                .remove("control_epoch");
            assert!(
                parse_resource_request(&encode(&value), id, id, &request).is_err(),
                "resource needs actual observed Source tuple"
            );
        }
    }
    #[test]
    fn sharing_source_cleanup_hashes_are_bounded_exact_and_never_infer_missing_settlement() {
        let registry = SourceStartRegistry::default();
        let grant = Uuid::new_v4();
        let viewer = "a".repeat(64);
        let input = parse(&fixture()).expect("request");
        let (entry, _) = registry
            .register(grant, &viewer, &input, &"0".repeat(64))
            .expect("entry");
        assert!(registry
            .cleanup_entry(&"0".repeat(64), &viewer, &input)
            .is_ok());
        assert!(matches!(
            registry.cleanup_entry(&"f".repeat(64), &viewer, &input),
            Err(SourceStartFailure::Unresolved)
        ));
        assert!(matches!(
            registry.cleanup_entry(&"0".repeat(64), &"b".repeat(64), &input),
            Err(SourceStartFailure::Unresolved)
        ));
        for index in 1..8 {
            let hash = format!("{index:064x}");
            assert!(registry
                .current_entry(grant, &hash, &viewer, &input)
                .is_ok());
        }
        assert!(matches!(
            registry.current_entry(grant, &format!("{:064x}", 8), &viewer, &input),
            Err(SourceStartFailure::Capacity)
        ));
        assert!(
            registry
                .current_entry(grant, &"0".repeat(64), &viewer, &input)
                .is_ok(),
            "repeat remembered hash does not consume capacity"
        );
        assert!(!entry.actual_settled());
        assert!(entry.published.lock().expect("published").is_none());
        assert!(
            SourceStartRegistry::default()
                .cleanup_entry(&"0".repeat(64), &viewer, &input)
                .is_err(),
            "restart absence stays unresolved"
        );
        assert_eq!(registry.settled.lock().expect("cache").len(), 0);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sharing_source_http_incomplete_h2_and_owned_read_join_before_end_receipt() {
        Box::pin(actual_source_read_join()).await;
    }
    async fn actual_source_read_join() {
        use std::{
            sync::{atomic::Ordering, Arc},
            time::{Duration, Instant},
        };
        let fixture = real_source_start_fixture().await;
        let response = start(
            axum::extract::State((*fixture.state).clone()),
            fixture.headers.clone(),
            axum::extract::Path((
                fixture.reference.item_id.as_str().to_owned(),
                fixture.reference.file_id.as_str().to_owned(),
            )),
            axum::body::Body::from(fixture.request.clone()),
        )
        .await
        .expect("actual Source Start");
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("real full DTO");
        let decoded = super::super::decode_source_start_response(&bytes, &fixture.reference)
            .expect("strict actual Start");
        let mut request: Value = serde_json::from_slice(&fixture.request).expect("recipe");
        let request_id = request["session"]["request_id"]
            .as_str()
            .expect("request")
            .to_owned();
        request["incarnation_id"] = json!(decoded.incarnation_id());
        request["session_id"] = json!(decoded.response().session_id);
        request["control_epoch"] = json!(
            decoded
                .response()
                .control
                .as_ref()
                .expect("control")
                .control_epoch
        );
        let known_body = serde_json::to_vec(&request).expect("known full recipe");
        request["resource"] = json!("init.mp4");
        let resource_body = serde_json::to_vec(&request).expect("resource request");
        let read_gate = Arc::new(SourceReadJobGate::default());
        let io_probe = Arc::new(AcceptedWriterProbe::default());
        let (capture, captured) = tokio::sync::oneshot::channel();
        let capture = Arc::new(std::sync::Mutex::new(Some(capture)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let app = super::super::sharing::peer_router((*fixture.state).clone())
            .layer(axum::Extension(Arc::clone(&read_gate)))
            .layer(axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let capture = Arc::clone(&capture);
                    async move {
                        if request.uri().path().ends_with("/resources") {
                            let connection = request
                                .extensions()
                                .get::<crate::SharingConnectionCancellation>()
                                .expect("actual connection")
                                .clone();
                            capture
                                .lock()
                                .expect("capture")
                                .take()
                                .expect("one resource")
                                .send(connection)
                                .ok();
                        }
                        next.run(request).await
                    }
                },
            ));
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(crate::serve_http(
            GatedStartListener {
                listener,
                probe: Arc::clone(&io_probe),
            },
            app,
            async move {
                let _ = stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        let socket = tokio::net::TcpStream::connect(address).await.expect("TCP");
        let (mut sender, driver) =
            hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                .handshake::<_, axum::body::Body>(hyper_util::rt::TokioIo::new(socket))
                .await
                .expect("real H2");
        let driver = tokio::spawn(driver);
        let path = format!(
            "/sharing/v1/items/{}/files/{}/sessions/{request_id}",
            fixture.reference.item_id.as_str(),
            fixture.reference.file_id.as_str()
        );
        let request = axum::http::Request::builder()
            .method("POST")
            .uri(format!("http://fixture{path}/resources"))
            .header(
                "authorization",
                fixture.headers.get("authorization").expect("auth"),
            )
            .header(
                "cinemashare-viewer",
                fixture.headers.get("cinemashare-viewer").expect("viewer"),
            )
            .header("content-type", "application/json")
            .body(axum::body::Body::from(resource_body))
            .expect("H2 request");
        let send = tokio::spawn(async move { sender.send_request(request).await });
        let connection = captured.await.expect("accepted resource connection");
        tokio::time::timeout(Duration::from_secs(10), async {
            while !read_gate.entered.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual blocking read is running");
        let response = tokio::time::timeout(Duration::from_secs(5), send)
            .await
            .expect("H2 response headers")
            .expect("send task")
            .expect("actual response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cinemashare-resource"], "init.mp4");
        assert_eq!(response.headers()["cinemashare-request-id"], request_id);
        assert_eq!(response.headers()["content-type"], "video/mp4");
        assert!(!connection.closed().is_closed());
        assert!(!read_gate.complete.load(Ordering::SeqCst));
        assert!(!read_gate.file_closed.load(Ordering::SeqCst));
        let entry = fixture
            .state
            .transcode
            .source_http_starts
            .entries
            .lock()
            .expect("entry")
            .first()
            .cloned()
            .expect("actual owner");
        let owned = entry
            .wait(Instant::now() + Duration::from_secs(1))
            .await
            .expect("owner");
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("End client");
        let end_url = format!("http://{address}{path}/end");
        let headers = fixture.headers.clone();
        let ending = tokio::spawn(async move {
            client
                .post(end_url)
                .headers(headers)
                .header("connection", "close")
                .body(known_body)
                .send()
                .await
                .expect("actual End")
                .bytes()
                .await
                .expect("receipt")
        });
        tokio::time::timeout(Duration::from_secs(10), connection.closed().wait())
            .await
            .expect("actual incomplete H2 connection closed");
        assert!(
            io_probe.dropped.load(Ordering::SeqCst),
            "accepted IO dropped before closure receipt"
        );
        assert!(
            !read_gate.complete.load(Ordering::SeqCst),
            "owned physical read is still parked"
        );
        assert!(
            !read_gate.file_closed.load(Ordering::SeqCst),
            "actual FD remains owned by read job"
        );
        assert_eq!(
            owned.actor.settlement_status(),
            None,
            "writer close alone cannot settle a live read job"
        );
        assert!(!ending.is_finished());
        drop(response);
        read_gate.release();
        let receipt = tokio::time::timeout(Duration::from_secs(15), ending)
            .await
            .expect("read/FD/writer joined")
            .expect("End owner");
        let receipt: Value = serde_json::from_slice(&receipt).expect("terminal receipt");
        assert_eq!(receipt["state"], "settled");
        assert_eq!(receipt["settled"], true);
        assert!(read_gate.complete.load(Ordering::SeqCst));
        assert!(read_gate.file_closed.load(Ordering::SeqCst));
        assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
        driver.abort();
        let _ = driver.await;
        let _ = stop.send(());
        server.await.expect("server").expect("shutdown");
        drop(owned);
        fixture.shutdown().await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sharing_source_http_live_media_body_stops_on_revocation() {
        Box::pin(actual_source_media_revocation()).await;
    }
    /// The one body that keeps producing bytes after its handler returns keeps
    /// its authority monitor, and replicated revocation still reaches it.
    async fn actual_source_media_revocation() {
        use std::{
            sync::{atomic::Ordering, Arc},
            time::{Duration, Instant},
        };
        let fixture = real_source_start_fixture().await;
        let response = start(
            axum::extract::State((*fixture.state).clone()),
            fixture.headers.clone(),
            axum::extract::Path((
                fixture.reference.item_id.as_str().to_owned(),
                fixture.reference.file_id.as_str().to_owned(),
            )),
            axum::body::Body::from(fixture.request.clone()),
        )
        .await
        .expect("actual Source Start");
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("real full DTO");
        let decoded = super::super::decode_source_start_response(&bytes, &fixture.reference)
            .expect("strict actual Start");
        let mut request: Value = serde_json::from_slice(&fixture.request).expect("recipe");
        let request_id = request["session"]["request_id"]
            .as_str()
            .expect("request")
            .to_owned();
        request["incarnation_id"] = json!(decoded.incarnation_id());
        request["session_id"] = json!(decoded.response().session_id);
        request["control_epoch"] = json!(
            decoded
                .response()
                .control
                .as_ref()
                .expect("control")
                .control_epoch
        );
        request["resource"] = json!("init.mp4");
        let resource_body = serde_json::to_vec(&request).expect("resource request");
        let read_gate = Arc::new(SourceReadJobGate::default());
        let io_probe = Arc::new(AcceptedWriterProbe::default());
        let (capture, captured) = tokio::sync::oneshot::channel();
        let capture = Arc::new(std::sync::Mutex::new(Some(capture)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let app = super::super::sharing::peer_router((*fixture.state).clone())
            .layer(axum::Extension(Arc::clone(&read_gate)))
            .layer(axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let capture = Arc::clone(&capture);
                    async move {
                        if request.uri().path().ends_with("/resources") {
                            let connection = request
                                .extensions()
                                .get::<crate::SharingConnectionCancellation>()
                                .expect("actual connection")
                                .clone();
                            capture
                                .lock()
                                .expect("capture")
                                .take()
                                .expect("one resource")
                                .send(connection)
                                .ok();
                        }
                        next.run(request).await
                    }
                },
            ));
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(crate::serve_http(
            GatedStartListener {
                listener,
                probe: Arc::clone(&io_probe),
            },
            app,
            async move {
                let _ = stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        let socket = tokio::net::TcpStream::connect(address).await.expect("TCP");
        let (mut sender, driver) =
            hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                .handshake::<_, axum::body::Body>(hyper_util::rt::TokioIo::new(socket))
                .await
                .expect("real H2");
        let driver = tokio::spawn(driver);
        let path = format!(
            "/sharing/v1/items/{}/files/{}/sessions/{request_id}",
            fixture.reference.item_id.as_str(),
            fixture.reference.file_id.as_str()
        );
        let request = axum::http::Request::builder()
            .method("POST")
            .uri(format!("http://fixture{path}/resources"))
            .header(
                "authorization",
                fixture.headers.get("authorization").expect("auth"),
            )
            .header(
                "cinemashare-viewer",
                fixture.headers.get("cinemashare-viewer").expect("viewer"),
            )
            .header("content-type", "application/json")
            .body(axum::body::Body::from(resource_body))
            .expect("H2 request");
        let send = tokio::spawn(async move { sender.send_request(request).await });
        let connection = captured.await.expect("accepted resource connection");
        tokio::time::timeout(Duration::from_secs(10), async {
            while !read_gate.entered.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual blocking read is running");
        let response = tokio::time::timeout(Duration::from_secs(5), send)
            .await
            .expect("H2 response headers")
            .expect("send task")
            .expect("actual response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cinemashare-resource"], "init.mp4");
        assert_eq!(response.headers()["cinemashare-request-id"], request_id);
        assert_eq!(response.headers()["content-type"], "video/mp4");
        assert!(!connection.closed().is_closed());
        assert!(!read_gate.complete.load(Ordering::SeqCst));
        assert!(!read_gate.file_closed.load(Ordering::SeqCst));
        let entry = fixture
            .state
            .transcode
            .source_http_starts
            .entries
            .lock()
            .expect("entry")
            .first()
            .cloned()
            .expect("actual owner");
        let owned = entry
            .wait(Instant::now() + Duration::from_secs(1))
            .await
            .expect("owner");
        fixture
            .state
            .store
            .revoke_share(fixture.grant, crate::state::clock_ms())
            .await
            .expect("actual grant revoke");
        tokio::time::timeout(Duration::from_secs(5), connection.0.cancelled())
            .await
            .expect("revocation closes the live media body's transport");
        tokio::time::timeout(Duration::from_secs(10), connection.closed().wait())
            .await
            .expect("accepted writer dropped");
        drop(response);
        read_gate.release();
        let actor = owned.actor.clone();
        let _ = tokio::time::timeout(
            Duration::from_secs(15),
            tokio::spawn(async move { actor.retire().await }),
        )
        .await;
        driver.abort();
        let _ = driver.await;
        let _ = stop.send(());
        server.await.expect("server").expect("shutdown");
        drop(owned);
        fixture.shutdown().await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sharing_source_http_fixture_uses_actual_startup_factory_and_full_current_file_reference(
    ) {
        let fixture = real_source_start_fixture().await;
        let (_, grant) = current_reference(&fixture.state, &fixture.headers, &fixture.reference)
            .await
            .expect("actual authenticated tuple");
        assert_eq!(grant, fixture.grant);
        let input = parse_start_request(
            &fixture.request,
            fixture.reference.item_id.as_str(),
            fixture.reference.file_id.as_str(),
        )
        .expect("complete canonical actual request");
        assert!(input.reference == fixture.reference);
        let mut stale = fixture.reference.clone();
        stale.revision =
            plurx_core::sharing_catalogue_details::FileRevision::parse(&"f".repeat(64))
                .expect("revision");
        assert!(current_reference(&fixture.state, &fixture.headers, &stale)
            .await
            .is_err());
        fixture.shutdown().await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sharing_source_http_genuine_invitation_replays_actual_claim_and_selected_identity() {
        Box::pin(actual_source_genuine_invitation()).await;
    }
    async fn actual_source_genuine_invitation() {
        use plurx_core::sharing::{
            secret_hash, ClaimOutcome, Endpoint, GrantState, Invitation, SecretDomain, ShareClaim,
        };
        let recipient = Uuid::new_v4();
        let fixture =
            real_source_start_fixture_with(SourceFixtureMode::Copy, Some(recipient)).await;
        assert_eq!(fixture.state.node_id, fixture.selected.identity.node_id);
        let tls = plurx_core::sharing_tls::LiveNodeTls::open(
            &fixture._directory.path().join("actual-peer-tls"),
            crate::state::clock_ms() / 1000,
        )
        .expect("actual production TLS key");
        let (pin, _) = tls.status().expect("actual SPKI");
        let endpoint = Endpoint {
            ipv4: "100.127.89.2".parse().expect("bounded endpoint"),
            ipv6: None,
            ts_fqdn: "source.fixture.ts.net".into(),
            port: 32443,
            spki_sha256: pin,
        };
        // This verifies the real invitation credential and TLS key material;
        // an actual pinned network exchange belongs to the separate CGNAT drill.
        let invitation = fixture
            .invitation_for(endpoint.clone())
            .expect("actual invitation");
        let blob = invitation.encode().expect("real bootstrap blob");
        let parsed = Invitation::parse(&blob).expect("closed actual invitation");
        assert_eq!(parsed.identity.server_id, fixture.reference.server_id);
        assert_eq!(
            parsed.identity.catalogue_epoch,
            fixture.reference.catalogue_epoch
        );
        assert_eq!(parsed.endpoints, vec![endpoint]);
        let secret = plurx_core::secrets::Secret::from_cleartext(
            fixture
                .headers
                .get("authorization")
                .expect("actual grant credential")
                .to_str()
                .expect("closed header")
                .strip_prefix("CinemaShare ")
                .expect("actual scheme"),
        );
        let grant_hash = secret_hash(SecretDomain::Grant, &secret);
        let status = fixture
            .state
            .store
            .sharing_grant_status(&grant_hash)
            .await
            .expect("actual credential status")
            .expect("actual active claim");
        assert_eq!(status.invitation_id, parsed.id);
        assert_eq!(status.grant.id, fixture.grant);
        assert_eq!(status.grant.recipient_server_id, recipient);
        assert_eq!(status.grant.state, GrantState::Active);
        let replay = fixture
            .state
            .store
            .claim_share(ShareClaim {
                invitation_id: parsed.id,
                invitation_hash: secret_hash(SecretDomain::Invitation, &parsed.secret),
                claim_id: status.claim_id,
                grant_id: status.grant.id,
                recipient_server_id: recipient,
                recipient_name: status.recipient_name.clone(),
                credential_hash: grant_hash,
                now_ms: crate::state::clock_ms(),
            })
            .await
            .expect("real original claim replay");
        assert!(matches!(replay,ClaimOutcome::Replay(grant) if grant==status.grant),"bootstrap secret must match the actual consumed invitation and unchanged approved grant");
        fixture.shutdown().await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sharing_source_http_end_retains_actual_owner_after_disconnect_and_revoke() {
        Box::pin(actual_source_http_end()).await;
    }
    async fn actual_source_http_end() {
        use std::time::{Duration, Instant};
        use tokio::io::AsyncWriteExt;
        let fixture = real_source_start_fixture().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("actual private listener");
        let address = listener.local_addr().expect("address");
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(crate::serve_http(
            listener,
            super::super::sharing::peer_router((*fixture.state).clone()),
            async move {
                let _ = stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");
        let path = format!(
            "http://{address}/sharing/v1/items/{}/files/{}/sessions",
            fixture.reference.item_id.as_str(),
            fixture.reference.file_id.as_str()
        );
        let started = client
            .post(&path)
            .headers(fixture.headers.clone())
            .header("connection", "close")
            .body(fixture.request.clone())
            .send()
            .await
            .expect("actual Start");
        assert_eq!(started.status(), StatusCode::OK);
        let start_bytes = started.bytes().await.expect("actual full Source envelope");
        let decoded = super::super::decode_source_start_response(&start_bytes, &fixture.reference)
            .expect("actual full strict Start");
        let mut request: Value = serde_json::from_slice(&fixture.request).expect("recipe");
        let request_id = request["session"]["request_id"]
            .as_str()
            .expect("request")
            .to_owned();
        let operation = format!("{path}/{request_id}");
        let entry = fixture
            .state
            .transcode
            .source_http_starts
            .entries
            .lock()
            .expect("registry")
            .first()
            .cloned()
            .expect("actual entry");
        let owned = entry
            .wait(Instant::now() + Duration::from_secs(1))
            .await
            .expect("actual actor");
        assert_eq!(owned.actor.settlement_status(), None);
        request["incarnation_id"] = json!(decoded.incarnation_id());
        request["session_id"] = json!(decoded.response().session_id);
        request["control_epoch"] = json!(
            decoded
                .response()
                .control
                .as_ref()
                .expect("actual control")
                .control_epoch
        );
        let known_body = serde_json::to_vec(&request).expect("exact known body");
        let status = client
            .post(format!("{operation}/status"))
            .headers(fixture.headers.clone())
            .header("connection", "close")
            .body(known_body.clone())
            .send()
            .await
            .expect("active status");
        assert_eq!(status.status(), StatusCode::OK);
        assert!(super::super::decode_source_start_response(
            &status.bytes().await.expect("active body"),
            &fixture.reference
        )
        .is_ok());
        let mut wrong = request.clone();
        wrong["session_id"] = json!(Uuid::new_v4());
        let denied = client
            .post(format!("{operation}/end"))
            .headers(fixture.headers.clone())
            .header("connection", "close")
            .body(serde_json::to_vec(&wrong).expect("wrong known"))
            .send()
            .await
            .expect("wrong End");
        assert_eq!(denied.status(), StatusCode::CONFLICT);
        assert!(entry.ending.lock().expect("End owner").is_none());
        // This is an actual actor-created metadata Body guard, not a projected
        // readiness flag. End cannot certify terminal settlement while held.
        let held = start(
            axum::extract::State((*fixture.state).clone()),
            fixture.headers.clone(),
            axum::extract::Path((
                fixture.reference.item_id.as_str().to_owned(),
                fixture.reference.file_id.as_str().to_owned(),
            )),
            axum::body::Body::from(fixture.request.clone()),
        )
        .await
        .expect("held actual Source Body");
        fixture
            .state
            .store
            .revoke_share(fixture.grant, crate::state::clock_ms())
            .await
            .expect("actual grant revoke");
        let status = client
            .post(format!("{operation}/status"))
            .headers(fixture.headers.clone())
            .header("connection", "close")
            .body(known_body.clone())
            .send()
            .await
            .expect("cleanup status");
        assert_eq!(status.status(), StatusCode::OK);
        let facts: Value = status.json().await.expect("closed cleanup facts");
        assert!(facts.get("response").is_none());
        assert_eq!(facts["state"], json!("unresolved"));
        fixture
            .state
            .store
            .put_setting(plurx_core::store::keys::SHARING_ENABLED, "false")
            .await
            .expect("saved off");
        let denied = client
            .get(format!("http://{address}/sharing/v1/identity"))
            .header("connection", "close")
            .send()
            .await
            .expect("off identity");
        assert_eq!(denied.status(), StatusCode::SERVICE_UNAVAILABLE);
        let denied = client
            .post(format!("{operation}/status"))
            .headers(fixture.headers.clone())
            .header("connection", "close")
            .body(known_body.clone())
            .send()
            .await
            .expect("off status");
        assert_eq!(denied.status(), StatusCode::SERVICE_UNAVAILABLE);
        let mut socket = tokio::net::TcpStream::connect(address)
            .await
            .expect("actual End TCP");
        let end_path = format!(
            "/sharing/v1/items/{}/files/{}/sessions/{request_id}/end",
            fixture.reference.item_id.as_str(),
            fixture.reference.file_id.as_str()
        );
        let head=format!("POST {end_path} HTTP/1.1\r\nHost: fixture\r\nAuthorization: {}\r\nCinemaShare-Viewer: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",fixture.headers.get("authorization").expect("auth").to_str().expect("text"),fixture.headers.get("cinemashare-viewer").expect("viewer").to_str().expect("text"),known_body.len());
        socket.write_all(head.as_bytes()).await.expect("End header");
        socket.write_all(&known_body).await.expect("End request");
        let end_owner = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(end) = entry.ending.lock().expect("actual End task").clone() {
                    break end;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached actual End owner");
        drop(socket);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(end_owner.result.lock().expect("End result").is_none());
        assert_eq!(owned.actor.settlement_status(), None);
        drop(held);
        let receipt = end_owner
            .wait(Instant::now() + Duration::from_secs(10))
            .await
            .expect("actual terminal receipt");
        assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
        let original = parse_start_request(
            &fixture.request,
            fixture.reference.item_id.as_str(),
            fixture.reference.file_id.as_str(),
        )
        .expect("exact original");
        let credential = super::super::sharing::credential(&fixture.headers)
            .expect("retained actually authenticated credential");
        let hash =
            plurx_core::sharing::secret_hash(plurx_core::sharing::SecretDomain::Grant, &credential);
        let (cached, new) = fixture
            .state
            .transcode
            .source_http_starts
            .register(
                fixture.grant,
                &viewer_hash(&fixture.headers).expect("viewer"),
                &original,
                &hash,
            )
            .expect("bookkeeping prunes only the actual settled actor");
        assert!(!new);
        assert!(std::sync::Arc::ptr_eq(&cached, &entry));
        assert!(fixture
            .state
            .transcode
            .source_http_starts
            .entries
            .lock()
            .expect("active entries")
            .is_empty());
        assert_eq!(
            fixture
                .state
                .transcode
                .source_http_starts
                .settled
                .lock()
                .expect("settled cache")
                .len(),
            1
        );

        assert_eq!(receipt.incarnation_id, decoded.incarnation_id());
        assert_eq!(
            receipt.session_id.map(|id| id.to_string()),
            Some(decoded.response().session_id.clone())
        );
        let first = client
            .post(format!("{operation}/end"))
            .headers(fixture.headers.clone())
            .header("connection", "close")
            .body(known_body.clone())
            .send()
            .await
            .expect("exact End ACK retry");
        assert_eq!(first.status(), StatusCode::OK);
        let first = first.bytes().await.expect("End receipt");
        assert_eq!(
            serde_json::from_slice::<Value>(&first).expect("receipt"),
            serde_json::to_value(&receipt).expect("actual stable receipt")
        );
        let second = client
            .post(format!("{operation}/end"))
            .headers(fixture.headers.clone())
            .header("connection", "close")
            .body(fixture.request.clone())
            .send()
            .await
            .expect("lost Start recipe cleanup retry");
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(second.bytes().await.expect("same actual receipt"), first);
        let mut wrong_headers = fixture.headers.clone();
        wrong_headers.insert(
            "cinemashare-viewer",
            "f".repeat(64).parse().expect("wrong viewer"),
        );
        let denied = client
            .post(format!("{operation}/end"))
            .headers(wrong_headers)
            .header("connection", "close")
            .body(known_body)
            .send()
            .await
            .expect("wrong cleanup principal");
        assert_eq!(denied.status(), StatusCode::SERVICE_UNAVAILABLE);
        stop.send(()).expect("stop");
        server.await.expect("server task").expect("shutdown");
        fixture.shutdown().await;
    }

    struct QueuedStartBytes {
        bytes: Vec<u8>,
        dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    impl AsRef<[u8]> for QueuedStartBytes {
        fn as_ref(&self) -> &[u8] {
            &self.bytes
        }
    }
    impl Drop for QueuedStartBytes {
        fn drop(&mut self) {
            self.dropped
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    struct QueuedStartBody {
        data: Option<bytes::Bytes>,
        dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    impl hyper::body::Body for QueuedStartBody {
        type Data = bytes::Bytes;
        type Error = std::convert::Infallible;
        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
            std::task::Poll::Ready(
                self.data
                    .take()
                    .map(|data| Ok(hyper::body::Frame::data(data))),
            )
        }
        fn is_end_stream(&self) -> bool {
            self.data.is_none()
        }
    }
    impl Drop for QueuedStartBody {
        fn drop(&mut self) {
            self.dropped
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    #[derive(Default)]
    struct AcceptedWriterProbe {
        vtt: std::sync::atomic::AtomicBool,
        actor: std::sync::Mutex<Option<crate::transcode::source_actor::SourceViewerActor>>,
        settled_before_drop: std::sync::atomic::AtomicBool,
        gate: std::sync::atomic::AtomicBool,
        blocked: std::sync::atomic::AtomicBool,
        queued_start: std::sync::atomic::AtomicBool,
        dropped: std::sync::atomic::AtomicBool,
    }
    struct GatedStartStream {
        stream: Option<tokio::net::TcpStream>,
        data_blocked: bool,
        probe: std::sync::Arc<AcceptedWriterProbe>,
    }
    impl Drop for GatedStartStream {
        fn drop(&mut self) {
            if self.data_blocked {
                if let Some(actor) = self
                    .probe
                    .actor
                    .lock()
                    .expect("actual Source actor probe")
                    .as_ref()
                {
                    self.probe.settled_before_drop.store(
                        actor.settlement_status() == Some(Ok(())),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                }
            }
            drop(self.stream.take());
            self.probe
                .dropped
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    impl tokio::io::AsyncRead for GatedStartStream {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(self.stream.as_mut().expect("actual accepted IO"))
                .poll_read(cx, buffer)
        }
    }
    impl tokio::io::AsyncWrite for GatedStartStream {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            let marker: &[u8] = if self.probe.vtt.load(std::sync::atomic::Ordering::SeqCst) {
                b"Actual HTTP Source caption"
            } else {
                b"incarnation_id"
            };
            let source_data = bytes.windows(marker.len()).any(|part| part == marker);
            if self.probe.gate.load(std::sync::atomic::Ordering::SeqCst)
                && (source_data || self.data_blocked)
            {
                self.data_blocked = true;
                if source_data {
                    self.probe
                        .queued_start
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                // Publish the observed DATA before the waiter sees blocked.
                self.probe
                    .blocked
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                return std::task::Poll::Pending;
            }
            std::pin::Pin::new(self.stream.as_mut().expect("actual accepted IO"))
                .poll_write(cx, bytes)
        }
        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(self.stream.as_mut().expect("actual accepted IO")).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(self.stream.as_mut().expect("actual accepted IO")).poll_shutdown(cx)
        }
    }
    struct GatedStartListener {
        listener: tokio::net::TcpListener,
        probe: std::sync::Arc<AcceptedWriterProbe>,
    }
    impl crate::HttpAcceptor for GatedStartListener {
        type Stream = GatedStartStream;
        async fn accept(&self) -> std::io::Result<(Self::Stream, std::net::SocketAddr)> {
            let (stream, address) = self.listener.accept().await?;
            Ok((
                GatedStartStream {
                    stream: Some(stream),
                    data_blocked: false,
                    probe: self.probe.clone(),
                },
                address,
            ))
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sharing_source_http_queued_h1_h2_writer_closes_before_actual_settlement() {
        Box::pin(actual_queued_source_start()).await;
    }
    async fn actual_queued_source_start() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        use std::time::{Duration, Instant};
        use tokio::io::AsyncWriteExt;
        for h2 in [false, true] {
            let fixture = real_source_start_fixture().await;
            let probe = Arc::new(AcceptedWriterProbe::default());
            let body_dropped = Arc::new(AtomicBool::new(false));
            let bytes_dropped = Arc::new(AtomicBool::new(false));
            let (accepted, captured) = tokio::sync::oneshot::channel();
            let accepted = Arc::new(std::sync::Mutex::new(Some(accepted)));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("private accepted listener");
            let address = listener.local_addr().expect("address");
            let app = super::super::sharing::peer_router((*fixture.state).clone()).layer(
                axum::middleware::from_fn({
                    let probe = probe.clone();
                    let body_dropped = body_dropped.clone();
                    let bytes_dropped = bytes_dropped.clone();
                    move |request: axum::extract::Request, next: axum::middleware::Next| {
                        let probe = probe.clone();
                        let body_dropped = body_dropped.clone();
                        let bytes_dropped = bytes_dropped.clone();
                        let accepted = accepted.clone();
                        async move {
                            let connection = request
                                .extensions()
                                .get::<crate::SharingConnectionCancellation>()
                                .expect("actual accepted connection")
                                .clone();
                            accepted
                                .lock()
                                .expect("capture")
                                .take()
                                .expect("one request")
                                .send(connection)
                                .ok();
                            let response = next.run(request).await;
                            assert_eq!(response.status(), StatusCode::OK);
                            let (parts, body) = response.into_parts();
                            assert!(parts
                                .extensions
                                .get::<Arc<crate::transcode::source_actor::SourceResponseGuard>>()
                                .is_some());
                            let bytes = axum::body::to_bytes(body, 4 * 1024 * 1024)
                                .await
                                .expect("actual complete Source envelope");
                            assert!(serde_json::from_slice::<Value>(&bytes)
                                .expect("actual envelope")
                                .get("incarnation_id")
                                .is_some());
                            // Test-only controlled backpressure starts after actual
                            // production Start/guard admission. No DTO is fabricated.
                            probe.gate.store(true, Ordering::SeqCst);
                            axum::response::Response::from_parts(
                                parts,
                                axum::body::Body::new(QueuedStartBody {
                                    data: Some(bytes::Bytes::from_owner(QueuedStartBytes {
                                        bytes: bytes.to_vec(),
                                        dropped: bytes_dropped,
                                    })),
                                    dropped: body_dropped,
                                }),
                            )
                        }
                    }
                }),
            );
            let (stop, stopped) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(crate::serve_http(
                GatedStartListener {
                    listener,
                    probe: probe.clone(),
                },
                app,
                async move {
                    let _ = stopped.await;
                },
                crate::HTTP_TIMEOUTS,
            ));
            let mut socket = tokio::net::TcpStream::connect(address)
                .await
                .expect("actual TCP client");
            let path = format!(
                "/sharing/v1/items/{}/files/{}/sessions",
                fixture.reference.item_id.as_str(),
                fixture.reference.file_id.as_str()
            );
            let mut h2_tasks = None;
            if h2 {
                let (mut sender, driver) =
                    hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                        .handshake::<_, axum::body::Body>(hyper_util::rt::TokioIo::new(socket))
                        .await
                        .expect("actual H2 handshake");
                let driver = tokio::spawn(driver);
                let request = axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("http://fixture{path}"))
                    .header(
                        "authorization",
                        fixture.headers.get("authorization").expect("auth"),
                    )
                    .header(
                        "cinemashare-viewer",
                        fixture.headers.get("cinemashare-viewer").expect("viewer"),
                    )
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(fixture.request.clone()))
                    .expect("actual H2 request");
                let send = tokio::spawn(async move { sender.send_request(request).await });
                h2_tasks = Some((driver, send));
            } else {
                let head = format!("POST {path} HTTP/1.1\r\nHost: fixture\r\nAuthorization: {}\r\nCinemaShare-Viewer: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",fixture.headers.get("authorization").expect("auth").to_str().expect("text"),fixture.headers.get("cinemashare-viewer").expect("viewer").to_str().expect("text"),fixture.request.len());
                socket
                    .write_all(head.as_bytes())
                    .await
                    .expect("actual H1 head");
                socket
                    .write_all(&fixture.request)
                    .await
                    .expect("actual H1 request");
            }
            let connection = captured.await.expect("accepted connection");
            tokio::time::timeout(Duration::from_secs(30), async {
                while !probe.blocked.load(Ordering::SeqCst) || !body_dropped.load(Ordering::SeqCst)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("actual Hyper drained body and blocked writer");
            // Small DATA can be copied into Hyper's own write buffer before
            // delivery. The gate passes headers and blocks only when the real
            // Source JSON is presented to the actual accepted writer.
            assert!(
                probe.queued_start.load(Ordering::SeqCst),
                "actual Source envelope remains in blocked H1/H2 writer buffer"
            );
            assert!(!probe.dropped.load(Ordering::SeqCst));
            assert!(!connection.closed().is_closed());
            let entry = fixture
                .state
                .transcode
                .source_http_starts
                .entries
                .lock()
                .expect("actual registry")
                .first()
                .cloned()
                .expect("actual owner");
            let owned = entry
                .wait(Instant::now() + Duration::from_secs(1))
                .await
                .expect("actual published owner");
            let actor = owned.actor.clone();
            assert_eq!(actor.settlement_status(), None);
            let retire = tokio::spawn(async move { actor.retire().await });
            tokio::time::timeout(Duration::from_secs(10), connection.closed().wait())
                .await
                .expect("actual accepted writer dropped");
            assert!(
                probe.dropped.load(Ordering::SeqCst),
                "accepted IO dropped before closure receipt"
            );
            assert!(
                bytes_dropped.load(Ordering::SeqCst),
                "queued actual DATA dropped before closure receipt"
            );
            tokio::time::timeout(Duration::from_secs(10), retire)
                .await
                .expect("physical/body/SQL settlement deadline")
                .expect("retire task")
                .expect("actual settled retirement");
            assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
            if let Some((driver, send)) = h2_tasks {
                driver.abort();
                send.abort();
            }
            stop.send(()).expect("stop");
            server.await.expect("server task").expect("server shutdown");
            fixture.shutdown().await;
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sharing_source_http_start_disconnect_joins_actual_actor_and_fresh_exact_replay() {
        Box::pin(actual_source_http_start()).await;
    }
    async fn actual_source_http_start() {
        use std::time::{Duration, Instant};
        use tokio::io::AsyncWriteExt;
        let fixture = real_source_start_fixture().await;
        let hold = fixture
            .state
            .transcode
            .test_hold_source_software_capacity()
            .expect("actual occupied software capacity");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("private fixture listener");
        let address = listener.local_addr().expect("private address");
        let router = super::super::sharing::peer_router((*fixture.state).clone());
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(crate::serve_http(
            listener,
            router,
            async move {
                let _ = stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        let path = format!(
            "http://{address}/sharing/v1/items/{}/files/{}/sessions",
            fixture.reference.item_id.as_str(),
            fixture.reference.file_id.as_str()
        );
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("actual HTTP client");
        let mut initial = tokio::net::TcpStream::connect(address)
            .await
            .expect("actual initial TCP connection");
        let wire=format!("POST /sharing/v1/items/{}/files/{}/sessions HTTP/1.1\r\nHost: {address}\r\nAuthorization: {}\r\nCinemaShare-Viewer: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",fixture.reference.item_id.as_str(),fixture.reference.file_id.as_str(),fixture.headers.get("authorization").expect("auth").to_str().expect("auth text"),fixture.headers.get("cinemashare-viewer").expect("viewer").to_str().expect("viewer text"),fixture.request.len());
        initial
            .write_all(wire.as_bytes())
            .await
            .expect("actual start headers");
        initial
            .write_all(&fixture.request)
            .await
            .expect("actual start body");
        initial.flush().await.expect("actual start sent");
        let deadline = Instant::now() + Duration::from_secs(10);
        let owned = loop {
            let entry = fixture
                .state
                .transcode
                .source_http_starts
                .entries
                .lock()
                .expect("registry")
                .first()
                .cloned();
            if let Some(entry) = entry {
                let outcome = entry.result.lock().expect("actual owner outcome").clone();
                if let Some(result) = outcome {
                    break result.expect("actual Source actor inserted");
                }
            }
            assert!(
                Instant::now() < deadline,
                "actual HTTP owner must be retained before cancelled waiter"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(fixture.state.transcode.test_software_threads_in_use(), 4);
        assert!(
            fixture
                .state
                .store
                .media_session_route_by_incarnation(
                    &owned.assignment.binding().incarnation_id().to_string()
                )
                .await
                .expect("actual route read")
                .is_none(),
            "occupied admission has no published Source route"
        );
        drop(initial); // Actual TCP EOF drops only the HTTP waiter.
        assert_eq!(
            fixture
                .state
                .transcode
                .source_http_starts
                .entries
                .lock()
                .expect("registry")
                .len(),
            1
        );
        drop(hold);
        let response = tokio::time::timeout(
            Duration::from_secs(20),
            client
                .post(&path)
                .headers(fixture.headers.clone())
                .body(fixture.request.clone())
                .send(),
        )
        .await
        .expect("actual retry deadline")
        .expect("actual HTTP retry");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .expect("private response"),
            "no-store"
        );
        let bytes = response.bytes().await.expect("guarded full Source body");
        let decoded = super::super::decode_source_start_response(&bytes, &fixture.reference)
            .expect("actual complete strict Source envelope");
        assert_eq!(
            decoded.incarnation_id(),
            owned.assignment.binding().incarnation_id()
        );
        let decoded_start = decoded.response();
        assert_eq!(
            decoded_start
                .control
                .as_ref()
                .expect("actual controller")
                .generation,
            decoded.incarnation_id().to_string()
        );
        let route = fixture
            .state
            .store
            .media_session_route_by_incarnation(&decoded.incarnation_id().to_string())
            .await
            .expect("actual route read")
            .expect("actual producer publication");
        assert_eq!(route.session_id, decoded_start.session_id);
        assert_eq!(route.publication_ready_at_ms, 0);
        let retry = client
            .post(&path)
            .headers(fixture.headers.clone())
            .body(fixture.request.clone())
            .send()
            .await
            .expect("exact metadata replay");
        assert_eq!(retry.status(), StatusCode::OK);
        assert_eq!(retry.bytes().await.expect("replay body"), bytes);
        let mut different: Value =
            serde_json::from_slice(&fixture.request).expect("canonical recipe");
        different["session"]["start"] = serde_json::json!(1);
        let conflict = client
            .post(&path)
            .headers(fixture.headers.clone())
            .body(serde_json::to_vec(&different).expect("changed recipe"))
            .send()
            .await
            .expect("conflict HTTP");
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        // A benign rotation authenticates the same stable grant and joins the
        // same actual Source owner; a rotating hash never keys its identity.
        let replacement = plurx_core::sharing::new_secret().expect("rotation credential");
        let new_hash = plurx_core::sharing::secret_hash(
            plurx_core::sharing::SecretDomain::Grant,
            &replacement,
        );
        let (old_hash, _) = current_reference(&fixture.state, &fixture.headers, &fixture.reference)
            .await
            .expect("old current credential");
        assert_eq!(
            fixture
                .state
                .store
                .rotate_share(
                    fixture.grant,
                    Uuid::new_v4(),
                    &old_hash,
                    &new_hash,
                    crate::state::clock_ms()
                )
                .await
                .expect("actual rotation"),
            plurx_core::sharing::MutationOutcome::Applied
        );
        let mut rotated = fixture.headers.clone();
        rotated.insert(
            "authorization",
            format!("CinemaShare {}", replacement.expose())
                .parse()
                .expect("new auth"),
        );
        let after_rotation = client
            .post(&path)
            .headers(rotated.clone())
            .body(fixture.request.clone())
            .send()
            .await
            .expect("rotated exact retry");
        assert_eq!(after_rotation.status(), StatusCode::OK);
        assert_eq!(after_rotation.bytes().await.expect("rotated body"), bytes);
        // An empty manager registry is not readiness or no-activation proof.
        // Keep the original real producer alive, but model lost in-process
        // HTTP/worker ownership with a new actual manager over the same Store.
        let mut untracked = (*fixture.state).clone();
        untracked.transcode = std::sync::Arc::new(crate::transcode::TranscodeManager::new(
            std::sync::Arc::clone(&untracked.store),
            fixture._directory.path().join("untracked-workers"),
            plurx_core::transcode::EncoderCaps::default(),
            plurx_core::transcode::Pipeline::Cpu,
        ));
        let unknown = Box::pin(start(
            axum::extract::State(untracked.clone()),
            rotated.clone(),
            axum::extract::Path((
                fixture.reference.item_id.as_str().to_owned(),
                fixture.reference.file_id.as_str().to_owned(),
            )),
            axum::body::Body::from(fixture.request.clone()),
        ))
        .await
        .expect_err("stored response without actual owner must refuse");
        use axum::response::IntoResponse;
        assert_eq!(
            unknown.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(untracked.transcode.test_software_threads_in_use(), 0);
        assert_eq!(owned.actor.settlement_status(), None);
        assert_eq!(
            untracked
                .transcode
                .source_http_starts
                .entries
                .lock()
                .expect("unresolved registry")
                .len(),
            1
        );
        // Hold the actual returned HTTP Body, not an invented publication
        // flag. The actor cannot settle physical/body/SQL obligations while
        // the complete Start transport still owns this response guard.
        let held = Box::pin(start(
            axum::extract::State((*fixture.state).clone()),
            rotated.clone(),
            axum::extract::Path((
                fixture.reference.item_id.as_str().to_owned(),
                fixture.reference.file_id.as_str().to_owned(),
            )),
            axum::body::Body::from(fixture.request.clone()),
        ))
        .await
        .expect("actual guarded HTTP response");
        let retiring = owned.actor.clone();
        let retirement = tokio::spawn(async move { retiring.retire().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !retirement.is_finished(),
            "actual returned Start Body keeps retirement barrier"
        );
        assert_eq!(owned.actor.settlement_status(), None);
        fixture
            .state
            .store
            .revoke_share(fixture.grant, crate::state::clock_ms())
            .await
            .expect("actual revoke");
        let revoked = client
            .post(&path)
            .headers(rotated)
            .body(fixture.request.clone())
            .send()
            .await
            .expect("revoked retry");
        assert!(!revoked.status().is_success());
        drop(held);
        tokio::time::timeout(Duration::from_secs(15), retirement)
            .await
            .expect("physical settlement deadline")
            .expect("detached retirement task")
            .expect("actual physical/body/SQL settlement");
        assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
        assert_eq!(fixture.state.transcode.test_software_threads_in_use(), 0);
        let _ = stop.send(());
        server
            .await
            .expect("server join")
            .expect("actual server shutdown");
        drop((owned, client));
        fixture.shutdown().await;
    }
    fn fixture() -> Value {
        json!({"reference":{"server_id":Uuid::new_v4(),"catalogue_epoch":Uuid::new_v4(),"library_id":"0","item_id":"9007199254740993","file_id":"9223372036854775807","revision":"a".repeat(64)},"session":{"playback_id":"actual-client-instance","request_id":Uuid::new_v4(),"copy":true,"presentation":"vod","start":0,"caps":{"v":2,"video":[{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"]}}})
    }
    fn parse(value: &Value) -> Result<SourceStartInput, ApiError> {
        parse_start_request(
            &serde_json::to_vec(value).expect("canonical JSON"),
            "9007199254740993",
            "9223372036854775807",
        )
    }
    #[test]
    fn sharing_source_start_request_preserves_complete_actual_create_session_and_exact_source_ids()
    {
        let value = fixture();
        let input = parse(&value).expect("actual Source start body");
        assert_eq!(input.reference.library_id.as_str(), "0");
        assert_eq!(input.reference.item_id.as_str(), "9007199254740993");
        assert_eq!(input.reference.file_id.as_str(), "9223372036854775807");
        assert_eq!(
            input.session.request_id.as_deref(),
            Some(input.request_id.to_string().as_str())
        );
        assert_eq!(
            input.canonical_recipe,
            serde_json::to_vec(&value).expect("recipe")
        );
    }
    #[test]
    fn sharing_source_start_request_refuses_unknown_nested_fields_duplicates_and_path_drift() {
        let value = fixture();
        for path in ["", "/session", "/session/caps", "/session/caps/video/0"] {
            let mut bad = value.clone();
            let field = if path.is_empty() {
                &mut bad
            } else {
                bad.pointer_mut(path).expect("path")
            };
            field["foreign_authority"] = json!(true);
            assert!(parse(&bad).is_err(), "unknown {path}");
        }
        assert!(parse_start_request(
            &serde_json::to_vec(&value).expect("JSON"),
            "1",
            "9223372036854775807"
        )
        .is_err());
        let duplicate = format!(
            "{{\"reference\":{},\"session\":{},\"session\":{}}}",
            value["reference"], value["session"], value["session"]
        );
        assert!(parse_start_request(
            duplicate.as_bytes(),
            "9007199254740993",
            "9223372036854775807"
        )
        .is_err());
        let mut bad = value;
        bad["session"]["request_id"] = json!(Uuid::nil());
        assert!(parse(&bad).is_err());
    }
    #[test]
    fn sharing_source_start_viewer_requires_single_sensitive_hash_shape() {
        let mut headers = HeaderMap::new();
        headers.insert("cinemashare-viewer", "a".repeat(64).parse().expect("hash"));
        assert_eq!(viewer_hash(&headers).expect("viewer"), "a".repeat(64));
        headers.append(
            "cinemashare-viewer",
            "b".repeat(64).parse().expect("duplicate"),
        );
        assert!(viewer_hash(&headers).is_err());
        for invalid in ["A".repeat(64), "g".repeat(64), "a".repeat(63)] {
            headers.remove("cinemashare-viewer");
            headers.insert("cinemashare-viewer", invalid.parse().expect("header"));
            assert!(viewer_hash(&headers).is_err());
        }
    }
    #[test]
    fn sharing_source_start_registry_binds_full_identity_and_keeps_owner_after_waiter_drop() {
        let value = fixture();
        let input = parse(&value).expect("request");
        let grant = Uuid::new_v4();
        let viewer = "a".repeat(64);
        let registry = SourceStartRegistry::default();
        let (first, new) = registry
            .register(grant, &viewer, &input, &"d".repeat(64))
            .expect("first");
        assert!(new);
        let pointer = std::sync::Arc::as_ptr(&first);
        drop(first);
        let (retry, new) = registry
            .register(grant, &viewer, &input, &"d".repeat(64))
            .expect("retry");
        assert!(!new);
        assert_eq!(std::sync::Arc::as_ptr(&retry), pointer);
        let mut changed = value.clone();
        changed["session"]["quality_auto"] = json!(true);
        assert!(matches!(
            registry.register(
                grant,
                &viewer,
                &parse(&changed).expect("changed body"),
                &"d".repeat(64)
            ),
            Err(SourceStartFailure::Conflict)
        ));
        let mut changed = value;
        changed["reference"]["revision"] = json!("b".repeat(64));
        assert!(matches!(
            registry.register(
                grant,
                &viewer,
                &parse(&changed).expect("changed reference"),
                &"d".repeat(64)
            ),
            Err(SourceStartFailure::Conflict)
        ));
        assert!(
            registry
                .register(Uuid::new_v4(), &viewer, &input, &"d".repeat(64))
                .expect("other grant")
                .1
        );
        assert!(
            registry
                .register(grant, &"b".repeat(64), &input, &"d".repeat(64))
                .expect("other viewer")
                .1
        );
        let second = SourceStartRegistry::default();
        let (other, new) = second
            .register(grant, &viewer, &input, &"d".repeat(64))
            .expect("independent registry");
        assert!(new);
        assert!(!std::sync::Arc::ptr_eq(&retry, &other));
    }
    #[test]
    fn sharing_source_start_registry_refuses_ninth_retained_owner_without_evicting_unknown_requests(
    ) {
        let registry = SourceStartRegistry::default();
        let grant = Uuid::new_v4();
        let viewer = "a".repeat(64);
        let mut value = fixture();
        for _ in 0..8 {
            value["session"]["request_id"] = json!(Uuid::new_v4());
            let input = parse(&value).expect("request");
            assert!(
                registry
                    .register(grant, &viewer, &input, &"d".repeat(64))
                    .expect("bounded entry")
                    .1
            );
        }
        value["session"]["request_id"] = json!(Uuid::new_v4());
        assert!(matches!(
            registry.register(
                grant,
                &viewer,
                &parse(&value).expect("ninth"),
                &"d".repeat(64)
            ),
            Err(SourceStartFailure::Capacity)
        ));
        assert_eq!(registry.entries.lock().expect("entries").len(), 8);
    }
    #[test]
    fn sharing_source_start_registry_moves_a_joined_failed_start_to_the_settled_cache() {
        let registry = SourceStartRegistry::default();
        let grant = Uuid::new_v4();
        let viewer = "a".repeat(64);
        let mut value = fixture();
        let mut retained = Vec::new();
        for _ in 0..8 {
            value["session"]["request_id"] = json!(Uuid::new_v4());
            let input = parse(&value).expect("request");
            let (entry, new) = registry
                .register(grant, &viewer, &input, &"d".repeat(64))
                .expect("bounded entry");
            assert!(new);
            retained.push((input, entry));
        }
        let (failed_input, failed) = retained.remove(0);
        // The supervisor publishes an outcome only after its worker joined.
        // A failure there handed no actor to this entry, so nothing in this
        // process still works for it.
        *failed.result.lock().expect("Source HTTP outcome") =
            Some(Err(SourceStartFailure::Unavailable));
        value["session"]["request_id"] = json!(Uuid::new_v4());
        assert!(
            registry
                .register(
                    grant,
                    &viewer,
                    &parse(&value).expect("ninth"),
                    &"d".repeat(64)
                )
                .expect("the failed start's slot is free")
                .1
        );
        assert_eq!(registry.entries.lock().expect("entries").len(), 8);
        assert_eq!(registry.settled.lock().expect("cache").len(), 1);
        let (replay, new) = registry
            .register(grant, &viewer, &failed_input, &"d".repeat(64))
            .expect("the failed receipt still answers its request");
        assert!(!new);
        assert!(std::sync::Arc::ptr_eq(&replay, &failed));
    }
}

#[cfg(test)]
#[path = "sharing_source_adapter_tests.rs"]
mod actor_adapter_tests;
