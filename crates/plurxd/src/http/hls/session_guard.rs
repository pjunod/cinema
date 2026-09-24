use super::*;

#[derive(Deserialize)]
pub struct StartQuery {
    /// Target height (e.g. 1080, 720). Omitted means Auto (server-chosen).
    /// Ignored when `copy=1`.
    pub height: Option<i64>,
    /// Start offset in seconds (resume / seek).
    pub start: Option<f64>,
    /// Audio stream to use (`a:{audio}`); overrides the automatic pick.
    pub audio: Option<i64>,
    /// `copy=1` → a copy-video HLS session (repackage the source video into
    /// fMP4 HLS untouched, transcode audio only). For players that can't take a
    /// progressive fMP4 remux but decode HEVC/HDR natively via HLS (Safari).
    pub copy: Option<u8>,
    /// With `copy`: `aac=1` transcodes the audio to AAC (the codec the client
    /// can't take), `aac=0` copies it. The client knows which from `/decision`.
    pub aac: Option<u8>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StartResponse {
    pub session_id: String,
    pub playlist_url: String,
    pub duration_ms: Option<i64>,
    pub start_seconds: f64,
    /// Source-timeline timestamp represented by player-local time zero.
    ///
    /// Additive for new clients. A copy session can start on the keyframe
    /// before `start_seconds`, while accurate transcodes start at the
    /// requested position. Old clients continue using `start_seconds`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_origin_ms: Option<i64>,
    /// The normalized output height this request actually received. For a
    /// bound stall reopen this is the persisted one-rung-down answer, repeated
    /// unchanged on a transport replay of the same `request_id`.
    pub height: i64,
    pub encoder: String,
    /// The whole stream already exists on disk (a pre-transcode cache hit).
    /// The player treats it like direct play: seek by `currentTime`, and don't
    /// arm the stall watchdog's restart — see `StartInfo::vod`.
    pub vod: bool,
    /// The quality ladder for this source, top rung first — the rungs the
    /// menu and the Auto controller move between, so the client never
    /// hardcodes them (ADAPTIVE-QUALITY.md Phase 1).
    pub ladder: Vec<crate::transcode::Rung>,
    /// Node-local sustained-throughput prior used for this Auto start.
    /// Additive and absent while the opt-in feature is disabled or cold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prior_kbps: Option<u32>,
    /// What dynamic range the bytes of *this session* carry
    /// (`"dolby_vision" | "hdr10" | "hlg" | "sdr"`). It overrides the
    /// decision's answer the moment the session attaches, because a burn or
    /// a manually-picked rung forces a transcode the decision never promised
    /// (MEDIA-BADGES-PLAN §3.2). Absent when the source file vanished from
    /// the store mid-request: the client keeps whatever it had.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_dynamic_range: Option<String>,
    /// The Dolby Vision profile *this session's* bytes carry, when it is
    /// known. Absent for a transcode, a strip, a source that never had Dolby
    /// Vision — and for a pre-M2 row whose label names no profile, which is
    /// why absence means "no answer" rather than "not Dolby Vision".
    /// `delivered_dynamic_range` beside it is the field that answers that.
    ///
    /// The one thing the field above cannot say. A Profile 7 title preserved
    /// for a device that enumerates 7 and the same title converted to 8.1 for
    /// a device that does not are both `"dolby_vision"`, and the badge that
    /// spells both `DV P7` is telling one of them something untrue about its
    /// own file (MEDIA-BADGES-PLAN §2.3). It overrides the decision's answer
    /// for the same reason the range does: a burn or a forced rung produces a
    /// session the decision never promised.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_dolby_vision_profile: Option<u8>,
    /// Optional behavior-neutral v1 control capability. It is persisted with
    /// the idempotent create result so a setting change cannot mutate the wire
    /// contract of an already-open session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<crate::playback_control::ControlBootstrap>,
    /// Why this session's plan is not the one the client asked for, when it
    /// isn't (PLAYBACK-CAPS-V2-PLAN §4.5). Every entry is either a named
    /// override the client itself supplied or a `plan_mismatch:` line naming
    /// the field, the client's value and the server's.
    ///
    /// Empty and omitted on the wire for every agreeing create, which is the
    /// case this exists to make visible by its absence. The stats overlay
    /// shows these next to the badge: a viewer whose Dolby Vision title is
    /// playing as HDR10 can read *why* without an admin reading the log.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plan_notes: Vec<String>,
}

pub(super) const PLANNING_CAPS_RESPONSE_FIELD: &str = "planning_caps";
pub(super) const PLANNING_OVERRIDES_RESPONSE_FIELD: &str = "planning_overrides";
pub(super) const PREPARATION_REASON_RESPONSE_FIELD: &str = "preparation_reason";

/// Read the create-time planning snapshot from the additive durable response.
///
/// The worker envelope intentionally denies unknown fields because it crosses
/// mixed-version nodes. `StartResponse` is already additive, so keeping this
/// server-owned sidecar beside its durable copy preserves legacy worker
/// activation while allowing a new owner to re-plan after takeover.
pub(super) fn retained_planning_caps(
    response_json: &str,
) -> Option<plurx_core::playback::DeviceCaps> {
    serde_json::from_str::<serde_json::Value>(response_json)
        .ok()?
        .get(PLANNING_CAPS_RESPONSE_FIELD)
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

pub(super) fn retained_planning_overrides(response_json: &str) -> Option<CreateOverrides> {
    serde_json::from_str::<serde_json::Value>(response_json)
        .ok()?
        .get(PLANNING_OVERRIDES_RESPONSE_FIELD)
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

/// Owns a published worker until the replicated activation has a definitive
/// outcome. Request futures are cancellation points at every store/network
/// await; tying cleanup to this value prevents a disconnected client from
/// leaving an encoder and its durable start claim behind.
pub(in crate::http) struct StartedSessionGuard {
    cleanup: Option<StartedSessionCleanup>,
}

pub(super) struct MediaSessionRequestGuard {
    cleanup: Option<(AppState, i64, String, String)>,
}

async fn settle_media_session_request_claim(
    state: &AppState,
    user_id: i64,
    request_id: &str,
    incarnation_id: &str,
) {
    let deadline = tokio::time::Instant::now() + REQUEST_CLAIM_SETTLEMENT_BUDGET;
    loop {
        match tokio::time::timeout_at(
            deadline,
            state
                .store
                .fail_media_session_request(user_id, request_id, incarnation_id, unix_ms()),
        )
        .await
        {
            // `false` is also settled: the exact claim already resolved or a
            // newer incarnation owns it, so this cleanup must not touch it.
            Ok(Ok(_)) => return,
            Ok(Err(error)) => {
                tracing::warn!(
                    %error,
                    user_id,
                    "media-session request cleanup is retrying"
                );
            }
            Err(_) => break,
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::time::sleep(REQUEST_CLAIM_SETTLEMENT_RETRY_DELAY.min(remaining)).await;
    }
    tracing::error!(
        user_id,
        retry_after_ms = 60_000,
        "media-session request cleanup exhausted its bound; claim expiry remains the durable fallback"
    );
}

impl MediaSessionRequestGuard {
    pub(super) fn new(
        state: AppState,
        user_id: i64,
        request_id: String,
        incarnation_id: String,
    ) -> Self {
        Self {
            cleanup: Some((state, user_id, request_id, incarnation_id)),
        }
    }

    pub(super) fn disarm(&mut self) {
        self.cleanup = None;
    }
}

impl Drop for MediaSessionRequestGuard {
    fn drop(&mut self) {
        let Some((state, user_id, request_id, incarnation_id)) = self.cleanup.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        std::mem::drop(runtime.spawn(async move {
            settle_media_session_request_claim(&state, user_id, &request_id, &incarnation_id).await;
        }));
    }
}

struct StartedSessionCleanup {
    state: AppState,
    owner_node_id: String,
    incarnation_id: String,
    session_id: String,
    user_id: i64,
    request_id: String,
    owns_worker: bool,
    owns_request_claim: bool,
    _replacement: Option<ClusterReplacementGuard>,
    #[cfg(test)]
    test_settlement: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    )>,
}

impl StartedSessionGuard {
    pub(in crate::http) fn new(
        state: AppState,
        owner_node_id: String,
        incarnation_id: String,
        session_id: String,
        user_id: i64,
        request_id: String,
        replacement: Option<ClusterReplacementGuard>,
    ) -> Self {
        Self::with_ownership(
            state,
            owner_node_id,
            incarnation_id,
            session_id,
            user_id,
            request_id,
            replacement,
            true,
            true,
        )
    }

    pub(in crate::http) fn recovered(
        state: AppState,
        owner_node_id: String,
        incarnation_id: String,
        session_id: String,
        user_id: i64,
        request_id: String,
        replacement: Option<ClusterReplacementGuard>,
    ) -> Self {
        Self::with_ownership(
            state,
            owner_node_id,
            incarnation_id,
            session_id,
            user_id,
            request_id,
            replacement,
            false,
            true,
        )
    }

    /// Observe an idempotently replayed worker without acquiring cleanup
    /// ownership of either the worker or its original durable request claim.
    pub(in crate::http) fn replayed(
        state: AppState,
        owner_node_id: String,
        incarnation_id: String,
        session_id: String,
        user_id: i64,
        request_id: String,
        replacement: Option<ClusterReplacementGuard>,
    ) -> Self {
        Self::with_ownership(
            state,
            owner_node_id,
            incarnation_id,
            session_id,
            user_id,
            request_id,
            replacement,
            false,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::http) fn worker_only(
        state: AppState,
        owner_node_id: String,
        incarnation_id: String,
        session_id: String,
        user_id: i64,
        request_id: String,
        replacement: Option<ClusterReplacementGuard>,
    ) -> Self {
        Self::with_ownership(
            state,
            owner_node_id,
            incarnation_id,
            session_id,
            user_id,
            request_id,
            replacement,
            true,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn claim_only(
        state: AppState,
        owner_node_id: String,
        incarnation_id: String,
        session_id: String,
        user_id: i64,
        request_id: String,
    ) -> Self {
        Self::with_ownership(
            state,
            owner_node_id,
            incarnation_id,
            session_id,
            user_id,
            request_id,
            None,
            false,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_ownership(
        state: AppState,
        owner_node_id: String,
        incarnation_id: String,
        session_id: String,
        user_id: i64,
        request_id: String,
        replacement: Option<ClusterReplacementGuard>,
        owns_worker: bool,
        owns_request_claim: bool,
    ) -> Self {
        Self {
            cleanup: Some(StartedSessionCleanup {
                state,
                owner_node_id,
                incarnation_id,
                session_id,
                user_id,
                request_id,
                owns_worker,
                owns_request_claim,
                _replacement: replacement,
                #[cfg(test)]
                test_settlement: None,
            }),
        }
    }

    pub(in crate::http) fn disarm(&mut self) {
        self.cleanup = None;
    }

    #[cfg(test)]
    pub(super) fn hold_cleanup_for_test(
        &mut self,
        settled: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
        released: tokio::sync::oneshot::Sender<()>,
    ) {
        self.cleanup
            .as_mut()
            .expect("armed guard cleanup")
            .test_settlement = Some((settled, release, released));
    }
}

impl crate::transcode::SessionAdoptionOwner for StartedSessionGuard {
    fn adopted_session_id(&mut self, durable_session_id: &str) {
        if let Some(cleanup) = self.cleanup.as_mut() {
            cleanup.session_id = durable_session_id.to_owned();
        }
    }
}

impl Drop for StartedSessionGuard {
    fn drop(&mut self) {
        let Some(cleanup) = self.cleanup.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let StartedSessionCleanup {
            state,
            owner_node_id,
            incarnation_id,
            session_id,
            user_id,
            request_id,
            owns_worker,
            owns_request_claim,
            _replacement,
            #[cfg(test)]
            test_settlement,
        } = cleanup;
        // Declared before the spawn, not inside it: from here the guard is
        // held by cleanup, its request has already answered the viewer, and a
        // later open for this player must not queue behind it. Publishing the
        // session first is what lets that open fence this teardown exactly.
        if let Some(replacement) = _replacement.as_ref() {
            replacement.publish_fenceable(&session_id);
            replacement.mark_abandoned();
        }
        std::mem::drop(runtime.spawn(async move {
            if owns_worker {
                abort_started_session(&state, &owner_node_id, &incarnation_id, &session_id).await;
            }
            if owns_request_claim {
                settle_media_session_request_claim(&state, user_id, &request_id, &incarnation_id)
                    .await;
            }
            #[cfg(test)]
            if let Some((settled, release, released)) = test_settlement {
                let _ = settled.send(());
                let _ = release.await;
                drop(_replacement);
                let _ = released.send(());
                return;
            }
            drop(_replacement);
        }));
    }
}

/// Owns the gap between durable activation confirmation and publication of
/// the corresponding create response. Confirmation deliberately leaves the
/// request claim in `starting`; this guard races final response publication
/// with atomic abandonment, so a healthy retry can never recover a route whose
/// worker a delayed cleanup is about to stop.
pub(super) struct ActivationPublicationGuard {
    cleanup: Option<(AppState, MediaSessionActivation)>,
}

impl ActivationPublicationGuard {
    pub(super) fn new(state: AppState, activation: MediaSessionActivation) -> Self {
        Self {
            cleanup: Some((state, activation)),
        }
    }

    pub(super) fn disarm(&mut self) {
        self.cleanup = None;
    }
}

impl Drop for ActivationPublicationGuard {
    fn drop(&mut self) {
        let Some((state, activation)) = self.cleanup.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        std::mem::drop(runtime.spawn(async move {
            settle_activation_publication_cleanup(state, activation).await;
        }));
    }
}
