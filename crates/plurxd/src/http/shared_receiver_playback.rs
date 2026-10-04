//! B owns its relay attempt independently of HTTP waiters.
use super::{hls::StartResponse, sharing_playback_wire::project_shared_start};
use crate::state::{clock_ms, AppState};
use plurx_core::{
    domain::{MediaSessionActivation, MediaSessionRequestClaim, MEDIA_SESSION_PUBLICATION_BLOCKED},
    playback_principal::PlaybackPrincipal,
    secrets::SharingSecretPurpose,
    sharing_receiver_sessions::{
        ReceiverPendingRenewal, ReceiverSessionIntent, ReceiverSourceAttachment,
        ReceiverSourceBinding, ReceiverSourceOwner, ReceiverSourcePublication,
        ReceiverSourceRenewal, ReceiverSourceWrite,
    },
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;
#[path = "shared_receiver_retirement.rs"]
mod retirement;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReceiverStartError {
    Unavailable,
    Capacity,
    Conflict,
    Unresolved,
    Deadline,
    Unsupported,
}
#[derive(Default)]
pub(crate) struct ReceiverStartRegistry {
    entries: Mutex<Vec<Arc<ReceiverStartInner>>>,
    settled: Mutex<Vec<SettledReceiverAttempt>>,
}
struct SettledReceiverAttempt {
    user_id: i64,
    request_id: String,
    login_hash: String,
    fingerprint: String,
}
struct ReceiverStartInner {
    intent: ReceiverSessionIntent,
    peer_session: crate::sharing_client::SourcePeerSession,
    request_id: String,
    fingerprint: String,
    state: Mutex<ReceiverStartState>,
    start_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    stop: tokio_util::sync::CancellationToken,
    bodies: Arc<retirement::ReceiverBodyRegistry>,
    retirement_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    changed: tokio::sync::Notify,
}
#[derive(Clone, Default)]
enum ReceiverClaimStage {
    #[default]
    NotAttempted,
    Claiming,
    NotAcquired,
    Acquired,
    Assigning(String),
    Assigned(String),
}
#[derive(Default)]
struct ReceiverStartState {
    claim: ReceiverClaimStage,
    start: Option<Result<StartResponse, ReceiverStartError>>,
    // Never discarded on publication failure or loss of the original login.
    source: Option<ReceiverSourceAttachment>,
    received: Option<Arc<ReceivedSource>>,
    dispatched: Option<Arc<DispatchedSource>>,
    confirmed_source_end: Option<Arc<crate::sharing_client::SourceEndReceipt>>,
    dispatch_closed: bool,
    owner: Option<ReceiverSourceOwner>,
    planned_activation: Option<MediaSessionActivation>,
    retirement_started: bool,
    retired: bool,
}
// The only constructor joins the exact registry-owned Start task. This is
// neither Source settlement nor accepted B body/writer completion.
struct JoinedReceiverStart(Arc<ReceiverStartInner>);
impl ReceiverStartInner {
    fn retain_dispatch(
        &self,
        dispatched: DispatchedSource,
    ) -> Result<(), crate::sharing_client::PeerError> {
        let mut owner = self.state.lock().expect("receiver owner");
        if owner.dispatch_closed || owner.dispatched.is_some() {
            return Err(crate::sharing_client::PeerError::Unavailable);
        }
        owner.dispatched = Some(Arc::new(dispatched));
        Ok(())
    }
}
struct DispatchedSource {
    credential: plurx_core::secrets::Secret,
    viewer_hash: String,
    endpoint: plurx_core::sharing::Endpoint,
}
struct ReceivedSource {
    credential: plurx_core::secrets::Secret,
    viewer_hash: String,
    endpoint: plurx_core::sharing::Endpoint,
    incarnation: Uuid,
    response: StartResponse,
}
#[derive(Clone)]
pub(crate) struct ReceiverStartActor(Arc<ReceiverStartInner>);
impl ReceiverStartRegistry {
    pub(crate) fn begin(
        &self,
        state: Arc<AppState>,
        intent: ReceiverSessionIntent,
        request_id: String,
        playback_id: String,
        source_wrapper: String,
    ) -> Result<ReceiverStartActor, ReceiverStartError> {
        let (entry, created) = self.register(intent, request_id, &playback_id, &source_wrapper)?;
        if !created {
            return Ok(ReceiverStartActor(entry));
        }
        // The owner is inserted before the first claim, activation or Source send.
        // Dropping an HTTP waiter never drops the owned producer obligation.
        let owner = entry.clone();
        let (installed, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            // Cleanup cannot race the installation of the exact task it joins.
            if ready.await.is_err() {
                return;
            }
            let result = run_owner(state.clone(), owner.clone(), playback_id, source_wrapper).await;
            if let Err(error) = result {
                owner.state.lock().expect("receiver owner").start = Some(Err(error));
                owner.changed.notify_waiters();
                ReceiverStartActor(owner).begin_retirement(
                    state,
                    plurx_core::sharing_receiver_retirement::ReceiverRetirementReason::Revoked,
                );
            }
        });
        *entry.start_task.lock().expect("receiver start task") = Some(task);
        let _ = installed.send(());
        Ok(ReceiverStartActor(entry))
    }
    fn register(
        &self,
        intent: ReceiverSessionIntent,
        request_id: String,
        playback_id: &str,
        source_wrapper: &str,
    ) -> Result<(Arc<ReceiverStartInner>, bool), ReceiverStartError> {
        if !crate::sharing::receiver_initial_request_supported(&intent.recipe.request_json) {
            return Err(ReceiverStartError::Unsupported);
        }
        let reference = crate::sharing::receiver_source_request(&intent, source_wrapper)
            .map_err(|_| ReceiverStartError::Conflict)?;
        let peer_session =
            crate::sharing_client::SourcePeerSession::new(reference, source_wrapper.as_bytes())
                .map_err(|_| ReceiverStartError::Conflict)?;
        let original: serde_json::Value = serde_json::from_str(&intent.recipe.request_json)
            .map_err(|_| ReceiverStartError::Conflict)?;
        if original
            .get("playback_id")
            .and_then(serde_json::Value::as_str)
            != Some(playback_id)
            || original
                .get("request_id")
                .and_then(serde_json::Value::as_str)
                != Some(&request_id)
        {
            return Err(ReceiverStartError::Conflict);
        }
        if request_id.is_empty()
            || request_id.len() > 128
            || request_id.chars().any(char::is_control)
            || playback_id.is_empty()
            || playback_id.len() > 128
            || intent.source_position_ms != 0
        {
            return Err(ReceiverStartError::Conflict);
        }
        let fingerprint = intent
            .recipe
            .request_fingerprint()
            .map_err(|_| ReceiverStartError::Conflict)?;
        let mut entries = self.entries.lock().expect("receiver starts");
        // Only the detached retirement owner can mark an entry retired after
        // actual joins and Applied/Replay. Keep bounded non-authorizing
        // tombstones so exact retries cannot recreate an already ended actor.
        let mut settled = self.settled.lock().expect("settled receiver attempts");
        entries.retain(|entry| {
            if !entry.state.lock().expect("receiver owner").retired {
                return true;
            }
            if settled.len() == 64 {
                settled.remove(0);
            }
            settled.push(SettledReceiverAttempt {
                user_id: entry.intent.user_id,
                request_id: entry.request_id.clone(),
                login_hash: entry.intent.login_hash.clone(),
                fingerprint: entry.fingerprint.clone(),
            });
            false
        });
        if let Some(previous) = settled
            .iter()
            .find(|s| s.user_id == intent.user_id && s.request_id == request_id)
        {
            return Err(
                if previous.fingerprint == fingerprint && previous.login_hash == intent.login_hash {
                    ReceiverStartError::Unresolved
                } else {
                    ReceiverStartError::Conflict
                },
            );
        }
        drop(settled);
        if let Some(entry) = entries
            .iter()
            .find(|entry| entry.intent.user_id == intent.user_id && entry.request_id == request_id)
        {
            if entry.fingerprint != fingerprint || entry.intent.login_hash != intent.login_hash {
                return Err(ReceiverStartError::Conflict);
            }
            return Ok((entry.clone(), false));
        }
        if entries.len() >= 8 {
            return Err(ReceiverStartError::Capacity);
        }
        let entry = Arc::new(ReceiverStartInner {
            intent,
            peer_session,
            request_id,
            fingerprint,
            state: Mutex::new(ReceiverStartState::default()),
            start_task: Mutex::new(None),
            stop: tokio_util::sync::CancellationToken::new(),
            bodies: Arc::new(retirement::ReceiverBodyRegistry::default()),
            retirement_task: Mutex::new(None),
            changed: tokio::sync::Notify::new(),
        });
        entries.push(entry.clone());
        Ok((entry, true))
    }
}
impl ReceiverStartActor {
    fn close_dispatch(&self) {
        self.0.state.lock().expect("receiver owner").dispatch_closed = true;
        self.0.stop.cancel();
    }
    // Only an independently owned retirement task may await this operation.
    // A missing handle, panic or cancelled task remains unresolved.
    async fn join_start_for_cleanup(&self) -> Result<JoinedReceiverStart, ReceiverStartError> {
        self.close_dispatch();
        let task = self
            .0
            .start_task
            .lock()
            .expect("receiver start task")
            .take()
            .ok_or(ReceiverStartError::Unresolved)?;
        task.await.map_err(|_| ReceiverStartError::Unresolved)?;
        Ok(JoinedReceiverStart(self.0.clone()))
    }
    // Called only by the independently owned retirement task. This exchange
    // retains Source facts; it cannot release B metadata or body ownership.
    async fn request_source_end(
        &self,
        state: &AppState,
        joined: &JoinedReceiverStart,
    ) -> Result<Arc<crate::sharing_client::SourceEndReceipt>, ReceiverStartError> {
        if !Arc::ptr_eq(&self.0, &joined.0) {
            return Err(ReceiverStartError::Unresolved);
        }
        let (dispatched, received) = {
            let owner = self.0.state.lock().expect("receiver owner");
            if let Some(receipt) = &owner.confirmed_source_end {
                return Ok(receipt.clone());
            }
            (
                owner
                    .dispatched
                    .clone()
                    .ok_or(ReceiverStartError::Unresolved)?,
                owner.received.clone(),
            )
        };
        let known = received
            .as_ref()
            .map(|source| {
                crate::sharing_client::SourcePeerLineage::from_start(
                    source.incarnation,
                    &source.response,
                )
            })
            .transpose()
            .map_err(|_| ReceiverStartError::Unresolved)?;
        let mut connection = crate::sharing_client::CleanupPeerConnection::connect(
            &state.sharing,
            &dispatched.endpoint,
        )
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?;
        let receipt = Arc::new(
            connection
                .end(
                    &dispatched.credential,
                    &dispatched.viewer_hash,
                    &self.0.peer_session,
                    known.as_ref(),
                )
                .await
                .map_err(|_| ReceiverStartError::Unresolved)?,
        );
        // Preserve the authenticated result before a subsequent Store await.
        let mut owner = self.0.state.lock().expect("receiver owner");
        Ok(owner.confirmed_source_end.get_or_insert(receipt).clone())
    }
    pub(crate) async fn wait_ready(
        &self,
        deadline: Instant,
    ) -> Result<StartResponse, ReceiverStartError> {
        loop {
            if self.0.stop.is_cancelled() {
                return Err(ReceiverStartError::Unresolved);
            }
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(result) = self.0.state.lock().expect("receiver owner").start.clone() {
                return result;
            }
            tokio::time::timeout_at(deadline.into(), changed)
                .await
                .map_err(|_| ReceiverStartError::Deadline)?;
        }
    }
}
async fn run_owner(
    state: Arc<AppState>,
    entry: Arc<ReceiverStartInner>,
    playback_id: String,
    source_wrapper: String,
) -> Result<(), ReceiverStartError> {
    let intent = &entry.intent;
    let principal = PlaybackPrincipal::LocalUser {
        user_id: intent.user_id,
    };
    let incarnation = intent.recipe.source_request_id;
    let now = clock_ms();
    let _authority = state
        .store
        .prepare_receiver_session_authority(intent.clone())
        .await
        .map_err(|_| ReceiverStartError::Unavailable)?
        .ok_or(ReceiverStartError::Unavailable)?;
    entry.state.lock().expect("receiver owner").claim = ReceiverClaimStage::Claiming;
    match state
        .store
        .claim_media_session_request(
            &principal,
            &entry.request_id,
            &entry.fingerprint,
            &playback_id,
            &incarnation.to_string(),
            now,
            now + 30_000,
        )
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
    {
        MediaSessionRequestClaim::Acquired { incarnation_id }
            if incarnation_id == incarnation.to_string() =>
        {
            entry.state.lock().expect("receiver owner").claim = ReceiverClaimStage::Acquired;
        }
        // A durable reply or missing process-local actor is never producer proof.
        MediaSessionRequestClaim::InFlight { .. } | MediaSessionRequestClaim::Resolved(_) => {
            entry.state.lock().expect("receiver owner").claim = ReceiverClaimStage::NotAcquired;
            return Err(ReceiverStartError::Unresolved);
        }
        MediaSessionRequestClaim::Conflict | MediaSessionRequestClaim::Overloaded => {
            entry.state.lock().expect("receiver owner").claim = ReceiverClaimStage::NotAcquired;
            return Err(ReceiverStartError::Conflict);
        }
        MediaSessionRequestClaim::Acquired { .. } => return Err(ReceiverStartError::Unresolved),
    }
    entry.state.lock().expect("receiver owner").claim =
        ReceiverClaimStage::Assigning(state.node_id.clone());
    if !state
        .store
        .assign_media_session_request_owner(
            &principal,
            &entry.request_id,
            &incarnation.to_string(),
            &state.node_id,
            clock_ms(),
        )
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
    {
        return Err(ReceiverStartError::Unresolved);
    }
    entry.state.lock().expect("receiver owner").claim =
        ReceiverClaimStage::Assigned(state.node_id.clone());
    // Full-film Source copy is anchored at zero; requested resume is retained
    // only in the actual complete client request, never invented as an origin.
    let now = clock_ms();
    let activation = MediaSessionActivation {
        incarnation_id: incarnation.to_string(),
        session_id: Uuid::new_v4().to_string(),
        principal,
        playback_id,
        recovery_epoch: Uuid::new_v4().to_string(),
        expected_predecessor_incarnation_id: None,
        fence_predecessor: true,
        request_id: Some(entry.request_id.clone()),
        request_fingerprint: entry.fingerprint.clone(),
        owner_node_id: state.node_id.clone(),
        recipe_json: serde_json::to_string(&intent.recipe)
            .map_err(|_| ReceiverStartError::Conflict)?,
        response_json: "{}".into(),
        publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms: 0,
        now_ms: now,
        lease_expires_at_ms: now + 30_000,
        expected_desired_revision: None,
    };
    let authority = state
        .store
        .prepare_receiver_session_authority(intent.clone())
        .await
        .map_err(|_| ReceiverStartError::Unavailable)?
        .ok_or(ReceiverStartError::Unavailable)?;
    // Retain the planned immutable tuple before the activation await so an
    // unknown commit cannot erase the metadata cleanup obligation.
    entry
        .state
        .lock()
        .expect("receiver owner")
        .planned_activation = Some(activation.clone());
    let route = state
        .store
        .activate_receiver_media_session(&authority, &activation)
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        .ok_or(ReceiverStartError::Conflict)?
        .route;
    entry.state.lock().expect("receiver owner").owner = Some(ReceiverSourceOwner {
        incarnation_id: incarnation,
        session_id: Uuid::parse_str(&route.session_id)
            .map_err(|_| ReceiverStartError::Unresolved)?,
        owner_node_id: state.node_id.clone(),
        owner_epoch: route.owner_epoch,
        request_id: entry.request_id.clone(),
        now_ms: clock_ms(),
        lease_expires_at_ms: route.lease_expires_at_ms,
    });
    let mut pending = ReceiverPendingRenewal {
        incarnation_id: route.incarnation_id.clone(),
        owner_node_id: state.node_id.clone(),
        owner_epoch: route.owner_epoch,
        request_id: entry.request_id.clone(),
        now_ms: clock_ms(),
        lease_expires_at_ms: route.lease_expires_at_ms,
    };
    let dispatch_owner = pending.clone();
    let dispatch_entry = entry.clone();
    let start = state.sharing.start_file_source(
        &state,
        intent,
        &dispatch_owner,
        &source_wrapper,
        move |credential, viewer, endpoint| {
            dispatch_entry.retain_dispatch(DispatchedSource {
                credential: plurx_core::secrets::Secret::from_cleartext(credential.expose()),
                viewer_hash: viewer.to_owned(),
                endpoint: endpoint.clone(),
            })
        },
    );
    tokio::pin!(start);
    let mut timer = tokio::time::interval(Duration::from_secs(10));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut pending_authorized = true;
    let result = loop {
        tokio::select! {
            result = &mut start => break result.map_err(|_| ReceiverStartError::Unresolved)?,
            _ = entry.stop.cancelled(), if pending_authorized => pending_authorized = false,
            _ = timer.tick(), if pending_authorized => {
                let renewed = async {
                    let authority = state.store.prepare_receiver_session_authority(intent.clone()).await
                        .map_err(|_| ReceiverStartError::Unresolved)?.ok_or(ReceiverStartError::Unresolved)?;
                    pending.now_ms = clock_ms();
                    pending.lease_expires_at_ms = pending.now_ms + 30_000;
                    state.store.renew_pending_receiver_session(&authority, &pending).await
                        .map_err(|_| ReceiverStartError::Unresolved)
                }.await;
                if renewed != Ok(true) {
                    // Losing B authority cannot cancel an already-sent Source
                    // Start and discard its eventual physical cleanup handle.
                    pending_authorized = false;
                } else if let Some(owner) = &mut entry.state.lock().expect("receiver owner").owner {
                    owner.now_ms = pending.now_ms;
                    owner.lease_expires_at_ms = pending.lease_expires_at_ms;
                }
            }
        }
    };
    let (_, source_incarnation, response) = result.source.into_parts();
    let received = Arc::new(ReceivedSource {
        credential: result.credential,
        viewer_hash: result.viewer_hash,
        endpoint: result.endpoint,
        incarnation: source_incarnation,
        response,
    });
    entry.state.lock().expect("receiver owner").received = Some(received.clone());
    if entry.stop.is_cancelled() {
        return Err(ReceiverStartError::Unresolved);
    }
    if received.response.media_origin_ms != Some(0) || !received.response.vod {
        return Err(ReceiverStartError::Unresolved);
    }
    let source_session = Uuid::parse_str(&received.response.session_id)
        .map_err(|_| ReceiverStartError::Unresolved)?;
    let source_epoch = received
        .response
        .control
        .as_ref()
        .ok_or(ReceiverStartError::Unresolved)?
        .control_epoch;
    let local = state
        .store
        .sharing_identity(clock_ms())
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?;
    let capability = plurx_core::secrets::Secret::from_cleartext(
        serde_json::to_string(&serde_json::json!({
            "version":1, "reference":intent.recipe.reference, "file_id":intent.recipe.file_id,
            "file_revision":intent.recipe.file_revision, "source_request_id":incarnation,
            "source_session_id":source_session, "source_incarnation_id":source_incarnation,
            "source_owner_epoch":source_epoch, "viewer_hash":received.viewer_hash,
            "endpoint":received.endpoint,
            "credential":received.credential.expose(),
        }))
        .map_err(|_| ReceiverStartError::Unresolved)?,
    );
    let envelope = state
        .sharing
        .key
        .seal_sharing(
            SharingSecretPurpose::Upstream,
            local.server_id,
            intent.scope.import_id,
            capability.expose(),
        )
        .map_err(|_| ReceiverStartError::Unresolved)?;
    let current = state
        .store
        .media_session_route_by_incarnation(&route.incarnation_id)
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        .ok_or(ReceiverStartError::Unresolved)?;
    let mut attachment = ReceiverSourceAttachment {
        owner: ReceiverSourceOwner {
            incarnation_id: incarnation,
            session_id: Uuid::parse_str(&route.session_id)
                .map_err(|_| ReceiverStartError::Unresolved)?,
            owner_node_id: state.node_id.clone(),
            owner_epoch: route.owner_epoch,
            request_id: entry.request_id.clone(),
            now_ms: clock_ms(),
            lease_expires_at_ms: current.lease_expires_at_ms,
        },
        binding: ReceiverSourceBinding {
            reference: intent.recipe.reference.clone(),
            file_id: intent.recipe.file_id.clone(),
            file_revision: intent.recipe.file_revision.clone(),
            source_request_id: incarnation,
            source_session_id: source_session,
            source_incarnation_id: source_incarnation,
            capability_envelope: envelope,
        },
    };
    // Retain the received physical lineage before any authority or Store await.
    entry.state.lock().expect("receiver owner").source = Some(attachment.clone());
    let projected = project_shared_start(
        received.response.clone(),
        source_session,
        attachment.owner.session_id,
        incarnation,
        attachment.owner.owner_epoch,
    )
    .map_err(|_| ReceiverStartError::Unresolved)?;
    let authority = state
        .store
        .prepare_receiver_session_authority(intent.clone())
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        .ok_or(ReceiverStartError::Unresolved)?;
    if state
        .store
        .attach_receiver_source(&authority, &attachment)
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        == ReceiverSourceWrite::Refused
    {
        return Err(ReceiverStartError::Unresolved);
    }
    let response_json = serde_json::to_string(
        &serde_json::to_value(&projected).map_err(|_| ReceiverStartError::Unresolved)?,
    )
    .map_err(|_| ReceiverStartError::Unresolved)?;
    let authority = state
        .store
        .prepare_receiver_session_authority(intent.clone())
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        .ok_or(ReceiverStartError::Unresolved)?;
    attachment.owner.now_ms = clock_ms();
    if state
        .store
        .publish_receiver_source(
            &authority,
            &ReceiverSourcePublication {
                attachment: attachment.clone(),
                response_json,
            },
        )
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        == ReceiverSourceWrite::Refused
    {
        return Err(ReceiverStartError::Unresolved);
    }
    {
        let mut owned = entry.state.lock().expect("receiver owner");
        if owned.dispatch_closed {
            return Err(ReceiverStartError::Unresolved);
        }
        owned.start = Some(Ok(projected));
    }
    entry.changed.notify_waiters();
    loop {
        tokio::select! {
            _ = entry.stop.cancelled() => return Err(ReceiverStartError::Unresolved),
            _ = timer.tick() => {}
        }
        let authority = state
            .store
            .prepare_receiver_session_authority(intent.clone())
            .await
            .map_err(|_| ReceiverStartError::Unresolved)?
            .ok_or(ReceiverStartError::Unresolved)?;
        attachment.owner.now_ms = clock_ms();
        let new_lease = attachment.owner.now_ms + 30_000;
        let renewal = ReceiverSourceRenewal {
            attachment: attachment.clone(),
            lease_expires_at_ms: new_lease,
        };
        if state
            .store
            .renew_receiver_source_session(&authority, &renewal)
            .await
            .map_err(|_| ReceiverStartError::Unresolved)?
            == ReceiverSourceWrite::Refused
        {
            return Err(ReceiverStartError::Unresolved);
        }
        attachment.owner.lease_expires_at_ms = new_lease;
        let mut owned = entry.state.lock().expect("receiver owner");
        owned.owner = Some(attachment.owner.clone());
        owned.source = Some(attachment.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::{
        sharing::SourceId,
        sharing_catalogue::SharedReference,
        sharing_catalogue_details::FileRevision,
        sharing_receiver_sessions::{ReceiverProducerKind, RemoteSourceRecipe},
        store::sharing_catalogue::ReceiverCatalogueScope,
    };
    fn intent(request: &str) -> ReceiverSessionIntent {
        let reference = SharedReference {
            import_id: Uuid::new_v4(),
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            library_id: SourceId::parse("0").expect("library"),
            item_id: SourceId::parse("9007199254740993").expect("item"),
        };
        ReceiverSessionIntent {
            scope: ReceiverCatalogueScope {
                import_id: reference.import_id,
                source_server_id: reference.server_id,
                catalogue_epoch: reference.catalogue_epoch,
                lifecycle_generation: 1,
                assignment_generation: 1,
                endpoint_generation: 1,
                claim_id: Uuid::new_v4(),
                remote_grant_id: Uuid::new_v4(),
                libraries: vec![reference.library_id.clone()],
            },
            user_id: 1,
            login_hash: "a".repeat(64),
            source_position_ms: 0,
            recipe: RemoteSourceRecipe {
                kind: ReceiverProducerKind::RemoteSource,
                version: 1,
                reference,
                lifecycle_generation: 1,
                file_id: SourceId::parse("0").expect("file"),
                file_revision: FileRevision::parse(&"b".repeat(64)).expect("revision"),
                source_request_id: Uuid::new_v4(),
                parent_login_hash: "a".repeat(64),
                request_json: serde_json::to_string(&serde_json::json!({
                    "playback_id":"player", "request_id":request, "start":30.5, "copy":true,
                }))
                .expect("complete retained fixture recipe"),
            },
        }
    }
    fn wrapper(intent: &ReceiverSessionIntent) -> String {
        let recipe = &intent.recipe;
        let target = super::super::hls::SourcePlaybackTarget {
            server_id: recipe.reference.server_id,
            catalogue_epoch: recipe.reference.catalogue_epoch,
            library_id: recipe.reference.library_id.clone(),
            item_id: recipe.reference.item_id.clone(),
            file_id: recipe.file_id.clone(),
            revision: recipe.file_revision.clone(),
        };
        let mut session: serde_json::Value =
            serde_json::from_str(&recipe.request_json).expect("recipe");
        session["request_id"] = recipe.source_request_id.to_string().into();
        serde_json::to_string(&serde_json::json!({"reference":target,"session":session}))
            .expect("wrapper")
    }
    #[test]
    fn sharing_receiver_initial_start_refuses_controls_before_registration() {
        let registry = ReceiverStartRegistry::default();
        for (field, value) in [
            ("previous_session_id", serde_json::json!(Uuid::new_v4())),
            ("control_sequence", serde_json::json!(1)),
            ("reopen_reason", serde_json::json!("stall")),
            ("intent", serde_json::json!({"version":1})),
        ] {
            let mut requested = intent("attempt");
            let mut original: serde_json::Value =
                serde_json::from_str(&requested.recipe.request_json).expect("original request");
            original[field] = value;
            requested.recipe.request_json = original.to_string();
            assert!(
                matches!(
                    registry.register(
                        requested.clone(),
                        "attempt".into(),
                        "player",
                        &wrapper(&requested)
                    ),
                    Err(ReceiverStartError::Unsupported)
                ),
                "{field}"
            );
            assert!(registry.entries.lock().expect("registry").is_empty());
        }
        let mut requested = intent("attempt");
        let mut original: serde_json::Value =
            serde_json::from_str(&requested.recipe.request_json).expect("original request");
        for field in [
            "previous_session_id",
            "control_sequence",
            "reopen_reason",
            "intent",
        ] {
            original[field] = serde_json::Value::Null;
        }
        requested.recipe.request_json = original.to_string();
        assert!(registry
            .register(
                requested.clone(),
                "attempt".into(),
                "player",
                &wrapper(&requested)
            )
            .is_ok());
    }
    #[tokio::test]
    async fn sharing_receiver_failed_preclaim_owner_joins_before_inert_slot_retirement() {
        let registry = ReceiverStartRegistry::default();
        let request = intent("attempt");
        let state = Arc::new(crate::http::source_actor_test_state());
        let actor = registry
            .begin(
                state.clone(),
                request.clone(),
                "attempt".into(),
                "player".into(),
                wrapper(&request),
            )
            .expect("owned attempt");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let changed = actor.0.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if actor.0.state.lock().expect("owner").retired {
                    break;
                }
                changed.await;
            }
        })
        .await
        .expect("actual owned error and join");
        {
            let owned = actor.0.state.lock().expect("owner");
            assert!(matches!(owned.claim, ReceiverClaimStage::NotAttempted));
            assert!(
                owned.dispatch_closed
                    && owned.dispatched.is_none()
                    && owned.planned_activation.is_none()
            );
        }
        assert!(actor.0.start_task.lock().expect("Start handle").is_none());
        assert!(state
            .store
            .media_session_route_by_incarnation(&request.recipe.source_request_id.to_string())
            .await
            .expect("actual route read")
            .is_none());
        assert!(matches!(
            registry.register(
                request.clone(),
                "attempt".into(),
                "player",
                &wrapper(&request)
            ),
            Err(ReceiverStartError::Unresolved)
        ));
    }
    #[test]
    fn sharing_receiver_registry_exact_retry_retains_original_source_obligation() {
        let registry = ReceiverStartRegistry::default();
        let first = intent("attempt");
        let (entry, created) = registry
            .register(first.clone(), "attempt".into(), "player", &wrapper(&first))
            .expect("first owner");
        assert!(created);
        let mut retry = first.clone();
        retry.recipe.source_request_id = Uuid::new_v4();
        let (joined, created) = registry
            .register(retry.clone(), "attempt".into(), "player", &wrapper(&retry))
            .expect("exact retry");
        assert!(!created);
        assert!(Arc::ptr_eq(&entry, &joined));
        assert_eq!(
            joined.intent.recipe.source_request_id,
            first.recipe.source_request_id
        );
        let mut changed = first.clone();
        changed.recipe.request_json = changed.recipe.request_json.replace("30.5", "31.5");
        assert!(matches!(
            registry.register(
                changed.clone(),
                "attempt".into(),
                "player",
                &wrapper(&changed)
            ),
            Err(ReceiverStartError::Conflict)
        ));
        changed = first.clone();
        changed.login_hash = "c".repeat(64);
        changed.recipe.parent_login_hash = changed.login_hash.clone();
        assert!(matches!(
            registry.register(
                changed.clone(),
                "attempt".into(),
                "player",
                &wrapper(&changed)
            ),
            Err(ReceiverStartError::Conflict)
        ));
        assert!(registry
            .register(
                first.clone(),
                "different-client-key".into(),
                "player",
                &wrapper(&first)
            )
            .is_err());
        assert!(registry
            .register(
                first.clone(),
                "attempt".into(),
                "different-player",
                &wrapper(&first)
            )
            .is_err());
    }
    #[tokio::test]
    async fn sharing_receiver_registry_waiter_timeout_never_releases_unknown_obligation() {
        let registry = ReceiverStartRegistry::default();
        let mut first = None;
        for n in 0..8 {
            let id = format!("attempt-{n}");
            let intent = intent(&id);
            let (entry, _) = registry
                .register(intent.clone(), id, "player", &wrapper(&intent))
                .expect("bounded owner");
            if n == 0 {
                first = Some(entry);
            }
        }
        let entry = first.expect("first owner");
        let actor = ReceiverStartActor(entry.clone());
        assert!(matches!(
            actor.wait_ready(Instant::now()).await,
            Err(ReceiverStartError::Deadline)
        ));
        drop(actor);
        entry.state.lock().expect("state").start = Some(Err(ReceiverStartError::Unresolved));
        let ninth = intent("ninth");
        assert!(matches!(
            registry.register(ninth.clone(), "ninth".into(), "player", &wrapper(&ninth)),
            Err(ReceiverStartError::Capacity)
        ));
        assert_eq!(registry.entries.lock().expect("entries").len(), 8);
    }

    fn dispatch() -> DispatchedSource {
        DispatchedSource {
            credential: plurx_core::secrets::Secret::from_cleartext("owned-metadata-fixture"),
            viewer_hash: "a".repeat(64),
            endpoint: plurx_core::sharing::Endpoint {
                ipv4: std::net::Ipv4Addr::new(100, 64, 0, 2),
                ipv6: None,
                ts_fqdn: "source.example.ts.net".into(),
                port: 9443,
                spki_sha256: "b".repeat(64),
            },
        }
    }

    #[tokio::test]
    async fn sharing_receiver_cleanup_closes_dispatch_and_joins_exact_owned_start() {
        let registry = ReceiverStartRegistry::default();
        let intent = intent("attempt");
        let (entry, _) = registry
            .register(
                intent.clone(),
                "attempt".into(),
                "player",
                &wrapper(&intent),
            )
            .expect("registered receiver");
        let actor = ReceiverStartActor(entry.clone());
        let (release, waiting) = tokio::sync::oneshot::channel();
        let owned = entry.clone();
        *entry.start_task.lock().expect("task") = Some(tokio::spawn(async move {
            waiting.await.expect("actual owner release");
            assert!(owned.state.lock().expect("owner").dispatch_closed);
            assert!(owned.retain_dispatch(dispatch()).is_err());
        }));
        actor.close_dispatch();
        assert!(entry.retain_dispatch(dispatch()).is_err());
        let cleanup = tokio::spawn(async move { actor.join_start_for_cleanup().await });
        tokio::task::yield_now().await;
        assert!(
            !cleanup.is_finished(),
            "cleanup cannot bypass the actual Start join"
        );
        release.send(()).expect("release actual Start task");
        let joined = cleanup
            .await
            .expect("cleanup task")
            .expect("joined actual Start");
        assert!(Arc::ptr_eq(&entry, &joined.0));
        assert!(entry.state.lock().expect("owner").dispatched.is_none());
        assert_eq!(registry.entries.lock().expect("registry").len(), 1);
    }

    #[test]
    fn sharing_receiver_cleanup_preserves_sent_obligation_and_refuses_missing_join() {
        let registry = ReceiverStartRegistry::default();
        let intent = intent("attempt");
        let (entry, _) = registry
            .register(
                intent.clone(),
                "attempt".into(),
                "player",
                &wrapper(&intent),
            )
            .expect("registered receiver");
        entry
            .retain_dispatch(dispatch())
            .expect("first dispatch metadata");
        let retained = entry
            .state
            .lock()
            .expect("owner")
            .dispatched
            .clone()
            .expect("sent obligation");
        ReceiverStartActor(entry.clone()).close_dispatch();
        assert!(entry.retain_dispatch(dispatch()).is_err());
        assert!(Arc::ptr_eq(
            &retained,
            entry
                .state
                .lock()
                .expect("owner")
                .dispatched
                .as_ref()
                .expect("retained")
        ));
        assert!(entry.start_task.lock().expect("task").is_none());
        // Absence of a task is unresolved, never a constructed join receipt.
    }
}
