//! B-owned prepared successor for a directed change on a shared session.
//!
//! Since P0 every B session is its own Store and Source playback
//! (`receiver_playback_id`). A Source successor therefore cannot be a staged
//! preparation of the Source session it replaces: every Source owned-route
//! guard requires the session's own playback pointer and refuses any
//! `media_session_preparations` row naming its incarnation. A prepared
//! successor is instead an ordinary second B session for the same viewer and
//! player playback, started through the same owner, claim, Source Start,
//! attachment, publication and delivery grant as any shared Start. That one
//! path already counts both slots on both sides, replays a lost Start by its
//! request id and retires through the single retirement owner.
//!
//! What makes it *prepared* is local to B: until the client commits, the
//! successor never supersedes its predecessor on publication, refuses
//! progress beats, and is withdrawn (reason `Replaced`) when its predecessor
//! retires, when a newer ask replaces it, on an `aborted`/`failed`
//! acknowledgement, or at its deadline. A commit makes it an ordinary
//! session and supersedes the predecessor through the ordinary
//! make-before-break path, whose retirement owner sends the Source its End.
use super::*;
use crate::playback_control::{
    AcknowledgementState, ClientSelection, CodecPolicy, ControlAction, ControlRequestV1,
    ControlResponseV1, DynamicRangePolicy, EffectiveSelection, QualitySelection, SubtitleMode,
    PREPARE_REPLACEMENT_ACTION, SHARED_PREPARE_REPLACEMENT_ACTION,
};
use plurx_core::sharing_receiver_retirement::ReceiverRetirementReason;
use serde_json::Value;

/// The same bound a Local preparation has: the VOD lease plus the commit
/// margin. Past it an uncommitted successor is withdrawn and a late
/// acknowledgement is refused.
pub(super) const SUCCESSOR_DEADLINE_MS: i64 =
    crate::playback_control::VOD_LEASE_TIMEOUT_MS as i64 + 30_000;
/// Acknowledgement answers retained per predecessor for exact replay.
const MAX_ACK_REPLIES: usize = 8;

/// Whether this client will consume a `Prepare` naming a Shared successor.
/// `prepare_replacement` alone is the Local promise: a client that has not
/// also declared the Shared one would keep beating progress on the session it
/// left, so it is answered `none` and reopens (P0).
pub(super) fn accepts_shared_successor(request: &ControlRequestV1) -> bool {
    request.accepts(PREPARE_REPLACEMENT_ACTION)
        && request.accepts(SHARED_PREPARE_REPLACEMENT_ACTION)
}

/// Carried by a session B started as a prepared successor.
#[derive(Clone, Copy, Debug)]
pub(super) struct PreparedRole {
    deadline_ms: i64,
    committed: bool,
}

/// Directed-change bookkeeping on the session the client is watching.
#[derive(Default)]
pub(super) struct HandoffState {
    /// The ask this session was started for or last staged. The first
    /// accepted exchange records it and dispatches nothing.
    dispatched_digest: Option<String>,
    /// The latest `dual_player_preparation` the client declared.
    dual_player: bool,
    /// The one prepared successor slot.
    slot: Option<SuccessorSlot>,
    /// A successor withdrawn for a newer ask. The newer ask is dispatched
    /// only once this has fully retired and freed its slots.
    withdrawing: Option<Arc<ReceiverStartInner>>,
    pub(super) replies: AckReplies,
}

struct SuccessorSlot {
    successor: Arc<ReceiverStartInner>,
    digest: String,
    deadline_ms: i64,
    /// Minted by the first exchange that offers this successor and reused by
    /// every later offer, so an acknowledgement names this exact staging.
    action_id: Option<String>,
    committed: bool,
}

struct AckReply {
    client_instance_id: String,
    sequence: u64,
    fingerprint: String,
    body: Vec<u8>,
}

/// The exact bytes answered to this session's latest acknowledgement
/// exchanges, bounded. They move to the registry tombstone when the retired
/// session is pruned, so a lost commit answer still replays.
#[derive(Default)]
pub(super) struct AckReplies(std::collections::VecDeque<AckReply>);

