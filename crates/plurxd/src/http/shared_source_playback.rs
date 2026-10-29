//! Private Source HLS start coordination. Wire data is never producer authority.
use super::{
    error::ApiError,
    hls::{CreateSession, SourcePlaybackTarget},
};
use axum::http::{HeaderMap, StatusCode};
use serde_json::Value;
use uuid::Uuid;

#[path = "shared_source_forwarding.rs"]
pub(crate) mod forwarding;
use forwarding::{AuthenticationHeaders, SourceHeaders};
#[path = "shared_source_ingress.rs"]
mod ingress;
use ingress::{
    apply_forward_custody, execute_forward_unassigned_cleanup, prepare_forward_start,
    resolve_forward_route, source_forward_locality, validate_retained_forward,
};

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
    forwarding: forwarding::ForwardingRegistry,
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
    end_deadline: std::sync::OnceLock<std::time::Instant>,
    local_custody: std::sync::Mutex<Vec<ingress::LocalCustody>>,
    registration_gate: std::sync::Arc<ingress::RegistrationGate>,
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
    cleanup: std::sync::Mutex<Option<SourceUninvokedCleanup>>,
    cleanup_confirmation: std::sync::Mutex<Option<Uuid>>,
    supervisor: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}
#[derive(Default)]
enum SourceStartTaskStage {
    #[default]
    Registered,
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
        // outcome, never as an adopted factory or physical-admission authority.
        intent: plurx_core::sharing_source_sessions::SourceSessionIntent,
    },
    Acquired(plurx_core::sharing_source_sessions::SourceBindingHandle),
    Assigning(plurx_core::sharing_source_sessions::SourceBindingHandle),
    Assigned(plurx_core::sharing_source_sessions::SourceDispatchAssignment),
    Activating(plurx_core::sharing_source_sessions::SourceDispatchAssignment),
    InvokingFactory(plurx_core::sharing_source_sessions::SourceDispatchAssignment),
    FactoryRefused(crate::transcode::source_actor::SourceWorkerNoAdmission),
}
enum SourceStartTaskJoined {
    Returned,
    PanickedOrCancelled,
}
// Private invocation custody: only the supervisor that joined the actual Start
// task can retain this receipt. It covers this acquired incarnation, never a
// historical request or an assignment reconstructed from durable rows.
#[derive(Clone)]
enum SourceUninvokedCleanup {
    UncertainG0 {
        planned_incarnation: Uuid,
        intent: Box<plurx_core::sharing_source_sessions::SourceSessionIntent>,
    },
    G0(plurx_core::sharing_source_sessions::SourceBindingHandle),
    G1(plurx_core::sharing_source_sessions::SourceDispatchAssignment),
}
impl SourceUninvokedCleanup {
    fn incarnation(&self) -> Uuid {
        match self {
            Self::UncertainG0 {
                planned_incarnation,
                ..
            } => *planned_incarnation,
            Self::G0(binding) => binding.incarnation_id(),
            Self::G1(assignment) => assignment.binding().incarnation_id(),
        }
    }
    async fn release(&self, state: &crate::state::AppState) -> Result<(), SourceStartFailure> {
        use plurx_core::sharing_source_sessions::SourceReleaseOutcome;
        let result = match self {
            Self::UncertainG0 { intent, .. } => {
                state
                    .store
                    .release_source_uncertain_uninvoked_claim(intent)
                    .await
            }
            Self::G0(binding) => state.store.release_source_never_dispatched(binding).await,
            Self::G1(assignment) => {
                state
                    .store
                    .settle_source_assigned_without_activation(assignment)
                    .await
            }
        }
        .map_err(|_| SourceStartFailure::Unresolved)?;
        match result {
            SourceReleaseOutcome::Released | SourceReleaseOutcome::ExactReplay => Ok(()),
            SourceReleaseOutcome::Refused => Err(SourceStartFailure::Unresolved),
        }
    }
}
impl SourceStartTaskStage {
    fn incarnation(&self) -> Option<Uuid> {
        match self {
            Self::Registered => None,
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
            Self::FactoryRefused(receipt) => Some(receipt.assignment().binding().incarnation_id()),
        }
    }
}
impl SourceStartEntry {
    fn retain_stage(&self, stage: SourceStartTaskStage) {
        *self.task.stage.lock().expect("Source owned start stage") = stage;
        self.changed.notify_waiters();
    }
    fn start_owned_task(
        self: &std::sync::Arc<Self>,
        state: std::sync::Arc<crate::state::AppState>,
        headers: SourceHeaders,
        input: SourceStartInput,
        deadline: std::time::Instant,
    ) {
        // The entry retains its supervisor, which owns the actual JoinHandle. Waiter loss
        // cannot drop this task or replace its retained claim/assignment stage.
        // Publish an outcome only after this exact worker future has joined.
        let entry = std::sync::Arc::clone(self);
        let worker_entry = std::sync::Arc::clone(self);
        let grant = self.grant;
        let cleanup_state = std::sync::Arc::clone(&state);
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
        let supervisor = tokio::spawn(async move {
            let (result, joined) = match worker.await {
                Ok(result) => (result, SourceStartTaskJoined::Returned),
                Err(_) => (
                    Err(SourceStartFailure::Unresolved),
                    SourceStartTaskJoined::PanickedOrCancelled,
                ),
            };
            if let Err(error) = &result {
                let stage = match &*entry.task.stage.lock().expect("Source start stage") {
                    SourceStartTaskStage::Registered => "registered",
                    SourceStartTaskStage::ReadingIntent { .. } => "reading_intent",
                    SourceStartTaskStage::IntentReady { .. } => "intent_ready",
                    SourceStartTaskStage::Claiming { .. } => "claiming",
                    SourceStartTaskStage::Acquired(_) => "preparing",
                    SourceStartTaskStage::Assigning(_) => "assigning",
                    SourceStartTaskStage::Assigned(_) => "ingress_admission",
                    SourceStartTaskStage::Activating(_) => "activation_authority",
                    SourceStartTaskStage::InvokingFactory(_) => "factory_invocation",
                    SourceStartTaskStage::FactoryRefused(_) => "factory_refused",
                };
                tracing::warn!(target: "plurx::sharing", stage, error_class = ?error, "Source initial Start failed");
            }
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
            if result.is_err() && matches!(joined, SourceStartTaskJoined::Returned) {
                // The exact future returned before dispatch; its prepared inputs
                // have dropped. A fresh acquired handle or this invocation's
                // retained planned nonce can fence only its exact g0 attempt;
                // refusal retains custody instead of fabricating g1 proof.
                let cleanup = match &*entry.task.stage.lock().expect("Source start stage") {
                    SourceStartTaskStage::Claiming {
                        planned_incarnation,
                        intent,
                    } => {
                        // Only this joined invocation's fresh planned nonce and
                        // opaque intent survive an unknown claim; an exact g0
                        // CAS may fence it, never release an adopted g1 owner.
                        Some(SourceUninvokedCleanup::UncertainG0 {
                            planned_incarnation: *planned_incarnation,
                            intent: Box::new(intent.clone()),
                        })
                    }
                    SourceStartTaskStage::Acquired(binding)
                    | SourceStartTaskStage::Assigning(binding) => {
                        Some(SourceUninvokedCleanup::G0(binding.clone()))
                    }
                    SourceStartTaskStage::Assigned(assignment)
                    | SourceStartTaskStage::Activating(assignment) => {
                        Some(SourceUninvokedCleanup::G1(assignment.clone()))
                    }
                    SourceStartTaskStage::FactoryRefused(receipt) => {
                        Some(SourceUninvokedCleanup::G1(receipt.assignment().clone()))
                    }
                    _ => None,
                };
                *entry.task.cleanup.lock().expect("uninvoked Source custody") = cleanup;
                let _ = entry.release_uninvoked(&cleanup_state).await;
            }
            *entry.task.joined.lock().expect("actual Source task join") = Some(joined);
            *entry.result.lock().expect("Source HTTP outcome") = Some(result);
            entry.changed.notify_waiters();
        });
        *self
            .task
            .supervisor
            .lock()
            .expect("Source start supervisor owner") = Some(supervisor);
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
    /// its own typed code after a fresh invocation claim, before admission.
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
            end_deadline: std::sync::OnceLock::new(),
            local_custody: Default::default(),
            registration_gate: Default::default(),
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
    task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
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
    /// A joined error may still own an ambiguous durable/physical obligation.
    /// Only an exact cleanup receipt allows that acquired obligation to leave
    /// admission capacity. Failure before any claim owns no durable work here.
    fn actual_finished(&self) -> bool {
        self.result
            .lock()
            .expect("Source HTTP outcome")
            .as_ref()
            .is_some_and(|result| match result {
                Ok(owned) => owned.actor.settlement_status() == Some(Ok(())),
                Err(_) => {
                    self.task
                        .cleanup_confirmation
                        .lock()
                        .expect("Source cleanup confirmation")
                        .is_some()
                        || (matches!(
                            *self.task.joined.lock().expect("Source joined task"),
                            Some(SourceStartTaskJoined::Returned)
                        ) && matches!(
                            *self.task.stage.lock().expect("Source start stage"),
                            SourceStartTaskStage::Registered
                                | SourceStartTaskStage::ReadingIntent { .. }
                                | SourceStartTaskStage::IntentReady { .. }
                        ))
                }
            })
    }
    async fn release_uninvoked(
        &self,
        state: &crate::state::AppState,
    ) -> Result<(Uuid, Uuid), SourceStartFailure> {
        let cleanup = self
            .task
            .cleanup
            .lock()
            .expect("uninvoked Source custody")
            .clone()
            .ok_or(SourceStartFailure::Unresolved)?;
        if self
            .task
            .cleanup_confirmation
            .lock()
            .expect("Source cleanup confirmation")
            .is_none()
        {
            cleanup.release(state).await?;
            let mut confirmation = self
                .task
                .cleanup_confirmation
                .lock()
                .expect("Source cleanup confirmation");
            confirmation.get_or_insert_with(Uuid::new_v4);
        }
        Ok((
            cleanup.incarnation(),
            self.task
                .cleanup_confirmation
                .lock()
                .expect("Source cleanup confirmation")
                .expect("actual cleanup receipt"),
        ))
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
    fn end(
        self: &std::sync::Arc<Self>,
        state: std::sync::Arc<crate::state::AppState>,
    ) -> std::sync::Arc<SourceEndOwner> {
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
            if !failed
                || (!self.actual_settled()
                    && self
                        .task
                        .cleanup
                        .lock()
                        .expect("uninvoked Source custody")
                        .is_none())
            {
                return std::sync::Arc::clone(owner);
            }
        }
        let deadline = *self
            .end_deadline
            .get_or_init(|| std::time::Instant::now() + std::time::Duration::from_secs(305));
        let owner = std::sync::Arc::new(SourceEndOwner {
            task: std::sync::Mutex::new(None),
            changed: tokio::sync::Notify::new(),
            result: std::sync::Mutex::new(None),
        });
        *ending = Some(std::sync::Arc::clone(&owner));
        let entry = std::sync::Arc::clone(self);
        let task = std::sync::Arc::clone(&owner);
        // Insertion and detached spawn precede every wait/retirement await.
        // Disconnect loses only the HTTP waiter, never this actual obligation.
        let handle = tokio::spawn(async move {
            let result = async {
                // A live actor must request retirement before waiting for its
                // ingress writers: held response bodies finish on that signal.
                // The actor settles their custody before releasing its SQL row.
                let owned = match entry.wait(deadline).await {
                    Ok(owned) => owned,
                    Err(_) => {
                        if let Some(assignment) = ingress::actual_assignment(&entry) {
                            ingress::retire_custody(&state, &assignment, deadline)
                                .await
                                .map_err(|_| SourceStartFailure::Unresolved)?;
                            entry.changed.notify_waiters();
                        }
                        let (incarnation_id, confirmation_id) =
                            entry.release_uninvoked(&state).await?;
                        return Ok(SourceEndReceipt {
                            reference: entry.identity.reference.clone(),
                            request_id: entry.identity.request_id,
                            incarnation_id,
                            session_id: None,
                            control_epoch: None,
                            state: "settled",
                            confirmation_id,
                            settled: true,
                        });
                    }
                };
                owned
                    .actor
                    .retire_with_deadline(deadline)
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
        *owner.task.lock().expect("Source End task owner") = Some(handle);
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
    headers: &SourceHeaders,
    input: &SourceOperationInput,
    connection: Option<&crate::SharingConnectionCancellation>,
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
    ingress::register_local(
        state,
        connection.ok_or_else(unavailable)?,
        &entry,
        &owned.assignment,
        deadline,
    )
    .await?;
    Ok((entry, owned))
}
async fn live_operation_response(
    state: crate::state::AppState,
    headers: &SourceHeaders,
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
    let assignment = ingress::actual_assignment(entry).ok_or_else(unavailable)?;
    ingress::publication_allowed(&state, &assignment).await?;
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
    headers: SourceHeaders,
    axum::extract::Path((item, file, request)): axum::extract::Path<(String, String, String)>,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    body: axum::body::Body,
) -> Result<axum::response::Response, ApiError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(9);
    let bytes = axum::body::to_bytes(body, 128 * 1024)
        .await
        .map_err(|_| invalid())?;
    let (input, _) = parse_live_operation(&bytes, &item, &file, &request, false)?;
    let (entry, owned) = live_operation_owner(
        &state,
        &headers,
        &input,
        connection.as_ref().map(|value| &value.0),
        deadline,
    )
    .await?;
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
    headers: SourceHeaders,
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
    let (entry, owned) = live_operation_owner(
        &state,
        &headers,
        &input,
        connection.as_ref().map(|value| &value.0),
        deadline,
    )
    .await?;
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
    headers: SourceHeaders,
    axum::extract::Path((item, file, request)): axum::extract::Path<(String, String, String)>,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    body: axum::body::Body,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(9);
    let bytes = axum::body::to_bytes(body, 128 * 1024)
        .await
        .map_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_status.body", error_class = "invalid", "Source status refused"); invalid() })?;
    let input = parse_operation_request(&bytes, &item, &file, &request).inspect_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_status.input", error_class = "invalid", "Source status refused"); })?;
    let viewer = viewer_hash(&headers).inspect_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_status.viewer", error_class = "invalid", "Source status refused"); })?;
    let hash = source_credential_hash(&headers).inspect_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_status.credential", error_class = "invalid", "Source status refused"); })?;
    let current = current_reference(&state, &headers, &input.start.reference).await;
    tracing::debug!(target: "plurx::sharing", stage = "source_status.current_reference", current_authority = current.is_ok(), "Source status authority observation");
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
    .inspect_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_status.entry", error_class = "operation_refused", "Source status refused"); }).map_err(SourceStartFailure::response)?;
    entry
        .validate_known(input.known.as_ref())
        .inspect_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_status.known_lineage", error_class = "operation_refused", "Source status refused"); })
        .map_err(SourceStartFailure::response)?;
    let owned = entry
        .wait(deadline)
        .await
        .inspect_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_status.entry_wait", error_class = "operation_refused", "Source status refused"); })
        .map_err(SourceStartFailure::response)?;
    let ledger = state
        .store
        .source_ingress_custody(&owned.assignment)
        .await
        .map_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_status.ledger_read", error_class = "store", "Source status refused"); unavailable() })?
        .ok_or_else(|| { tracing::warn!(target: "plurx::sharing", stage = "source_status.ledger_read", error_class = "absent", "Source status refused"); unavailable() })?;
    let cleanup_only = headers.cleanup_only()
        || current.is_err()
        || ledger.state.is_sealed()
        || entry.ending.lock().expect("Source ending owner").is_some();
    if !cleanup_only {
        ingress::register_local(
            &state,
            &connection.as_ref().ok_or_else(|| { tracing::warn!(target: "plurx::sharing", stage = "source_status.connection", error_class = "absent", "Source status refused"); unavailable() })?.0,
            &entry,
            &owned.assignment,
            deadline,
        )
        .await.inspect_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_status.register_local", error_class = "operation_refused", "Source status refused"); })?;
    }
    if let (Ok((_, current_grant)), true) =
        (current.as_ref(), owned.actor.is_direct() && !cleanup_only)
    {
        if let Ok(response) = direct::published_reply(
            &state,
            &headers,
            &entry,
            &owned,
            &input.start.reference,
            *current_grant,
            deadline,
        )
        .await.inspect_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_status.direct_published", error_class = "operation_refused", "Source status refused"); })
        {
            return Ok(super::shared_library::guard_source_response(
                state,
                connection.map(|c| c.0),
                response,
            )
            .await);
        }
    }
    if current.is_ok() && !cleanup_only {
        if let Ok((response, guard)) = owned.actor.open_start_response(deadline).await.inspect_err(|error| { tracing::warn!(target: "plurx::sharing", stage = "source_status.vod_published", error_class = ?error, "Source status refused"); }) {
            ingress::publication_allowed(&state, &owned.assignment).await.inspect_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_status.publication_allowed", error_class = "operation_refused", "Source status refused"); })?;
            entry
                .observe_published(&owned, &response)
                .inspect_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_status.observed_reply", error_class = "operation_refused", "Source status refused"); })
                .map_err(SourceStartFailure::response)?;
            // The same actual full Start envelope/guard is used for live status;
            // neither stored JSON nor cleanup authority can establish readiness.
            let (_, current_grant) =
                current_reference(&state, &headers, &input.start.reference).await.inspect_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_status.current_recheck", error_class = "operation_refused", "Source status refused"); })?;
            if current_grant != entry.grant {
                tracing::warn!(target: "plurx::sharing", stage = "source_status.grant_recheck", error_class = "mismatch", "Source status refused");
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
    tracing::debug!(target: "plurx::sharing", stage = "source_status.cleanup_projection", cleanup_only, current_authority = current.is_ok(), "Source status returns terminal-only projection");
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
    headers: SourceHeaders,
    axum::extract::Path((item, file, request)): axum::extract::Path<(String, String, String)>,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    body: axum::body::Body,
) -> Result<axum::Json<SourceEndReceipt>, ApiError> {
    let bytes = axum::body::to_bytes(body, 128 * 1024)
        .await
        .map_err(|_| invalid())?;
    let input = parse_operation_request(&bytes, &item, &file, &request)?;
    let viewer = viewer_hash(&headers)?;
    let hash = source_credential_hash(&headers)?;
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
    let owner = entry.end(std::sync::Arc::new(state.clone()));
    let mut drivers = Vec::with_capacity(2);
    if let Some(ingress) = headers.forwarded_ingress() {
        drivers.push(ingress.driver.clone());
    }
    if let Some(connection) = connection.as_ref() {
        if let Ok(captured) = state
            .sharing
            .accepted_drivers
            .capture(&connection.0, &state.node_id)
        {
            drivers.push(captured.id().clone());
        }
    }
    for driver in drivers {
        if ingress::end_uses_registered_driver(&state, &entry, &driver).await? {
            // Either the ingress writer or a pooled internal writer may carry
            // existing media debt. Return before waiting for that same writer.
            return Err(SourceStartFailure::Unresolved.response());
        }
    }
    let receipt = owner
        // A different driver may await the retained owner for a bounded reply;
        // retries preserve the same task and original retirement budget.
        .wait(headers.deadline(std::time::Duration::from_secs(9)))
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
pub(crate) async fn settle_ingress_custody(
    state: &crate::state::AppState,
    assignment: &plurx_core::sharing_source_sessions::SourceDispatchAssignment,
    deadline: std::time::Instant,
    mode: crate::sharing_connection_custody::DriverCloseMode,
) -> Result<(), ApiError> {
    ingress::retire_custody_with_mode(state, assignment, deadline, mode).await
}
pub(crate) fn internal_forwarding_router() -> axum::Router<crate::state::AppState> {
    axum::Router::new()
        .route(
            forwarding::LOCATE_PATH,
            axum::routing::post(forwarding::locate)
                .layer(axum::extract::DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            forwarding::PREPARE_PATH,
            axum::routing::post(forwarding::prepare),
        )
        .route(
            forwarding::CONTROL_PATH,
            axum::routing::post(forwarding::receive_control),
        )
        .route(
            forwarding::FORWARD_PATH,
            axum::routing::post(forwarding::receive),
        )
        .route(
            forwarding::REGISTER_PATH,
            axum::routing::post(forwarding::register_http),
        )
        .route(
            forwarding::ACK_PATH,
            axum::routing::post(forwarding::ack_http),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            forwarding::MAX_WIRE_BYTES,
        ))
}
pub(crate) fn peer_router(state: crate::state::AppState) -> axum::Router<crate::state::AppState> {
    axum::Router::new()
        .route(
            "/sharing/v1/items/{item}/files/{file}/sessions",
            axum::routing::post(start),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
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
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            forwarding::route_to_owner,
        ))
}
#[cfg(test)]
pub(crate) struct SourceReadJobGate {
    pub(crate) entered: std::sync::atomic::AtomicBool,
    pub(crate) complete: std::sync::atomic::AtomicBool,
    pub(crate) file_closed: std::sync::atomic::AtomicBool,
    released: std::sync::Mutex<bool>,
    release: std::sync::Condvar,
    #[cfg(target_os = "linux")]
    core: std::sync::Arc<
        plurx_core::adaptive_resources::runtime::direct_source::DirectSourceReadGate,
    >,
}
#[cfg(test)]
impl Default for SourceReadJobGate {
    fn default() -> Self {
        Self {
            entered: std::sync::atomic::AtomicBool::new(false),
            complete: std::sync::atomic::AtomicBool::new(false),
            file_closed: std::sync::atomic::AtomicBool::new(false),
            released: std::sync::Mutex::new(false),
            release: std::sync::Condvar::new(),
            #[cfg(target_os = "linux")]
            core: plurx_core::adaptive_resources::runtime::direct_source::DirectSourceReadGate::new(
            ),
        }
    }
}
#[cfg(test)]
impl SourceReadJobGate {
    pub(crate) fn entered(&self) -> bool {
        self.entered.load(std::sync::atomic::Ordering::SeqCst) || {
            #[cfg(target_os = "linux")]
            {
                self.core.entered()
            }
            #[cfg(not(target_os = "linux"))]
            {
                false
            }
        }
    }
    pub(crate) fn complete(&self) -> bool {
        self.complete.load(std::sync::atomic::Ordering::SeqCst) || {
            #[cfg(target_os = "linux")]
            {
                self.core.complete()
            }
            #[cfg(not(target_os = "linux"))]
            {
                false
            }
        }
    }
    pub(crate) fn file_closed(&self) -> bool {
        self.file_closed.load(std::sync::atomic::Ordering::SeqCst) || {
            #[cfg(target_os = "linux")]
            {
                self.core.file_closed()
            }
            #[cfg(not(target_os = "linux"))]
            {
                false
            }
        }
    }
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
        #[cfg(target_os = "linux")]
        self.core.release();
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
pub(super) static SOURCE_READ_JOBS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(8)));
async fn source_file_body(
    file: crate::vodserve::ReadyFile,
    guard: std::sync::Arc<crate::transcode::source_actor::SourceResponseGuard>,
    len: u64,
    start: Option<u64>,
    #[cfg(test)] gate: Option<std::sync::Arc<SourceReadJobGate>>,
) -> Result<axum::body::Body, ApiError> {
    // Source custody currently selects the concrete legacy file owner. A
    // counted finite output has its own original native reader, which cannot
    // be converted into an uncounted Tokio file for this actor.
    let file = match file {
        crate::vodserve::ReadyFile::Local(file) => file,
        #[cfg(unix)]
        crate::vodserve::ReadyFile::Finite { .. } => return Err(unavailable()),
        // A selected copy object is likewise read only through its counted
        // native reader; it is never converted into an uncounted file here.
        #[cfg(target_os = "linux")]
        crate::vodserve::ReadyFile::Copy { .. } => return Err(unavailable()),
    };
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
    headers: SourceHeaders,
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
    ingress::register_local(
        &state,
        &connection.as_ref().ok_or_else(unavailable)?.0,
        &entry,
        &owned.assignment,
        deadline,
    )
    .await?;
    let opened = owned
        .actor
        .open_resource(&input.resource, deadline)
        .await
        .map_err(|e| SourceStartFailure::from(e).response())?;
    let (_, current_grant) =
        current_reference(&state, &headers, &input.operation.start.reference).await?;
    ingress::publication_allowed(&state, &owned.assignment).await?;
    if current_grant != grant {
        return Err(unavailable());
    }
    let (payload, guard) = opened.into_parts();
    let guard = std::sync::Arc::new(guard);
    let mut subtitle_complete = None;
    let mut subtitle_absent = false;
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
        crate::transcode::source_actor::SourceResourcePayload::CachedSubtitle {
            bytes,
            complete,
            absent,
        } => {
            if input.resource.cached_subtitle_revision().is_none() {
                return Err(unavailable());
            }
            subtitle_complete = Some(complete);
            subtitle_absent = absent;
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
    if let Some(complete) = subtitle_complete {
        response.headers_mut().insert(
            "x-plurx-subtitle-complete",
            axum::http::HeaderValue::from_static(if complete { "true" } else { "false" }),
        );
        response.headers_mut().insert(
            "x-plurx-subtitle-absent",
            axum::http::HeaderValue::from_static(if subtitle_absent { "true" } else { "false" }),
        );
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
    headers: SourceHeaders,
    axum::extract::Path((item, file)): axum::extract::Path<(String, String)>,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    body: axum::body::Body,
) -> Result<axum::response::Response, ApiError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(305);
    let bytes = axum::body::to_bytes(body, 128 * 1024)
        .await
        .map_err(|_| invalid())?;
    let input = parse_start_request(&bytes, &item, &file)?;
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
    let assignment = ingress::assigned(&state, &entry, deadline).await?;
    let connection = connection.ok_or_else(unavailable)?;
    ingress::register_local(&state, &connection.0, &entry, &assignment, deadline).await?;
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
    ingress::publication_allowed(&state, &owned.assignment).await?;
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
    headers: SourceHeaders,
    input: SourceStartInput,
    grant: Uuid,
    deadline: std::time::Instant,
    entry: std::sync::Arc<SourceStartEntry>,
) -> Result<SourceStartOwned, SourceStartFailure> {
    use plurx_core::sharing_source_sessions::{
        SourceClaimOutcome, SourceIntentRead, SourceSessionRequest, SourceWriteAuthorityRead,
    };
    let reference = input.reference.clone();
    // v2 identifies this complete canonical wire invocation, before fallible
    // physical preparation. Engine normalization remains in prepared inputs.
    use sha2::Digest;
    let mut invocation = sha2::Sha256::new();
    invocation.update(b"plurx.sharing-source-invocation.v2\0");
    invocation.update(&input.canonical_recipe);
    let invocation_fingerprint = format!("{:x}", invocation.finalize());
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
                principal: plurx_core::playback_principal::PlaybackPrincipal::sharing(
                    grant,
                    &entry.viewer,
                )
                .map_err(|_| SourceStartFailure::Unavailable)?,
                request_id: input.request_id.to_string(),
                request_fingerprint: invocation_fingerprint.clone(),
                playback_id: input.session.playback_id.clone(),
                incarnation_id: planned_incarnation,
                ingress_registry_boot_id: state.sharing.accepted_drivers.boot_id(),
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
        intent: (*intent).clone(),
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
    // Only this fresh Acquired factory may cover preparation refusal. A
    // historical or commit-unknown claim never reaches this stage.
    validate_initial_source_start(&input.session)?;
    let mut prepared = Box::pin(direct::prepare_source_start(
        &state,
        &headers,
        reference.clone(),
        input.session,
    ))
    .await?;
    prepared.bind_invocation(&binding)?;
    if prepared.fingerprint() != invocation_fingerprint {
        return Err(SourceStartFailure::Unresolved);
    }
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
    state
        .store
        .initialize_source_ingress_custody(&assignment)
        .await
        .map_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_start.ledger_initialize", error_class = "store", "Source initial admission refused"); SourceStartFailure::Unresolved })?;
    loop {
        let changed = entry.changed.notified();
        let ledger = state
            .store
            .source_ingress_custody(&assignment)
            .await
            .map_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_start.ledger_read", error_class = "store", "Source initial admission refused"); SourceStartFailure::Unresolved })?
            .ok_or_else(|| { tracing::warn!(target: "plurx::sharing", stage = "source_start.ledger_absent", error_class = "absent", "Source initial admission refused"); SourceStartFailure::Unresolved })?;
        if ledger.state.is_sealed() {
            tracing::warn!(target: "plurx::sharing", stage = "source_start.ledger_sealed", "Source initial admission refused");
            return Err(SourceStartFailure::Unresolved);
        }
        if ledger.state.open().next().is_some() {
            break;
        }
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), changed)
            .await
            .map_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_start.ledger_wait", error_class = "deadline", "Source initial admission refused"); SourceStartFailure::Unresolved })?;
    }

    let members = state
        .membership
        .observe_source_admission_members()
        .await
        .map_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_start.members", error_class = "store", "Source initial admission refused"); SourceStartFailure::Unresolved })?
        .ok_or_else(|| { tracing::warn!(target: "plurx::sharing", stage = "source_start.ingress_members_absent", error_class = "absent", "Source initial admission refused"); SourceStartFailure::Unresolved })?;
    let ingress_permission = state
        .store
        .prepare_source_ingress_admission(
            &assignment,
            state.sharing.accepted_drivers.boot_id(),
            &members,
        )
        .await
        .map_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_start.ingress_permission", error_class = "store", "Source initial admission refused"); SourceStartFailure::Unresolved })?
        .ok_or_else(|| { tracing::warn!(target: "plurx::sharing", stage = "source_start.ingress_permission_absent", error_class = "absent", "Source initial admission refused"); SourceStartFailure::Unresolved })?;
    entry.retain_stage(SourceStartTaskStage::Activating(assignment.clone()));
    let activation = match state
        .store
        .prepare_source_activation_authority(&assignment, &state.sharing.key, &members)
        .await
        .map_err(|_| { tracing::warn!(target: "plurx::sharing", stage = "source_start.activation_authority", error_class = "store", "Source initial admission refused"); SourceStartFailure::Unresolved })?
    {
        SourceWriteAuthorityRead::Ready(value) => *value,
        SourceWriteAuthorityRead::Unavailable => { tracing::warn!(target: "plurx::sharing", stage = "source_start.activation_authority", error_class = "refused", "Source initial admission refused"); return Err(SourceStartFailure::Unresolved); },
        SourceWriteAuthorityRead::Capacity => { tracing::warn!(target: "plurx::sharing", stage = "source_start.activation_authority", error_class = "capacity", "Source initial admission refused"); return Err(SourceStartFailure::Capacity); },
    };
    entry.retain_stage(SourceStartTaskStage::InvokingFactory(assignment.clone()));
    let actor = match direct::start_prepared_worker(
        std::sync::Arc::clone(&state),
        assignment.clone(),
        activation,
        *ingress_permission,
        prepared,
        deadline,
    )
    .await
    {
        Ok(actor) => actor,
        Err(receipt) => {
            let reason = receipt.reason();
            entry.retain_stage(SourceStartTaskStage::FactoryRefused(receipt));
            return Err(SourceStartFailure::from(reason));
        }
    };
    Ok(SourceStartOwned { actor, assignment })
}

// Read-only current authentication can precede owner insertion. It owns no
// durable or physical obligation and binds a stable grant across rotation.
async fn current_reference<H: AuthenticationHeaders + Send + Sync + ?Sized>(
    state: &crate::state::AppState,
    headers: &H,
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
fn source_credential_hash<H: AuthenticationHeaders + ?Sized>(
    headers: &H,
) -> Result<String, ApiError> {
    if let Some(hash) = headers.authenticated_hash() {
        return Ok(hash.to_owned());
    }
    let credential = super::sharing::credential(headers.raw_headers())?;
    Ok(plurx_core::sharing::secret_hash(
        plurx_core::sharing::SecretDomain::Grant,
        &credential,
    ))
}
pub(in crate::http) fn viewer_hash<H: AuthenticationHeaders + ?Sized>(
    headers: &H,
) -> Result<String, ApiError> {
    if let Some(plurx_core::playback_principal::PlaybackPrincipal::Sharing { viewer_key, .. }) =
        headers.authenticated_principal()
    {
        return Ok(viewer_key.as_str().to_owned());
    }
    let headers = headers.raw_headers();
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
async fn fixture_start(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    headers: HeaderMap,
    axum::extract::Path((item, file)): axum::extract::Path<(String, String)>,
    body: axum::body::Body,
) -> Result<axum::response::Response, ApiError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("actual Source fixture listener");
    let address = listener
        .local_addr()
        .expect("actual Source fixture address");
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(crate::serve_http(
        listener,
        super::sharing::peer_router(state),
        async move {
            let _ = stopped.await;
        },
        crate::HTTP_TIMEOUTS,
    ));
    let bytes = axum::body::to_bytes(body, 128 * 1024)
        .await
        .map_err(|_| invalid())?;
    let url = format!("http://{address}/sharing/v1/items/{item}/files/{file}/sessions");
    let response =
        tests::actual_resource_request(address, false, &url, headers, bytes.to_vec()).await;
    let _ = stop.send(());
    server
        .await
        .expect("actual fixture accept owner joined")
        .expect("actual fixture server drain");
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 128 * 1024)
        .await
        .map_err(|_| invalid())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let code = [
        "sharing_start_conflict",
        "sharing_start_capacity",
        "sharing_start_unsupported",
        SHARING_START_DOLBY_VISION_UNSUPPORTED,
        "sharing_start_unresolved",
        "sharing_start_unavailable",
        "sharing_start_deadline",
        "sharing_invalid_request",
        "sharing_playback_authority_unavailable",
        "sharing_body_authority_unavailable",
    ]
    .into_iter()
    .find(|code| Some(*code) == value.get("code").and_then(Value::as_str))
    .unwrap_or("sharing_source_fixture_refusal");
    Err(ApiError::typed(
        status,
        code,
        value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Source refused actual fixture HTTP request"),
    ))
}
#[cfg(test)]
pub(super) struct SourceFixtureMembershipOwner(tokio::task::JoinHandle<()>);
#[cfg(test)]
impl SourceFixtureMembershipOwner {
    pub(super) fn start(membership: plurx_core::cluster::membership::MembershipManager) -> Self {
        Self(tokio::spawn(membership.heartbeat_loop()))
    }
    pub(super) fn start_replication(
        replication: plurx_core::cluster::migration::status::ReplicationMonitor,
    ) -> Self {
        // Main owns this sampler alongside heartbeat. Peer placement requires
        // its actual local/quorum watermark, not heartbeat rows alone.
        Self(tokio::spawn(
            replication.passive_metrics_loop(std::future::pending()),
        ))
    }
    pub(super) async fn finish(&mut self) {
        self.0.abort();
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(15), &mut self.0)
            .await
            .expect("finite production membership owner join");
        assert!(stopped.is_err_and(|error| error.is_cancelled()));
    }
}
#[cfg(test)]
impl Drop for SourceFixtureMembershipOwner {
    fn drop(&mut self) {
        self.0.abort();
    }
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
    startup_clock: Option<std::sync::Arc<crate::StartupObservationHttp>>,
    membership_owner: SourceFixtureMembershipOwner,
    replication_owner: SourceFixtureMembershipOwner,
    _directory: tempfile::TempDir,
    _fixture_body_budget: tokio::sync::OwnedSemaphorePermit,
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
        drop(self.shutdown_retaining_fixture_budget().await);
    }
    pub(crate) async fn shutdown_retaining_fixture_budget(
        mut self,
    ) -> tokio::sync::OwnedSemaphorePermit {
        self.membership_owner.finish().await;
        self.replication_owner.finish().await;
        if let Some(clock) = &self.startup_clock {
            clock.stop_and_drain().await;
        }
        self.selected
            .shutdown()
            .await
            .expect("actual voter shutdown");
        self._fixture_body_budget
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
            fixture[cursor + 11..cursor + 13]
                .try_into()
                .expect("PGS length"),
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
    Box::pin(async move {
        let fixture_body_budget =
            crate::transcode::TranscodeManager::source_fixture_body_budget().await;
        // Keep this forwarding fixture frame small without changing the
        // original current-thread Direct test's scheduling or runtime.
        Box::pin(build_real_source_start_fixture(
            mode,
            recipient_server_id,
            None,
            fixture_body_budget,
        ))
        .await
    })
}
#[cfg(test)]
pub(crate) fn real_source_start_fixture_with_budget(
    mode: SourceFixtureMode,
    recipient_server_id: Option<Uuid>,
    fixture_body_budget: tokio::sync::OwnedSemaphorePermit,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = RealSourceStartFixture> + Send>> {
    Box::pin(build_real_source_start_fixture(
        mode,
        recipient_server_id,
        None,
        fixture_body_budget,
    ))
}
#[cfg(test)]
fn real_source_start_fixture_at(
    address: std::net::SocketAddr,
    fixture_body_budget: tokio::sync::OwnedSemaphorePermit,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = RealSourceStartFixture> + Send>> {
    Box::pin(build_real_source_start_fixture(
        SourceFixtureMode::Copy,
        None,
        Some(address),
        fixture_body_budget,
    ))
}
#[cfg(test)]
async fn build_real_source_start_fixture(
    mode: SourceFixtureMode,
    recipient_server_id: Option<Uuid>,
    advertised_http: Option<std::net::SocketAddr>,
    fixture_body_budget: tokio::sync::OwnedSemaphorePermit,
) -> RealSourceStartFixture {
    use plurx_core::{
        cluster::migration::select_daemon_store_observing,
        config::Config,
        domain::{ItemKind, LibraryKind, NewItem, NewLibrary},
        sharing::{new_secret, secret_hash, InvitationRecord, SecretDomain, ShareClaim, SourceId},
        sharing_catalogue_details::CatalogueRevisionKey,
        store::{keys, sharing_catalogue_details::SourceDetailsRead},
    };
    use std::sync::Arc;
    eprintln!("real Source HTTP fixture: entered original fixture body");
    let directory = crate::test_tempdir().expect("real Source fixture");
    let mut config = Config::default();
    config.storage.data_dir = directory.path().join("database");
    if let Some(address) = advertised_http {
        config.server.bind = address;
        config.cluster.artwork_url = format!("http://{address}");
    }
    let raft = std::net::TcpListener::bind("127.0.0.1:0").expect("Raft port");
    let api = std::net::TcpListener::bind("127.0.0.1:0").expect("API port");
    config.cluster.raft_bind = raft.local_addr().expect("Raft address");
    config.cluster.api_bind = api.local_addr().expect("API address");
    config.cluster.advertise_host = "localhost".into();
    drop((raft, api));
    crate::canonicalize_storage_roots(&mut config)
        .expect("production non-destructive storage preflight before Store ownership");
    let startup_clock =
        advertised_http.map(|address| Arc::new(crate::StartupObservationHttp::new(address)));
    eprintln!("real Source HTTP fixture: before original Store selector");
    let mut selected = if let Some(clock) = &startup_clock {
        Box::pin(select_daemon_store_observing(&config, Some(clock.as_ref()))).await
    } else {
        crate::sharing_fixture_clock::select_applied_singleton(&config).await
    }
    .expect("actual one voter");
    eprintln!("real Source HTTP fixture: original Store selector returned");
    selected
        .store
        .put_setting(keys::SHARING_ENABLED, "true")
        .await
        .expect("saved choice");
    // Run the real pre-serving factory before constructing State/listeners.
    // No manual candidate DDL or synthetic capability rows admit this fixture.
    eprintln!("real Source HTTP fixture: before original Source schema factory");
    assert!(Box::pin(selected.prepare_source_schema_before_serving())
        .await
        .expect("actual startup factory"));
    eprintln!("real Source HTTP fixture: original Source schema factory returned");
    #[cfg(target_os = "linux")]
    let direct_runtime = if matches!(mode, SourceFixtureMode::Direct) {
        eprintln!("real Source HTTP fixture: before original custody startup");
        Some(
            crate::resource_runtime::open_before_cleanup(&mut selected, &config)
                .await
                .expect("production direct custody startup")
                .expect("Linux direct custody runtime"),
        )
    } else {
        None
    };
    eprintln!("real Source HTTP fixture: original custody startup returned or not selected");
    let client = selected.local_client().expect("local voter client");
    eprintln!("real Source HTTP fixture: before original State construction");
    let mut state = Arc::new(crate::http::source_actor_test_state());
    eprintln!("real Source HTTP fixture: original State constructed");
    let state_mut = Arc::get_mut(&mut state).expect("sole initial State");
    #[cfg(target_os = "linux")]
    if let Some(runtime) = direct_runtime {
        state_mut.resource_runtime = Some(runtime);
    }
    state_mut.store = Arc::clone(&selected.store);
    state_mut.membership = selected.membership_manager();
    state_mut.clock_observer = if let Some(clock) = &startup_clock {
        clock
            .observer()
            .await
            .expect("actual startup clock observer")
    } else {
        crate::clock_offset::ClockObserver::new(state_mut.membership.clone())
    };
    state_mut.node_id = selected.identity.node_id.clone();
    state_mut.catalogue = selected.catalogue_reader();
    state_mut.replication = selected.replication_monitor();
    state_mut.media_sessions = crate::media_sessions::MediaSessionCoordinator::new(
        state_mut.membership.clone(),
        Arc::clone(&state_mut.store),
    );
    state_mut.sharing = Arc::new(crate::sharing::SharingManager::new(
        Arc::clone(&selected.credential_key),
        config.storage.data_dir.clone(),
        config.sharing.clone(),
    ));
    state_mut.node_id = selected.identity.node_id.clone();
    state_mut
        .membership
        .set_ingress_custody_boot(Some(state_mut.sharing.accepted_drivers.boot_id()));
    state_mut
        .membership
        .publish_ingress_custody_boot()
        .await
        .expect("actual registry boot publication before admission");

    let replication_owner =
        SourceFixtureMembershipOwner::start_replication(state.replication.clone());
    let membership_owner = SourceFixtureMembershipOwner::start(state.membership.clone());
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
        generate
            .arg("color=s=320x180:r=24")
            .args(["-t", "2", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-y"]);
    }
    eprintln!("real Source HTTP fixture: before actual fixture FFmpeg");
    let generated = generate.arg(&file).output().await.expect("actual FFmpeg");
    eprintln!("real Source HTTP fixture: actual fixture FFmpeg returned");
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
            mux.args(["-f", "sup", "-i"])
                .arg(&sup)
                .args(["-c:s", "copy"]);
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
            .execute(
                "UPDATE files SET hdr='dolby_vision' WHERE id=1",
                hiqlite::params!(),
            )
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
        startup_clock,
        membership_owner,
        replication_owner,
        _directory: directory,
        _fixture_body_budget: fixture_body_budget,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "sharing_source_adapter_tests.rs"]
mod actor_adapter_tests;

#[cfg(test)]
#[path = "shared_source_forwarding_tests.rs"]
mod actual_forwarding_tests;
