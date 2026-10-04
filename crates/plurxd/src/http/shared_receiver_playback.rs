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
#[path = "shared_receiver_control.rs"]
mod control;
#[path = "shared_receiver_retirement.rs"]
mod retirement;
pub(crate) use retirement::receiver_recovery_loop;

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
    end_confirmation: Option<Arc<retirement::ReceiverEndConfirmation>>,
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
    end_confirmation: Option<Arc<retirement::ReceiverEndConfirmation>>,
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
/// Upstream capability sealed into the attached binding (Upstream purpose,
/// B-server/import AAD). It exists in cleartext only inside the capsule.
#[derive(serde::Serialize)]
struct UpstreamCapsuleOut<'a> {
    version: u8,
    reference: &'a plurx_core::sharing_catalogue::SharedReference,
    file_id: &'a plurx_core::sharing::SourceId,
    file_revision: &'a plurx_core::sharing_catalogue_details::FileRevision,
    source_request_id: Uuid,
    source_session_id: Uuid,
    source_incarnation_id: Uuid,
    source_owner_epoch: u64,
    viewer_hash: &'a str,
    endpoint: &'a plurx_core::sharing::Endpoint,
    credential: &'a str,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UpstreamCapsule {
    version: u8,
    reference: plurx_core::sharing_catalogue::SharedReference,
    file_id: plurx_core::sharing::SourceId,
    file_revision: plurx_core::sharing_catalogue_details::FileRevision,
    #[serde(deserialize_with = "plurx_core::sharing::canonical_uuid")]
    source_request_id: Uuid,
    #[serde(deserialize_with = "plurx_core::sharing::canonical_uuid")]
    source_session_id: Uuid,
    #[serde(deserialize_with = "plurx_core::sharing::canonical_uuid")]
    source_incarnation_id: Uuid,
    source_owner_epoch: u64,
    viewer_hash: String,
    endpoint: plurx_core::sharing::Endpoint,
    #[serde(deserialize_with = "plurx_core::sharing::wire_secret")]
    credential: plurx_core::secrets::Secret,
}
/// Dispatch facts sealed before the first Start byte (same purpose and AAD
/// as the upstream capsule; the closed `kind` keeps the two apart).
#[derive(serde::Serialize)]
struct DispatchCapsuleOut<'a> {
    version: u8,
    kind: &'static str,
    reference: &'a plurx_core::sharing_catalogue::SharedReference,
    file_id: &'a plurx_core::sharing::SourceId,
    file_revision: &'a plurx_core::sharing_catalogue_details::FileRevision,
    source_request_id: Uuid,
    viewer_hash: &'a str,
    endpoint: &'a plurx_core::sharing::Endpoint,
    credential: &'a str,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchCapsule {
    version: u8,
    kind: String,
    reference: plurx_core::sharing_catalogue::SharedReference,
    file_id: plurx_core::sharing::SourceId,
    file_revision: plurx_core::sharing_catalogue_details::FileRevision,
    #[serde(deserialize_with = "plurx_core::sharing::canonical_uuid")]
    source_request_id: Uuid,
    viewer_hash: String,
    endpoint: plurx_core::sharing::Endpoint,
    #[serde(deserialize_with = "plurx_core::sharing::wire_secret")]
    credential: plurx_core::secrets::Secret,
}
const DISPATCH_CAPSULE_KIND: &str = "receiver_dispatch";
/// Seal the facts a recovered owner needs to send the End this dispatch owes.
pub(crate) fn seal_dispatch_capsule(
    key: &plurx_core::secrets::CredentialKey,
    server: Uuid,
    intent: &ReceiverSessionIntent,
    credential: &plurx_core::secrets::Secret,
    viewer_hash: &str,
    endpoint: &plurx_core::sharing::Endpoint,
) -> Result<plurx_core::secrets::SealedSecret, ReceiverStartError> {
    let recipe = &intent.recipe;
    let capsule = plurx_core::secrets::Secret::from_cleartext(
        serde_json::to_string(&DispatchCapsuleOut {
            version: 1,
            kind: DISPATCH_CAPSULE_KIND,
            reference: &recipe.reference,
            file_id: &recipe.file_id,
            file_revision: &recipe.file_revision,
            source_request_id: recipe.source_request_id,
            viewer_hash,
            endpoint,
            credential: credential.expose(),
        })
        .map_err(|_| ReceiverStartError::Unresolved)?,
    );
    key.seal_sharing(
        SharingSecretPurpose::Upstream,
        server,
        intent.scope.import_id,
        capsule.expose(),
    )
    .map_err(|_| ReceiverStartError::Unresolved)
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
    fn confirmed_end(&self, session: Uuid) -> bool {
        self.settled
            .lock()
            .expect("settled receiver attempts")
            .iter()
            .any(|attempt| {
                attempt
                    .end_confirmation
                    .as_ref()
                    .is_some_and(|proof| proof.session_id() == session)
            })
    }
    /// A live (not yet retired) local actor owns this Source request. Crash
    /// recovery never claims such a route: that actor is its owner.
    fn holds(&self, incarnation: Uuid) -> bool {
        self.entries
            .lock()
            .expect("receiver registry")
            .iter()
            .any(|entry| {
                entry.intent.recipe.source_request_id == incarnation
                    && !entry.state.lock().expect("receiver owner").retired
            })
    }
    pub(crate) fn by_session(&self, session: Uuid) -> Option<ReceiverStartActor> {
        self.entries
            .lock()
            .expect("receiver registry")
            .iter()
            .find_map(|entry| {
                let owned = entry.state.lock().expect("receiver owner");
                owned
                    .owner
                    .as_ref()
                    .filter(|owner| owner.session_id == session)
                    .map(|_| ReceiverStartActor(entry.clone()))
            })
    }
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
        // Only the detached retirement owner marks an entry retired: after
        // actual joins and Applied/Replay, or when it ends without them. Keep
        // bounded non-authorizing tombstones so exact retries cannot recreate
        // an already ended actor; only a confirmed one carries an End receipt.
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
                end_confirmation: entry
                    .state
                    .lock()
                    .expect("receiver owner")
                    .end_confirmation
                    .clone(),
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
        // Every retirement owner is bounded and retires its entry on every
        // exit, so this counts live attempts, never stuck settlements.
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
/// Whether a receiver session's recipe names exactly this signed file: import,
/// lifecycle, Source item, file and revision. A session for another file, or
/// for an earlier revision or lifecycle of this one, binds nothing here.
pub(crate) fn recipe_binds_file(
    intent: &ReceiverSessionIntent,
    reference: &plurx_core::sharing_file_locators::FileLocatorReference,
) -> bool {
    let recipe = &intent.recipe;
    intent.scope.import_id == reference.item.import_id
        && recipe.reference == reference.item
        && recipe.lifecycle_generation == reference.lifecycle_generation
        && recipe.file_id == reference.file_id
        && recipe.file_revision == reference.revision
}
impl ReceiverStartActor {
    /// A file asset requested under `session=` is bound when this live
    /// session plays exactly that file and its delivery attachment (original
    /// login and delivery grant) is still current. Answers the session's
    /// viewer and original login; it creates and changes nothing.
    pub(crate) async fn bound_file_viewer(
        &self,
        state: &AppState,
        reference: &plurx_core::sharing_file_locators::FileLocatorReference,
    ) -> Result<(i64, String), ReceiverStartError> {
        if !recipe_binds_file(&self.0.intent, reference) {
            return Err(ReceiverStartError::Conflict);
        }
        self.current_delivery_attachment(state).await?;
        Ok((self.0.intent.user_id, self.0.intent.login_hash.clone()))
    }
    async fn current_delivery_attachment(
        &self,
        state: &AppState,
    ) -> Result<
        (
            plurx_core::sharing_receiver_sessions::ReceiverSessionWriteAuthority,
            ReceiverSourceAttachment,
            Arc<ReceivedSource>,
        ),
        ReceiverStartError,
    > {
        if self.0.stop.is_cancelled() {
            return Err(ReceiverStartError::Unresolved);
        }
        let (mut attachment, received) = {
            let owned = self.0.state.lock().expect("receiver owner");
            if !matches!(owned.start, Some(Ok(_))) || owned.retirement_started {
                return Err(ReceiverStartError::Unresolved);
            }
            let mut attachment = owned.source.clone().ok_or(ReceiverStartError::Unresolved)?;
            attachment.owner = owned.owner.clone().ok_or(ReceiverStartError::Unresolved)?;
            (
                attachment,
                owned
                    .received
                    .clone()
                    .ok_or(ReceiverStartError::Unresolved)?,
            )
        };
        attachment.owner.now_ms = clock_ms();
        let authority = state
            .store
            .prepare_receiver_session_authority(self.0.intent.clone())
            .await
            .map_err(|_| ReceiverStartError::Unresolved)?
            .ok_or(ReceiverStartError::Unresolved)?;
        let snapshot = state
            .store
            .receiver_source_binding(&authority, &attachment.owner)
            .await
            .map_err(|_| ReceiverStartError::Unresolved)?
            .ok_or(ReceiverStartError::Unresolved)?;
        let a = &attachment.binding;
        let b = &snapshot.binding;
        let original = a
            .capability_envelope
            .to_persist()
            .map_err(|_| ReceiverStartError::Unresolved)?;
        let current = b
            .capability_envelope
            .to_persist()
            .map_err(|_| ReceiverStartError::Unresolved)?;
        if snapshot.response_json.is_none()
            || a.reference != b.reference
            || a.file_id != b.file_id
            || a.file_revision != b.file_revision
            || a.source_request_id != b.source_request_id
            || a.source_session_id != b.source_session_id
            || a.source_incarnation_id != b.source_incarnation_id
            || original != current
        {
            return Err(ReceiverStartError::Unresolved);
        }
        let hash = plurx_core::auth::hash_token(&attachment.owner.session_id.to_string());
        state
            .store
            .receiver_delivery(&authority, &attachment, &hash)
            .await
            .map_err(|_| ReceiverStartError::Unresolved)?
            .ok_or(ReceiverStartError::Unresolved)?;
        if self.0.stop.is_cancelled() {
            return Err(ReceiverStartError::Unresolved);
        }
        Ok((authority, attachment, received))
    }
    pub(crate) async fn record_progress(
        &self,
        state: &AppState,
        user_id: i64,
        login_hash: &str,
        import: Uuid,
        item: &plurx_core::sharing::SourceId,
        beat: &ReceiverProgressBeat,
    ) -> Result<
        (
            plurx_core::sharing_receiver_progress::ReceiverProgressOutcome,
            Option<i64>,
        ),
        ReceiverStartError,
    > {
        if self.0.intent.user_id != user_id
            || self.0.intent.login_hash != login_hash
            || self.0.intent.recipe.reference.import_id != import
            || &self.0.intent.recipe.reference.item_id != item
        {
            return Err(ReceiverStartError::Unavailable);
        }
        let (authority, attachment, _) = self.current_delivery_attachment(state).await?;
        if attachment.owner.session_id.to_string() != beat.session_id {
            return Err(ReceiverStartError::Unavailable);
        }
        self.current_source_status(state).await?;
        let progress = plurx_core::sharing_receiver_progress::ReceiverProgress {
            attachment,
            sequence: beat.sequence,
            position_ms: beat.position_ms,
            duration_ms: beat.duration_ms,
            watched: beat.watched,
        };
        let outcome = state
            .store
            .save_receiver_progress(&authority, &progress)
            .await
            .map_err(|_| ReceiverStartError::Unresolved)?;
        self.current_delivery_attachment(state).await?;
        use plurx_core::sharing_receiver_progress::ReceiverProgressOutcome;
        let current = if matches!(
            outcome,
            ReceiverProgressOutcome::Stale | ReceiverProgressOutcome::Conflict
        ) {
            let reference = &self.0.intent.recipe.reference;
            let watch = state
                .store
                .remote_watch(import, reference.library_id.clone(), item.clone(), user_id)
                .await
                .map_err(|_| ReceiverStartError::Unresolved)?;
            self.current_delivery_attachment(state).await?;
            watch.map(|watch| watch.sequence)
        } else {
            None
        };
        Ok((outcome, current))
    }
    async fn wait_confirmed_end(&self, session: Uuid) -> Result<(), ReceiverStartError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(35);
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let owned = self.0.state.lock().expect("receiver owner");
                if owned
                    .end_confirmation
                    .as_ref()
                    .is_some_and(|proof| proof.session_id() == session)
                {
                    return Ok(());
                }
                // A retirement owner that stopped without this receipt has
                // ended; no later change can produce it.
                if owned.retired {
                    return Err(ReceiverStartError::Unresolved);
                }
            }
            tokio::time::timeout_at(deadline, changed)
                .await
                .map_err(|_| ReceiverStartError::Unresolved)?;
        }
    }
    pub(crate) async fn protect_start_response(
        &self,
        state: Arc<AppState>,
        connection: &crate::SharingConnectionCancellation,
        response: axum::response::Response,
    ) -> Result<axum::response::Response, ReceiverStartError> {
        use futures_util::StreamExt;
        self.current_delivery_attachment(&state).await?;
        self.current_source_status(&state).await?;
        self.current_delivery_attachment(&state).await?;
        let guard = self.retain_accepted_connection(state, connection)?;
        let (parts, body) = response.into_parts();
        let stream = body.into_data_stream().map(move |frame| {
            let _accepted_writer = &guard;
            frame
        });
        Ok(axum::http::Response::from_parts(
            parts,
            axum::body::Body::from_stream(stream),
        ))
    }
    pub(crate) fn retain_accepted_connection(
        &self,
        state: Arc<AppState>,
        connection: &crate::SharingConnectionCancellation,
    ) -> Result<Arc<dyn Send + Sync>, ReceiverStartError> {
        let (guard, new_monitor) = self.0.bodies.reserve_connection(connection)?;
        if !new_monitor {
            return Ok(guard);
        }
        // Capture tokens and the closure observer only: capturing the monitor
        // owner itself would form an Arc -> JoinHandle -> Arc cycle.
        let cancel = connection.0.clone();
        let closed = connection.closed();
        let actor = self.clone();
        let retained = guard.clone();
        if connection.monitor(async move {
            let _retained = retained;
            let mut timer = tokio::time::interval(Duration::from_secs(1));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = closed.wait() => return,
                    () = actor.0.stop.cancelled() => break,
                    _ = timer.tick() => {}
                }
                // Original B login and exact delivery binding remain required
                // while a writer has queued bytes, including after Body EOF.
                if actor.current_delivery_attachment(&state).await.is_err() {
                    actor.begin_retirement(state.clone(), plurx_core::sharing_receiver_retirement::ReceiverRetirementReason::Revoked);
                    break;
                }
            }
            cancel.cancel();
            closed.wait().await;
        }).is_err() {
            connection.0.cancel();
            return Err(ReceiverStartError::Capacity);
        }
        Ok(guard)
    }
    pub(crate) async fn open_source_resource(
        &self,
        state: &AppState,
        resource: &plurx_core::sharing_resources::SharingHlsResource,
    ) -> Result<crate::sharing_client::SourcePeerResource, ReceiverStartError> {
        let lifetime: Arc<dyn Send + Sync> = self.0.bodies.reserve()?;
        let actor = self.clone();
        let state = state.clone();
        let resource = resource.clone();
        // A cancelled HTTP waiter cannot cancel a sent Source request or
        // release custody of its nested dial/body jobs. The bounded counted
        // task owns the entire open until completion and its actual driver
        // retains the same guard through subsequent socket closure.
        tokio::spawn(async move {
            let custody = lifetime.clone();
            let result = actor
                .open_source_resource_owned(&state, &resource, lifetime)
                .await;
            drop(custody);
            result
        })
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
    }
    async fn open_source_resource_owned(
        &self,
        state: &AppState,
        resource: &plurx_core::sharing_resources::SharingHlsResource,
        lifetime: Arc<dyn Send + Sync>,
    ) -> Result<crate::sharing_client::SourcePeerResource, ReceiverStartError> {
        let (_, _, received) = self.current_delivery_attachment(state).await?;
        self.current_source_status_owned(state, lifetime.clone())
            .await?;
        let expected = plurx_core::sharing::SharingIdentity {
            server_id: self.0.intent.scope.source_server_id,
            catalogue_epoch: self.0.intent.scope.catalogue_epoch,
            created_at_ms: 0,
        };
        let (peer, _) = crate::sharing_client::PeerConnection::verified_with_lifetime(
            &state.sharing,
            std::slice::from_ref(&received.endpoint),
            &expected,
            lifetime,
        )
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?;
        let known = crate::sharing_client::SourcePeerLineage::from_start(
            received.incarnation,
            &received.response,
        )
        .map_err(|_| ReceiverStartError::Unresolved)?;
        let opened = peer
            .file_resource(
                &received.credential,
                &received.viewer_hash,
                &self.0.peer_session,
                &known,
                resource,
            )
            .await
            .map_err(|_| ReceiverStartError::Unresolved)?;
        // Source IO can park. Reobserve the original B login, exact route,
        // current binding and delivery grant before returning any body bytes.
        self.current_delivery_attachment(state).await?;
        Ok(opened)
    }
    async fn current_source_status(
        &self,
        state: &AppState,
    ) -> Result<crate::sharing_client::SourceStatusReceipt, ReceiverStartError> {
        let lifetime: Arc<dyn Send + Sync> = self.0.bodies.reserve()?;
        self.current_source_status_owned(state, lifetime).await
    }
    async fn current_source_status_owned(
        &self,
        state: &AppState,
        lifetime: Arc<dyn Send + Sync>,
    ) -> Result<crate::sharing_client::SourceStatusReceipt, ReceiverStartError> {
        if self.0.stop.is_cancelled() {
            return Err(ReceiverStartError::Unresolved);
        }
        // Current B policy precedes the network operation; every following
        // committing writer still obtains and repeats its own fresh guard.
        state
            .store
            .prepare_receiver_session_authority(self.0.intent.clone())
            .await
            .map_err(|_| ReceiverStartError::Unresolved)?
            .ok_or(ReceiverStartError::Unresolved)?;
        let received = self
            .0
            .state
            .lock()
            .expect("receiver owner")
            .received
            .clone()
            .ok_or(ReceiverStartError::Unresolved)?;
        let expected = plurx_core::sharing::SharingIdentity {
            server_id: self.0.intent.scope.source_server_id,
            catalogue_epoch: self.0.intent.scope.catalogue_epoch,
            created_at_ms: 0,
        };
        let (mut peer, _) = crate::sharing_client::PeerConnection::verified_with_lifetime(
            &state.sharing,
            std::slice::from_ref(&received.endpoint),
            &expected,
            lifetime,
        )
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?;
        let known = crate::sharing_client::SourcePeerLineage::from_start(
            received.incarnation,
            &received.response,
        )
        .map_err(|_| ReceiverStartError::Unresolved)?;
        peer.file_status(
            &received.credential,
            &received.viewer_hash,
            &self.0.peer_session,
            &known,
        )
        .await
        .map_err(|_| ReceiverStartError::Unresolved)
    }
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
    // Only an authenticated, definitive Source refusal is terminal.
    async fn request_source_end(
        &self,
        state: &AppState,
        joined: &JoinedReceiverStart,
    ) -> Result<Arc<crate::sharing_client::SourceEndReceipt>, retirement::RetirementStep> {
        use retirement::RetirementStep;
        if !Arc::ptr_eq(&self.0, &joined.0) {
            return Err(RetirementStep::Refused);
        }
        let (dispatched, received) = {
            let owner = self.0.state.lock().expect("receiver owner");
            if let Some(receipt) = &owner.confirmed_source_end {
                return Ok(receipt.clone());
            }
            (
                owner.dispatched.clone().ok_or(RetirementStep::Refused)?,
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
            .map_err(|_| RetirementStep::Refused)?;
        let mut connection = crate::sharing_client::CleanupPeerConnection::connect(
            &state.sharing,
            &dispatched.endpoint,
        )
        .await
        .map_err(RetirementStep::from_source_end)?;
        let receipt = Arc::new(
            connection
                .end(
                    &dispatched.credential,
                    &dispatched.viewer_hash,
                    &self.0.peer_session,
                    known.as_ref(),
                )
                .await
                .map_err(RetirementStep::from_source_end)?,
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
    let connection_lifetime: Arc<dyn Send + Sync> = entry.bodies.reserve()?;
    let start = state.sharing.start_file_source(
        &state,
        intent,
        &dispatch_owner,
        &source_wrapper,
        connection_lifetime.clone(),
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
        serde_json::to_string(&UpstreamCapsuleOut {
            version: 1,
            reference: &intent.recipe.reference,
            file_id: &intent.recipe.file_id,
            file_revision: &intent.recipe.file_revision,
            source_request_id: incarnation,
            source_session_id: source_session,
            source_incarnation_id: source_incarnation,
            source_owner_epoch: source_epoch,
            viewer_hash: &received.viewer_hash,
            endpoint: &received.endpoint,
            credential: received.credential.expose(),
        })
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
    let source_status = ReceiverStartActor(entry.clone())
        .current_source_status_owned(&state, connection_lifetime.clone())
        .await?;
    let projected = project_shared_start(
        source_status.response().clone(),
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
    // The actual B session UUID remains its bearer capability; persist only
    // its hash and a finite deadline under fresh original-login authority.
    let authority = state
        .store
        .prepare_receiver_session_authority(intent.clone())
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        .ok_or(ReceiverStartError::Unresolved)?;
    attachment.owner.now_ms = clock_ms();
    let deadline_ms = authority
        .receiver_delivery_deadline(attachment.owner.lease_expires_at_ms)
        .ok_or(ReceiverStartError::Unresolved)?;
    let delivery = plurx_core::sharing_receiver_delivery::ReceiverDeliveryGrant {
        token_hash: plurx_core::auth::hash_token(&attachment.owner.session_id.to_string()),
        deadline_ms,
    };
    if state
        .store
        .issue_receiver_delivery(&authority, &attachment, &delivery)
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        == plurx_core::sharing_receiver_delivery::ReceiverDeliveryWrite::Refused
    {
        return Err(ReceiverStartError::Unresolved);
    }
    {
        let mut owned = entry.state.lock().expect("receiver owner");
        if owned.dispatch_closed {
            return Err(ReceiverStartError::Unresolved);
        }
        // Source dispatch may have extended the pending lease. Retain the
        // exact lease used by attachment/publication before readers can run.
        owned.owner = Some(attachment.owner.clone());
        owned.source = Some(attachment.clone());
        owned.start = Some(Ok(projected));
    }
    entry.changed.notify_waiters();
    // One Source round trip per lease period. The 30 s lease exchange is what
    // answers "is the Source alive"; local revocation is enforced by each
    // accepted connection's monitor and every viewer-visible byte by
    // `open_source_resource_owned`. A faster Source-end signal has to come
    // from the Source, never from a faster poll here. Daemon drain ends this
    // owner so retirement can still send the Source its End.
    let mut renewal = tokio::time::interval(Duration::from_secs(10));
    renewal.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = entry.stop.cancelled() => return Err(ReceiverStartError::Unresolved),
            () = state.shutdown.cancelled() => return Err(ReceiverStartError::Unresolved),
            _ = renewal.tick() => {}
        }
        let actor = ReceiverStartActor(entry.clone());
        actor
            .current_source_status_owned(&state, connection_lifetime.clone())
            .await?;
        actor.current_delivery_attachment(&state).await?;
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReceiverProgressBeat {
    pub(crate) session_id: String,
    pub(crate) sequence: i64,
    pub(crate) position_ms: i64,
    pub(crate) duration_ms: Option<i64>,
    pub(crate) watched: bool,
}
impl ReceiverProgressBeat {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, ReceiverStartError> {
        if bytes.is_empty() || bytes.len() > 1024 {
            return Err(ReceiverStartError::Unavailable);
        }
        let beat: Self =
            serde_json::from_slice(bytes).map_err(|_| ReceiverStartError::Unavailable)?;
        let session =
            Uuid::parse_str(&beat.session_id).map_err(|_| ReceiverStartError::Unavailable)?;
        const MAX_SAFE: i64 = 9_007_199_254_740_991;
        if session.is_nil()
            || session.to_string() != beat.session_id
            || !(0..=MAX_SAFE).contains(&beat.sequence)
            || !(0..=MAX_SAFE).contains(&beat.position_ms)
            || beat
                .duration_ms
                .is_some_and(|n| !(0..=MAX_SAFE).contains(&n))
        {
            return Err(ReceiverStartError::Unavailable);
        }
        Ok(beat)
    }
}
/// Typed receiver dispatch precedes the Local HLS handlers. A durable remote
/// recipe cannot recreate the physical actor or authorize a Local producer.
pub(crate) async fn receiver_media(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::{
        http::{Method, StatusCode},
        response::IntoResponse,
    };
    use futures_util::StreamExt;
    // This layer sits on the router nested at /api/v1, which strips the
    // prefix from the request URI; the public path is the original one.
    let original = request
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map_or_else(
            || request.uri().path().to_owned(),
            |uri| uri.path().to_owned(),
        );
    let Some(path) = original.strip_prefix("/api/v1/hls/") else {
        return next.run(request).await;
    };
    let (session, suffix) = path.split_once('/').unwrap_or((path, ""));
    let Ok(session_id) = Uuid::parse_str(session) else {
        return next.run(request).await;
    };
    if session_id.to_string() != session {
        return next.run(request).await;
    }
    let actor = state.sharing.receiver_starts.by_session(session_id);
    if actor.is_none() {
        if suffix.is_empty()
            && request.method() == Method::DELETE
            && request.uri().query().is_none()
            && state.sharing.receiver_starts.confirmed_end(session_id)
        {
            return StatusCode::NO_CONTENT.into_response();
        }
        match state.store.media_session_route(session).await {
            Ok(Some(route)) => {
                // Only the explicit remote discriminant is inspected here;
                // neither a row nor its recipe creates a playback authority.
                let remote = serde_json::from_str::<serde_json::Value>(&route.recipe_json)
                    .ok()
                    .is_some_and(|value| {
                        value.get("kind").and_then(|kind| kind.as_str()) == Some("remote_source")
                    });
                if remote {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "shared playback owner unavailable",
                    )
                        .into_response();
                }
            }
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            Ok(None) => {}
        }
        return next.run(request).await;
    }
    let actor = actor.expect("actual receiver actor");
    if suffix.is_empty() && request.method() == Method::DELETE && request.uri().query().is_none() {
        actor.begin_retirement(
            Arc::new(state),
            plurx_core::sharing_receiver_retirement::ReceiverRetirementReason::Deleted,
        );
        return match actor.wait_confirmed_end(session_id).await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    }
    if request.uri().query().is_none() && matches!(suffix, "status" | "control") {
        let Some(connection) = request
            .extensions()
            .get::<crate::SharingConnectionCancellation>()
            .cloned()
        else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        if actor.current_delivery_attachment(&state).await.is_err() {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        let state = Arc::new(state);
        let method = request.method().clone();
        return match (suffix, &method) {
            ("status", &Method::GET) => control::receiver_status(actor, state, &connection).await,
            ("control", &Method::POST) => {
                control::receiver_control(actor, state, &connection, request.into_body()).await
            }
            _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
        };
    }
    if request.method() != Method::GET {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "shared playback operation unavailable",
        )
            .into_response();
    }
    let relative = match request.uri().query() {
        Some(query) => format!("{suffix}?{query}"),
        None => suffix.to_owned(),
    };
    let Ok(resource) = plurx_core::sharing_resources::SharingHlsResource::parse(&relative) else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    let Some(connection) = request
        .extensions()
        .get::<crate::SharingConnectionCancellation>()
        .cloned()
    else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    // Authorize before registering a writer, then again after Source IO.
    if actor.current_delivery_attachment(&state).await.is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let state = Arc::new(state);
    let guard = match actor.retain_accepted_connection(state.clone(), &connection) {
        Ok(guard) => guard,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let opened = match actor.open_source_resource(&state, &resource).await {
        Ok(opened) => opened,
        Err(ReceiverStartError::Capacity) => return StatusCode::TOO_MANY_REQUESTS.into_response(),
        Err(_) => {
            actor.begin_retirement(
                state,
                plurx_core::sharing_receiver_retirement::ReceiverRetirementReason::Revoked,
            );
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let stream = opened.body.into_data_stream().map(move |frame| {
        let _writer_ownership = &guard;
        frame
    });
    let mut builder = axum::http::Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, opened.mime)
        .header(axum::http::header::CONTENT_LENGTH, opened.length)
        .header(axum::http::header::CACHE_CONTROL, "no-store");
    if let Some(etag) = opened.etag {
        builder = builder.header(axum::http::header::ETAG, etag);
    }
    builder
        .body(axum::body::Body::from_stream(stream))
        .expect("validated Source resource headers")
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
    #[tokio::test(start_paused = true)]
    async fn sharing_receiver_end_never_acknowledges_retired_metadata_without_physical_receipt() {
        let registry = ReceiverStartRegistry::default();
        let request = intent("attempt");
        let (entry, _) = registry
            .register(
                request.clone(),
                "attempt".into(),
                "player",
                &wrapper(&request),
            )
            .expect("owned attempt");
        entry.state.lock().expect("owner").retired = true;
        let actor = ReceiverStartActor(entry);
        let unknown = Uuid::new_v4();
        assert!(!registry.confirmed_end(unknown));
        assert!(matches!(
            actor.wait_confirmed_end(unknown).await,
            Err(ReceiverStartError::Unresolved)
        ));
        // Pruning into a non-authorizing retry tombstone still creates no
        // physical completion receipt and cannot produce an End204.
        assert!(matches!(
            registry.register(
                request.clone(),
                "attempt".into(),
                "player",
                &wrapper(&request)
            ),
            Err(ReceiverStartError::Unresolved)
        ));
        assert!(!registry.confirmed_end(unknown));
    }
    #[test]
    fn sharing_receiver_progress_wire_rejects_foreign_fields_duplicates_and_unsafe_numbers() {
        let session = Uuid::new_v4();
        let value = serde_json::json!({"session_id":session,"sequence":9,"position_ms":42,"duration_ms":null,"watched":false});
        assert!(ReceiverProgressBeat::parse(value.to_string().as_bytes()).is_ok());
        for field in ["sequence", "position_ms", "duration_ms"] {
            for number in [-1, 9_007_199_254_740_992_i64] {
                let mut bad = value.clone();
                bad[field] = number.into();
                assert!(ReceiverProgressBeat::parse(bad.to_string().as_bytes()).is_err());
            }
        }
        let mut bad = value.clone();
        bad["user_id"] = 0.into();
        assert!(ReceiverProgressBeat::parse(bad.to_string().as_bytes()).is_err());
        let duplicate = value.to_string().replacen("{", "{\"sequence\":8,", 1);
        assert!(ReceiverProgressBeat::parse(duplicate.as_bytes()).is_err());
        let mut bad = value.clone();
        bad["session_id"] = Uuid::nil().to_string().into();
        assert!(ReceiverProgressBeat::parse(bad.to_string().as_bytes()).is_err());
        let mut bad = value;
        bad["sequence"] = 1.25.into();
        assert!(ReceiverProgressBeat::parse(bad.to_string().as_bytes()).is_err());
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

    async fn retired_within(actor: &ReceiverStartActor, limit: Duration) -> bool {
        tokio::time::timeout(limit, async {
            loop {
                let changed = actor.0.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if actor.0.state.lock().expect("owner").retired {
                    return;
                }
                changed.await;
            }
        })
        .await
        .is_ok()
    }

    #[tokio::test]
    async fn sharing_receiver_unjoinable_retirement_releases_its_registry_slot() {
        let registry = ReceiverStartRegistry::default();
        let state = Arc::new(crate::http::source_actor_test_state());
        let mut owners = Vec::new();
        for n in 0..8 {
            let id = format!("attempt-{n}");
            let intent = intent(&id);
            let (entry, _) = registry
                .register(intent.clone(), id, "player", &wrapper(&intent))
                .expect("bounded owner");
            owners.push((entry, intent));
        }
        let ninth = intent("ninth");
        assert!(matches!(
            registry.register(ninth.clone(), "ninth".into(), "player", &wrapper(&ninth)),
            Err(ReceiverStartError::Capacity)
        ));
        // The Start task was cancelled, so no join receipt and therefore no
        // retirement witness can exist. The owner still ends.
        let (entry, first) = owners.swap_remove(0);
        let cancelled = tokio::spawn(std::future::pending::<()>());
        cancelled.abort();
        *entry.start_task.lock().expect("Start handle") = Some(cancelled);
        let actor = ReceiverStartActor(entry.clone());
        actor.begin_retirement(
            state,
            plurx_core::sharing_receiver_retirement::ReceiverRetirementReason::Revoked,
        );
        assert!(
            retired_within(&actor, Duration::from_secs(5)).await,
            "an unjoinable retirement owner ends instead of holding its slot"
        );
        assert!(entry
            .state
            .lock()
            .expect("owner")
            .end_confirmation
            .is_none());
        assert!(registry
            .register(ninth.clone(), "ninth".into(), "player", &wrapper(&ninth))
            .is_ok());
        // The pruned tombstone carries no End receipt and recreates nothing.
        assert!(matches!(
            registry.register(
                first.clone(),
                "attempt-0".into(),
                "player",
                &wrapper(&first)
            ),
            Err(ReceiverStartError::Unresolved)
        ));
    }

    #[tokio::test]
    async fn sharing_receiver_refused_pending_retirement_is_terminal_and_frees_its_slot() {
        let registry = ReceiverStartRegistry::default();
        let state = Arc::new(crate::http::source_actor_test_state());
        let request = intent("attempt");
        let (entry, _) = registry
            .register(
                request.clone(),
                "attempt".into(),
                "player",
                &wrapper(&request),
            )
            .expect("owned attempt");
        // An assigned claim whose activation was never planned, joined after
        // its Start task returned without sending Source Start.
        entry.state.lock().expect("owner").claim =
            ReceiverClaimStage::Assigned(state.node_id.clone());
        *entry.start_task.lock().expect("Start handle") = Some(tokio::spawn(async {}));
        let actor = ReceiverStartActor(entry.clone());
        actor.begin_retirement(
            state.clone(),
            plurx_core::sharing_receiver_retirement::ReceiverRetirementReason::Revoked,
        );
        // No claimed row exists, so both exact pending witnesses are Refused.
        // The owner ends before its first 5 s retry instead of resending them.
        assert!(
            retired_within(&actor, Duration::from_secs(4)).await,
            "a refused immutable pending witness is terminal"
        );
        {
            let owned = entry.state.lock().expect("owner");
            assert!(owned.end_confirmation.is_none() && owned.dispatched.is_none());
        }
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
}