impl AckReplies {
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// The earlier answer to this exchange (same client instance and
    /// sequence): exact for the same request, `Changed` for another one.
    pub(super) fn replay(&self, request: &ControlRequestV1) -> Option<AckReplay> {
        request.acknowledgement.as_ref()?;
        let fingerprint = request.fingerprint()?;
        let reply = self.0.iter().find(|reply| {
            reply.client_instance_id == request.client_instance_id
                && reply.sequence == request.sequence
        })?;
        Some(if reply.fingerprint == fingerprint {
            AckReplay::Exact(reply.body.clone())
        } else {
            AckReplay::Changed
        })
    }
    fn retain(&mut self, request: &ControlRequestV1, body: &[u8]) {
        let Some(fingerprint) = request.fingerprint() else {
            return;
        };
        if self.0.len() == MAX_ACK_REPLIES {
            self.0.pop_front();
        }
        self.0.push_back(AckReply {
            client_instance_id: request.client_instance_id.clone(),
            sequence: request.sequence,
            fingerprint,
            body: body.to_vec(),
        });
    }
}

/// What an acknowledgement asks of the current slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AckPlan {
    Commit(String),
    Abort(String),
    Report,
}

/// An exact earlier acknowledgement answer, or a refusal of a changed one.
pub(super) enum AckReplay {
    Exact(Vec<u8>),
    Changed,
}

impl HandoffState {
    fn take_uncommitted(&mut self) -> Option<Arc<ReceiverStartInner>> {
        if self.slot.as_ref().is_some_and(|slot| !slot.committed) {
            return self.slot.take().map(|slot| slot.successor);
        }
        None
    }
}

impl ReceiverStartInner {
    /// A prepared successor the client has not committed to.
    pub(super) fn awaiting_commit(&self) -> bool {
        self.state
            .lock()
            .expect("receiver owner")
            .prepared
            .is_some_and(|role| !role.committed)
    }
    /// An uncommitted successor past its deadline.
    pub(super) fn prepared_expired(&self, now_ms: i64) -> bool {
        self.state
            .lock()
            .expect("receiver owner")
            .prepared
            .is_some_and(|role| !role.committed && now_ms >= role.deadline_ms)
    }
    /// The withdrawn successor a predecessor's retirement hands on.
    pub(super) fn take_uncommitted_successor(&self) -> Option<Arc<ReceiverStartInner>> {
        self.state
            .lock()
            .expect("receiver owner")
            .handoff
            .take_uncommitted()
    }
    fn published_hls(&self) -> Option<Box<StartResponse>> {
        let owned = self.state.lock().expect("receiver owner");
        if owned.retirement_started {
            return None;
        }
        match owned.start.as_ref() {
            Some(Ok(ReceiverPublished::Hls(start))) => Some(start.clone()),
            _ => None,
        }
    }
    fn live(&self) -> bool {
        let owned = self.state.lock().expect("receiver owner");
        !owned.retirement_started && !matches!(owned.start, Some(Err(_)))
    }
}

