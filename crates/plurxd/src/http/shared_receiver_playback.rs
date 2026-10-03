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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReceiverStartError {
    Unavailable,
    Capacity,
    Conflict,
    Unresolved,
    Deadline,
}
#[derive(Default)]
pub(crate) struct ReceiverStartRegistry {
    entries: Mutex<Vec<Arc<ReceiverStartInner>>>,
}
struct ReceiverStartInner {
    intent: ReceiverSessionIntent,
    request_id: String,
    fingerprint: String,
    state: Mutex<ReceiverStartState>,
    changed: tokio::sync::Notify,
}
#[derive(Default)]
struct ReceiverStartState {
    start: Option<Result<StartResponse, ReceiverStartError>>,
    // Never discarded on publication failure or loss of the original login.
    source: Option<ReceiverSourceAttachment>,
    received: Option<Arc<ReceivedSource>>,
}
struct ReceivedSource {
    credential: plurx_core::secrets::Secret,
    viewer_hash: String,
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
        tokio::spawn(async move {
            let result = run_owner(state, owner.clone(), playback_id, source_wrapper).await;
            if let Err(error) = result {
                owner.state.lock().expect("receiver owner").start = Some(Err(error));
                owner.changed.notify_waiters();
            }
        });
        Ok(ReceiverStartActor(entry))
    }
    fn register(
        &self,
        intent: ReceiverSessionIntent,
        request_id: String,
        playback_id: &str,
        source_wrapper: &str,
    ) -> Result<(Arc<ReceiverStartInner>, bool), ReceiverStartError> {
        crate::sharing::receiver_source_request(&intent, source_wrapper)
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
            request_id,
            fingerprint,
            state: Mutex::new(ReceiverStartState::default()),
            changed: tokio::sync::Notify::new(),
        });
        entries.push(entry.clone());
        Ok((entry, true))
    }
}
impl ReceiverStartActor {
    pub(crate) async fn wait_ready(
        &self,
        deadline: Instant,
    ) -> Result<StartResponse, ReceiverStartError> {
        loop {
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
            if incarnation_id == incarnation.to_string() => {}
        // A durable reply or missing process-local actor is never producer proof.
        MediaSessionRequestClaim::InFlight { .. } | MediaSessionRequestClaim::Resolved(_) => {
            return Err(ReceiverStartError::Unresolved)
        }
        _ => return Err(ReceiverStartError::Conflict),
    }
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
    let route = state
        .store
        .activate_receiver_media_session(&authority, &activation)
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        .ok_or(ReceiverStartError::Conflict)?
        .route;
    let mut pending = ReceiverPendingRenewal {
        incarnation_id: route.incarnation_id.clone(),
        owner_node_id: state.node_id.clone(),
        owner_epoch: route.owner_epoch,
        request_id: entry.request_id.clone(),
        now_ms: clock_ms(),
        lease_expires_at_ms: route.lease_expires_at_ms,
    };
    let dispatch_owner = pending.clone();
    let start = state
        .sharing
        .start_file_source(&state, intent, &dispatch_owner, &source_wrapper);
    tokio::pin!(start);
    let mut timer = tokio::time::interval(Duration::from_secs(10));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let result = loop {
        tokio::select! {
            result = &mut start => break result.map_err(|_| ReceiverStartError::Unresolved)?,
            _ = timer.tick() => {
                let authority = state.store.prepare_receiver_session_authority(intent.clone()).await
                    .map_err(|_| ReceiverStartError::Unresolved)?.ok_or(ReceiverStartError::Unresolved)?;
                pending.now_ms = clock_ms();
                pending.lease_expires_at_ms = pending.now_ms + 30_000;
                if !state.store.renew_pending_receiver_session(&authority, &pending).await
                    .map_err(|_| ReceiverStartError::Unresolved)? { return Err(ReceiverStartError::Unresolved); }
            }
        }
    };
    let (_, source_incarnation, response) = result.source.into_parts();
    let received = Arc::new(ReceivedSource {
        credential: result.credential,
        viewer_hash: result.viewer_hash,
        incarnation: source_incarnation,
        response,
    });
    entry.state.lock().expect("receiver owner").received = Some(received.clone());
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
    entry.state.lock().expect("receiver owner").start = Some(Ok(projected));
    entry.changed.notify_waiters();
    loop {
        timer.tick().await;
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
        entry.state.lock().expect("receiver owner").source = Some(attachment.clone());
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
}