/// The complete retained request for a successor: the predecessor's own
/// request with the new selection, the sampled position, a fresh request id
/// and no lineage fields. The Source re-plans it exactly as it plans any
/// shared Start. `None` is a selection a shared Start cannot carry (a burn,
/// an explicit codec or grade, a negotiated candidate, direct play), and the
/// client then reopens on `preparation: none`.
///
/// Fields are removed rather than nulled, because the Source closes every
/// provided field against the typed create body.
pub(super) fn successor_request(
    original: &str,
    selection: &ClientSelection,
    start_ms: i64,
    request_id: Uuid,
) -> Option<String> {
    if selection.codec != CodecPolicy::Auto || selection.dynamic_range != DynamicRangePolicy::Auto {
        return None;
    }
    let mut value: Value = serde_json::from_str(original).ok()?;
    let object = value.as_object_mut()?;
    if object.get("presentation").and_then(Value::as_str)
        == Some(super::super::sharing_direct_wire::DIRECT_PRESENTATION)
        || [
            "previous_session_id",
            "control_sequence",
            "reopen_reason",
            "intent",
            "subtitle_burn",
        ]
        .iter()
        .any(|key| object.get(*key).is_some_and(|value| !value.is_null()))
    {
        return None;
    }
    for key in [
        "previous_session_id",
        "control_sequence",
        "reopen_reason",
        "intent",
        "subtitle_burn",
        "subtitle_burn_sdr",
    ] {
        object.remove(key);
    }
    object.insert("request_id".into(), request_id.to_string().into());
    #[allow(clippy::cast_precision_loss)] // A validated film position, far below 2^53 ms.
    let start = start_ms.max(0) as f64 / 1_000.0;
    object.insert("start".into(), serde_json::json!(start));
    let mut overrides = match object.remove("overrides") {
        Some(Value::Object(map)) => map,
        Some(Value::Null) | None => serde_json::Map::new(),
        Some(_) => return None,
    };
    overrides.remove("force");
    match selection.quality {
        QualitySelection::Auto {
            candidate_id: Some(_),
            ..
        } => return None,
        QualitySelection::Auto { height, .. } => {
            object.insert("quality_auto".into(), true.into());
            object.remove("copy");
            match height {
                Some(height) => object.insert("height".into(), height.into()),
                None => object.remove("height"),
            };
        }
        QualitySelection::Original => {
            object.insert("quality_auto".into(), false.into());
            object.insert("copy".into(), true.into());
            object.remove("height");
            overrides.insert("force".into(), "original".into());
        }
        QualitySelection::Manual { height } => {
            object.insert("quality_auto".into(), false.into());
            object.insert("copy".into(), false.into());
            object.insert("height".into(), height.into());
            overrides.insert("force".into(), "transcode".into());
        }
    }
    if !overrides.is_empty() {
        object.insert("overrides".into(), Value::Object(overrides));
    }
    match selection.audio_track {
        Some(track) => object.insert("audio".into(), track.into()),
        None => object.remove("audio"),
    };
    object.insert("audio_offset_ms".into(), selection.audio_offset_ms.into());
    match selection.subtitle.mode {
        SubtitleMode::Burn => return None,
        SubtitleMode::Native => {
            object.insert("native_subtitles".into(), true.into());
            object.insert("subtitle".into(), selection.subtitle.track?.into());
        }
        SubtitleMode::Off | SubtitleMode::Overlay => {
            object.remove("subtitle");
        }
    }
    serde_json::to_string(&value).ok()
}

/// The answer an offered successor will deliver, from its own published
/// Start and complete request. A VOD Start names no encoder route, so the
/// codec is `source` only for the explicit Original ask.
fn successor_effective(request_json: &str, start: &StartResponse) -> EffectiveSelection {
    let request: Value = serde_json::from_str(request_json).unwrap_or(Value::Null);
    EffectiveSelection {
        candidate_id: None,
        quality_auto: request["quality_auto"].as_bool().unwrap_or(false),
        height: start.height,
        audio_track: request["audio"].as_i64(),
        subtitle_burn: None,
        audio_offset_ms: request["audio_offset_ms"].as_i64().unwrap_or(0),
        codec: if request["copy"].as_bool() == Some(true) {
            "source"
        } else {
            "server_selected"
        }
        .to_owned(),
        dynamic_range: start.delivered_dynamic_range.clone(),
    }
}

impl ReceiverStartRegistry {
    /// Called by an owner once its Start is published. An ordinary Start
    /// supersedes its player's older attempts at once (P0 make-before-break);
    /// a prepared successor waits for the client's commit, and its
    /// predecessor keeps serving until then.
    pub(super) fn supersede_on_publication(
        &self,
        state: &Arc<AppState>,
        published: &Arc<ReceiverStartInner>,
    ) -> usize {
        if published.awaiting_commit() {
            return 0;
        }
        self.supersede_predecessors(state, published)
    }

    /// Register a successor for `predecessor` without starting it. The
    /// successor shares the viewer, original login, file and player playback
    /// id, and has its own request and Source request identity.
    fn register_successor(
        &self,
        predecessor: &ReceiverStartActor,
        request_json: String,
        digest: &str,
        deadline_ms: i64,
    ) -> Result<(Arc<ReceiverStartInner>, String), ReceiverStartError> {
        let previous = &predecessor.0;
        if previous.direct {
            return Err(ReceiverStartError::Unsupported);
        }
        let request: Value =
            serde_json::from_str(&request_json).map_err(|_| ReceiverStartError::Conflict)?;
        let request_id = request
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or(ReceiverStartError::Conflict)?
            .to_owned();
        let mut intent = previous.intent.clone();
        intent.recipe.source_request_id = Uuid::new_v4();
        intent.recipe.request_json = request_json;
        intent.source_position_ms = 0;
        let wrapper = crate::sharing::receiver_source_wrapper(&intent.recipe)
            .map_err(|_| ReceiverStartError::Conflict)?;
        let (entry, created) =
            self.register(intent, request_id, &previous.playback_id, &wrapper)?;
        if !created {
            return Err(ReceiverStartError::Conflict);
        }
        {
            let mut owned = entry.state.lock().expect("receiver owner");
            owned.prepared = Some(PreparedRole {
                deadline_ms,
                committed: false,
            });
            // Its own first exchange is not a change from what it was staged for.
            owned.handoff.dispatched_digest = Some(digest.to_owned());
        }
        Ok((entry, wrapper))
    }

    /// Start a prepared successor through the ordinary owner. Marked prepared
    /// before its owner task can run, so its publication never supersedes.
    pub(super) fn begin_successor(
        &self,
        state: Arc<AppState>,
        predecessor: &ReceiverStartActor,
        request_json: String,
        digest: &str,
        deadline_ms: i64,
    ) -> Result<ReceiverStartActor, ReceiverStartError> {
        let (entry, wrapper) =
            self.register_successor(predecessor, request_json, digest, deadline_ms)?;
        Ok(self.spawn_owner(state, entry, wrapper))
    }
}

impl ReceiverStartActor {
    /// Record the ask of one Source-accepted exchange and decide whether it
    /// dispatches a successor. Returns the digest to stage.
    ///
    /// The rule is Local's (`take_preparation_dispatch`): the first exchange
    /// records the ask the session was created for; a different ask
    /// dispatches once; an ask arriving while the slot is busy stays
    /// undispatched and the first exchange after the slot frees picks it up.
    /// A staged successor for an ask the client has since left is withdrawn.
    pub(super) fn observe_ask(
        &self,
        state: &Arc<AppState>,
        request: &ControlRequestV1,
        enabled: bool,
    ) -> Option<String> {
        // An uncommitted successor is not what the viewer watches: it stages
        // nothing of its own until the client commits to it.
        if self.0.awaiting_commit() {
            return None;
        }
        let digest = request.selection.desired().digest();
        let (stage, withdrawn) =
            {
                let mut owned = self.0.state.lock().expect("receiver owner");
                let handoff = &mut owned.handoff;
                if let Some(capabilities) = request.capabilities.as_ref() {
                    handoff.dual_player = capabilities.dual_player_preparation;
                }
                if handoff
                    .slot
                    .as_ref()
                    .is_some_and(|slot| !slot.committed && !slot.successor.live())
                {
                    handoff.slot = None;
                }
                if handoff.withdrawing.as_ref().is_some_and(|successor| {
                    successor.state.lock().expect("receiver owner").retired
                }) {
                    handoff.withdrawing = None;
                }
                let withdrawn = if handoff
                    .slot
                    .as_ref()
                    .is_some_and(|slot| !slot.committed && slot.digest != digest)
                {
                    handoff.take_uncommitted()
                } else {
                    None
                };
                if let Some(successor) = withdrawn.as_ref() {
                    handoff.withdrawing = Some(Arc::clone(successor));
                }
                let stage = match handoff.dispatched_digest.as_deref() {
                    None => {
                        handoff.dispatched_digest = Some(digest.clone());
                        None
                    }
                    Some(dispatched)
                        if dispatched != digest
                            && handoff.slot.is_none()
                            && handoff.withdrawing.is_none()
                            && handoff.dual_player
                            && enabled
                            && accepts_shared_successor(request) =>
                    {
                        handoff.dispatched_digest = Some(digest.clone());
                        Some(digest)
                    }
                    Some(_) => None,
                };
                (stage, withdrawn)
            };
        if let Some(successor) = withdrawn {
            ReceiverStartActor(successor)
                .begin_retirement(state.clone(), ReceiverRetirementReason::Replaced);
        }
        stage
    }

    /// Start the successor for a dispatched ask at the film time this
    /// exchange accepted. Any refusal (a selection a shared Start cannot
    /// carry, a full registry, or a Source that later refuses the Start) is a
    /// typed decline: no slot, the answer is `none`, the client reopens.
    pub(super) fn stage_successor(
        &self,
        state: &Arc<AppState>,
        request: &ControlRequestV1,
        duration_ms: Option<i64>,
        digest: String,
    ) {
        // Bounded by the film itself, as Local bounds its seam: the looser
        // validation slack past the end is not a place a successor may begin.
        let asked = request.seek_target_ms.unwrap_or(request.position_ms).max(0);
        let film_ms = match duration_ms {
            Some(duration) if duration > 0 => asked.min(duration),
            _ => asked,
        };
        let Some(request_json) = successor_request(
            &self.0.intent.recipe.request_json,
            &request.selection,
            film_ms,
            Uuid::new_v4(),
        ) else {
            return;
        };
        let deadline_ms = clock_ms().saturating_add(SUCCESSOR_DEADLINE_MS);
        let Ok(successor) = state.sharing.receiver_starts.begin_successor(
            state.clone(),
            self,
            request_json,
            &digest,
            deadline_ms,
        ) else {
            return;
        };
        if !self.install_successor(&successor, digest, deadline_ms) {
            successor.begin_retirement(state.clone(), ReceiverRetirementReason::Replaced);
        }
    }

    /// Take ownership of a just-started successor. `false` when the slot was
    /// taken or this session began retiring meanwhile; the caller withdraws
    /// the successor it started.
    pub(super) fn install_successor(
        &self,
        successor: &ReceiverStartActor,
        digest: String,
        deadline_ms: i64,
    ) -> bool {
        let mut owned = self.0.state.lock().expect("receiver owner");
        if owned.retirement_started || owned.handoff.slot.is_some() {
            return false;
        }
        owned.handoff.slot = Some(SuccessorSlot {
            successor: Arc::clone(&successor.0),
            digest,
            deadline_ms,
            action_id: None,
            committed: false,
        });
        true
    }

    /// B's own preparation answer for a client that accepts
    /// `prepare_replacement`. `offered` restates a `Prepare` action naming
    /// only B's successor session, playlist and control bootstrap; `staging`
    /// is a successor for this exact ask that has not published yet (or an
    /// outranking action this exchange); anything else is `none`, and the
    /// client reopens.
    pub(super) fn compose_preparation(
        &self,
        response: &mut ControlResponseV1,
        request: &ControlRequestV1,
        now_ms: i64,
    ) {
        if !accepts_shared_successor(request) {
            return;
        }
        let digest = request.selection.desired().digest();
        let mut owned = self.0.state.lock().expect("receiver owner");
        let slot =
            owned.handoff.slot.as_mut().filter(|slot| {
                !slot.committed && slot.digest == digest && slot.deadline_ms > now_ms
            });
        let answer = match slot {
            None => "none",
            Some(slot) if !slot.successor.live() => "none",
            Some(slot) => match slot.successor.published_hls() {
                None => "staging",
                Some(_) if !matches!(response.action, ControlAction::None) => "staging",
                Some(start) => {
                    let action_id = slot
                        .action_id
                        .get_or_insert_with(|| Uuid::new_v4().to_string())
                        .clone();
                    response.action = ControlAction::Prepare {
                        action_id,
                        session_id: start.session_id.clone(),
                        playlist_url: start.playlist_url.clone(),
                        control: start.control.clone().map(Box::new),
                        media_origin_ms: start.media_origin_ms.unwrap_or(0),
                        effective_selection: successor_effective(
                            &slot.successor.intent.recipe.request_json,
                            &start,
                        ),
                    };
                    "offered"
                }
            },
        };
        response.delivery.preparation = Some(answer.to_owned());
    }

    /// Decide an acknowledgement against the current slot before anything is
    /// sent to the Source. `Err` is a stale acknowledgement, refused exactly:
    /// an action id this slot never offered, a late one past the deadline
    /// (which withdraws the successor), or a commit whose ask, timeline origin
    /// or successor no longer matches the offer.
    pub(super) fn plan_acknowledgement(
        &self,
        state: &Arc<AppState>,
        request: &ControlRequestV1,
        now_ms: i64,
    ) -> Result<AckPlan, ()> {
        let ack = request.acknowledgement.as_ref().ok_or(())?;
        let (plan, expired) = {
            let mut owned = self.0.state.lock().expect("receiver owner");
            let Some(slot) = owned.handoff.slot.as_ref().filter(|slot| {
                !slot.committed && slot.action_id.as_deref() == Some(ack.action_id.as_str())
            }) else {
                return Err(());
            };
            if now_ms >= slot.deadline_ms {
                (Err(()), owned.handoff.take_uncommitted())
            } else {
                let plan = match ack.state {
                    AcknowledgementState::Committed => {
                        let offered = slot.successor.published_hls();
                        if offered.as_ref().is_some_and(|start| {
                            ack.committed_media_origin_ms
                                == Some(start.media_origin_ms.unwrap_or(0))
                        }) && request.selection.desired().digest() == slot.digest
                        {
                            Ok(AckPlan::Commit(ack.action_id.clone()))
                        } else {
                            Err(())
                        }
                    }
                    AcknowledgementState::Failed | AcknowledgementState::Aborted => {
                        Ok(AckPlan::Abort(ack.action_id.clone()))
                    }
                    AcknowledgementState::MetadataReady
                    | AcknowledgementState::BufferReady
                    | AcknowledgementState::Switched => Ok(AckPlan::Report),
                };
                (plan, None)
            }
        };
        if let Some(successor) = expired {
            ReceiverStartActor(successor)
                .begin_retirement(state.clone(), ReceiverRetirementReason::Replaced);
        }
        plan
    }

    /// The commit, after the Source accepted the committing exchange: the
    /// successor becomes an ordinary session and supersedes this one (and any
    /// older attempt of the same player) through the single retirement owner.
    pub(super) fn commit_successor(
        &self,
        state: &Arc<AppState>,
        registry: &ReceiverStartRegistry,
        action_id: &str,
    ) -> Result<usize, ReceiverStartError> {
        let successor = {
            let owned = self.0.state.lock().expect("receiver owner");
            owned
                .handoff
                .slot
                .as_ref()
                .filter(|slot| !slot.committed && slot.action_id.as_deref() == Some(action_id))
                .map(|slot| Arc::clone(&slot.successor))
                .ok_or(ReceiverStartError::Conflict)?
        };
        if successor.published_hls().is_none() {
            return Err(ReceiverStartError::Conflict);
        }
        {
            let mut owned = self.0.state.lock().expect("receiver owner");
            let slot = owned
                .handoff
                .slot
                .as_mut()
                .filter(|slot| Arc::ptr_eq(&slot.successor, &successor) && !slot.committed)
                .ok_or(ReceiverStartError::Conflict)?;
            slot.committed = true;
        }
        if let Some(role) = successor
            .state
            .lock()
            .expect("receiver owner")
            .prepared
            .as_mut()
        {
            role.committed = true;
        }
        Ok(registry.supersede_predecessors(state, &successor))
    }

    /// Withdraw the uncommitted successor named by `action_id`.
    pub(super) fn abort_successor(&self, state: &Arc<AppState>, action_id: &str) -> bool {
        let successor =
            {
                let mut owned = self.0.state.lock().expect("receiver owner");
                if owned.handoff.slot.as_ref().is_some_and(|slot| {
                    !slot.committed && slot.action_id.as_deref() == Some(action_id)
                }) {
                    owned.handoff.take_uncommitted()
                } else {
                    None
                }
            };
        let Some(successor) = successor else {
            return false;
        };
        ReceiverStartActor(successor)
            .begin_retirement(state.clone(), ReceiverRetirementReason::Replaced);
        true
    }

    /// The exact earlier answer to this acknowledgement exchange, if any.
    /// A replay performs no write and sends nothing to the Source.
    pub(super) fn acknowledgement_replay(&self, request: &ControlRequestV1) -> Option<AckReplay> {
        self.0
            .state
            .lock()
            .expect("receiver owner")
            .handoff
            .replies
            .replay(request)
    }

    pub(super) fn has_acknowledgement_replies(&self) -> bool {
        !self
            .0
            .state
            .lock()
            .expect("receiver owner")
            .handoff
            .replies
            .is_empty()
    }

    /// Retain the exact bytes answered to an acknowledgement exchange.
    pub(super) fn retain_acknowledgement_reply(&self, request: &ControlRequestV1, body: &[u8]) {
        self.0
            .state
            .lock()
            .expect("receiver owner")
            .handoff
            .replies
            .retain(request, body);
    }
}

#[cfg(test)]
#[path = "shared_receiver_successor_tests.rs"]
mod tests;
