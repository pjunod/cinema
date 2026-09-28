use super::*;

/// A live session, as the activity page sees it.
#[derive(Clone, serde::Serialize)]
pub struct SessionInfo {
    pub id: String,
    /// `vod` for the immutable presentation; `live-recovery` for the
    /// temporary growing-HLS compatibility engine.
    pub presentation: &'static str,
    pub file_id: i64,
    pub item_id: i64,
    pub item_title: String,
    pub user_name: String,
    pub target_height: i64,
    pub encoder: &'static str,
    /// Present only when this session's delivered bytes use the CPU zscale
    /// chain. `default` is the documented policy assumption, not source truth.
    pub tone_map_peak_nits: Option<u32>,
    pub tone_map_peak_source: Option<&'static str>,
    pub started_unix: i64,
    pub idle_seconds: u64,
    /// Last capability-authenticated resource this viewer requested. A stalled
    /// client that keeps polling playlists is different from one that stopped
    /// making requests altogether, even when both have the same idle age.
    pub last_request: &'static str,
    /// Which liveness contract owns this rolling generation. `explicit`
    /// begins only after a newly accepted control sequence carries a complete
    /// demand snapshot; media-only viewers remain visible during rollout.
    pub lease_mode: &'static str,
    /// Actor lifecycle verdict. Expiry may be visible briefly while the repair
    /// pass performs process and scratch teardown; serving is already fenced.
    pub lease_state: &'static str,
    /// The timeout currently enforced by the actor. Legacy viewers retain the
    /// rollout-compatible lifetime; explicit viewers receive the shorter
    /// deadline renewed by control or authenticated media consumption.
    pub lease_timeout_ms: Option<u32>,
    /// Presentation admission is actor-owned and finite. A positive remaining
    /// value is time left for accepted Rendering progress, not lease time.
    pub startup_state: Option<&'static str>,
    pub startup_remaining_ms: Option<i64>,
    pub presentation_progress_seen: Option<bool>,
    /// Produced inventory and the immutable served snapshot are distinct.
    /// These coordinates expose that separation without making clients infer
    /// it from playlist fetch timing.
    pub produced_end_ms: Option<i64>,
    pub served_end_ms: Option<i64>,
    /// Cumulative publication-budget diagnostics. The sequence is absent in
    /// legacy bootstrap mode; every endpoint is attempt-relative.
    pub budget_anchor_sequence: Option<u64>,
    pub allowed_end_ms: Option<i64>,
    pub carried_surplus_ms: Option<i64>,
    pub demand_observation_age_ms: Option<i64>,
    pub staged_bytes: i64,
    pub playlist_target_ms: Option<i64>,
    pub served_revision: Option<u64>,
    pub last_segment_advanced_idle_ms: Option<i64>,
    pub next_publication_in_ms: Option<i64>,
    pub publication_deadline_remaining_ms: Option<i64>,
    /// Maintenance and rate provenance are labels over measured state, not
    /// feature gates or policy inputs.
    pub maintenance_state: &'static str,
    pub rate_estimate_source: &'static str,
    pub estimate_active_speed: Option<f64>,
    pub pause_grace_remaining_ms: Option<i64>,
    pub retirement_reason: Option<&'static str>,
    /// Object-lifetime accounting. Advertised and Grace bytes are both
    /// included in live bytes until physical deletion succeeds.
    pub advertised_bytes: i64,
    pub grace_bytes: i64,
    pub reserved_bytes: Option<i64>,
    pub live_bytes: i64,
    /// Last accepted viewer intent and render facts. These are observations,
    /// not inferred recovery decisions.
    pub control_demand: Option<&'static str>,
    pub reported_position_ms: Option<i64>,
    pub client_runway_ms: Option<i64>,
    pub render_state: Option<&'static str>,
    /// Contiguous, complete, retained media at the latest accepted absolute
    /// film-time anchor. `missing` carries a measured zero; `unavailable`
    /// deliberately carries no seconds.
    pub server_ready_state: &'static str,
    pub server_ready_anchor_ms: Option<i64>,
    pub server_ready_end_ms: Option<i64>,
    pub server_ready_seconds: Option<f64>,
    /// A retained interval after an unavailable anchor. Debug consumers may
    /// show it, but it is never runway at the playhead.
    pub server_next_ready_start_ms: Option<i64>,
    pub server_next_ready_end_ms: Option<i64>,
    /// The pacing authority and its measured high-water coordinates. The
    /// legacy policy is download-frontier inference; explicit policy is
    /// actor-owned demand plus reported playhead/runway.
    pub production_policy: &'static str,
    pub production_ahead_seconds: Option<i64>,
    pub production_target_seconds: Option<i64>,
    /// Bounded actor-owned producer control truth. `None` means the rolling
    /// actor was unavailable or this is immutable VOD, rather than inferring
    /// control state from compatibility fields.
    pub producer_control: Option<crate::playback_control::RollingProducerOperationalSnapshot>,
    /// Compatibility-derived producer summary retained for existing Activity
    /// consumers. `producer_control` is the actor-owned source for deadline
    /// and ingress truth; this field remains non-authoritative until the M4
    /// decision/executor cutover removes the remaining compatibility inputs.
    pub producer_state: &'static str,
    /// Actor-owned rolling attempt and delivery coordinates. Immutable VOD is
    /// `None`; an unavailable rolling actor reports the once-sampled fallback
    /// attempt so exact terminal facts remain attributable. Attempt zero is a
    /// valid compatibility cache generation with no child process.
    pub producer_attempt: Option<u64>,
    pub playlist_ready: Option<bool>,
    pub published_segment: Option<i64>,
    pub next_media_sequence: Option<i64>,
    pub pending_fetched_segment: Option<i64>,
    /// Cumulative encode rate as a multiple of realtime, as ffmpeg reports it.
    pub speed: Option<f64>,
    /// Rate over the last few seconds. This is the one that answers "is the
    /// server keeping up *now*" — the cumulative figure hides a slowdown
    /// behind a fast start, which is exactly the shape a session takes when
    /// its burst has ended and the encoder can't hold realtime.
    pub recent_speed: Option<f64>,
    /// Content produced so far, in ms from this session's start offset.
    pub out_time_ms: Option<i64>,
    /// Wall time since ffmpeg's output timestamp last advanced. Unlike
    /// `recent_speed`, this remains decisive when the producer has stopped
    /// emitting samples entirely. `-1` is explicit unknown when neither an
    /// actor snapshot nor an exact-attempt compatibility projection exists.
    pub progress_idle_ms: i64,
    /// Exact process terminal facts for the actor attempt above. `code` is
    /// present for ordinary exits; `signal` is present for Unix signal exits.
    /// Success is kept separately because platforms may report neither code
    /// nor signal for a terminal status.
    pub producer_exit_success: Option<bool>,
    pub producer_exit_code: Option<i32>,
    pub producer_exit_signal: Option<i32>,
    pub producer_exit_idle_ms: Option<i64>,
    /// End of the newest complete, fetchable segment on the session timeline.
    pub published_end_ms: Option<i64>,
    /// End of the highest segment the client has requested.
    pub fetched_end_ms: i64,
    /// Highest segment ordinal requested by the client.
    pub fetched_segment: Option<i64>,
    /// First segment still present after retention pruning.
    pub first_retained_segment: Option<i64>,
    /// `sliding` for every rolling session from its first response, and `vod`
    /// for a completed cache hit. Retention never changes this contract.
    pub playlist_shape: &'static str,
    /// Published media beyond the client's download frontier — the reserve a
    /// hiccup gets to spend. Not measured from the playhead: the client has
    /// usually fetched further than it is showing.
    pub ahead_seconds: Option<i64>,
    /// The active limit keeping this session held. Capacity reasons take
    /// precedence when more than one limit is above its release point.
    pub hold_reason: Option<AheadHoldReason>,
    /// Present only for a time-held session.
    pub resume_below_seconds: Option<i64>,
    /// Present only for a per-session or global byte hold. For `global`, this
    /// is the release point for total live scratch across every session.
    pub resume_below_bytes: Option<i64>,
    /// The same reserve in bytes, which is what actually bounds the disk.
    pub ahead_bytes: Option<i64>,
    /// Segment bytes handed to this client since the session opened.
    pub delivered_bytes: i64,
    /// Recent delivery rate in BITS per second, or `None` before a window has
    /// closed. Measured here rather than in the player because only the server
    /// sees every transport — Safari's native HLS reports no such number, and
    /// hls.js's estimator exists only when hls.js is doing the fetching.
    pub delivered_bps: Option<i64>,
    /// How long since that rate was last recomputed. A client with a full
    /// buffer stops fetching, which is health, not a slow link — the rate keeps
    /// its last real value and this says how old it is, so a reader can tell a
    /// measurement from a memory.
    pub delivered_idle_ms: i64,
    /// Media requests parked waiting for publication right now: rolling
    /// sessions measure it in their [`HttpWaitLedger`], VOD from its bounded
    /// wait pool. The diagnostics-only VOD delivery listing does not carry the
    /// pool and reports zero.
    pub http_wait_count: usize,
    pub http_wait_oldest_ms: Option<i64>,
    pub http_wait_segment: Option<i64>,
    /// Server generation time. Clients also retain receipt time so wire age
    /// and local display age remain different observations.
    pub status_generated_unix_ms: i64,
    /// Effective ffmpeg input pace, matching `StreamInfo.readrate`.
    pub readrate: f64,
    pub suspended: bool,
    /// Number of times this producer entered a held state. This stays visible
    /// after resume so a flapping session cannot look healthy merely because
    /// the activity poll landed between transitions.
    pub suspend_count: u64,
}

/// The two honest shapes returned by `/hls/{session}/status`. Kept untagged so
/// existing live clients see byte-for-byte the object they already consume;
/// the VOD arm adds only the fields that presentation can actually measure.
#[derive(Clone, serde::Serialize)]
#[serde(untagged)]
pub enum HlsSessionInfo {
    Live(Box<SessionInfo>),
    Vod(Box<crate::vodserve::VodSessionInfo>),
}

/// Monotone failover coordinates sampled for the owner's two-second liveness
/// batch. This intentionally excludes activity-page locks and byte accounting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionFrontier {
    pub produced_playable_through_ms: i64,
    pub fetched_through_ms: i64,
    pub media_sequence: i64,
}

/// Coordinates selected before an expired incarnation is reproduced locally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionTakeoverStart {
    /// Process-local capability chosen by the settlement supervisor before
    /// creation. Knowing it in advance lets that supervisor reap an exact
    /// worker even if the creation task panics after manager registration.
    pub provisional_session_id: String,
    /// The durable incarnation this generation continues. Teardown keys off
    /// the incarnation rather than the process-local request record, because
    /// a successor is not created by any request on this node.
    pub incarnation_id: String,
    /// The replicated row's `media_origin_ms`: the fixed zero every
    /// generation's frontier is measured from. The offset a session finally
    /// records is the difference between the origin it *achieved* and this
    /// base, so a copy session's keyframe pull-back cannot over-report
    /// progress and strand media no generation ever produces.
    pub origin_base_ms: i64,
    /// Where this generation was asked to resume, relative to `origin_base_ms`.
    pub frontier_offset_ms: i64,
    pub media_sequence: i64,
    pub discontinuity_sequence: i64,
    pub owner_epoch: i64,
}

#[derive(Debug)]
pub(super) enum CopyInitValidationError {
    StateChanged,
    Invalid(String),
}

pub(super) fn copy_init_attempt_is_current(session: &Session, producer_attempt: u64) -> bool {
    session.actor_prepublication_producer.load(Acquire)
        && !session.replacing_child.load(Acquire)
        && !session.control.is_retired()
        && session.control.current_producer_attempt() == producer_attempt
}

async fn read_bounded_copy_object(
    session: &Session,
    producer_attempt: u64,
    name: &str,
    deadline: tokio::time::Instant,
) -> Result<Vec<u8>, CopyInitValidationError> {
    let path = session.dir.join(name);
    let metadata = loop {
        if !copy_init_attempt_is_current(session, producer_attempt) {
            return Err(CopyInitValidationError::StateChanged);
        }
        match tokio::time::timeout_at(deadline, tokio::fs::metadata(&path)).await {
            Err(_) => return Err(CopyInitValidationError::StateChanged),
            Ok(Ok(metadata)) => break metadata,
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                if tokio::time::timeout_at(deadline, tokio::time::sleep(Duration::from_millis(10)))
                    .await
                    .is_err()
                {
                    return Err(CopyInitValidationError::StateChanged);
                }
            }
            Ok(Err(error)) => {
                return Err(CopyInitValidationError::Invalid(format!(
                    "could not inspect emitted object {name}: {error}"
                )));
            }
        }
    };
    if !metadata.file_type().is_file()
        || metadata.len() > plurx_core::transcode::manifest::MAX_OBJECT_BYTES
    {
        return Err(CopyInitValidationError::Invalid(format!(
            "emitted object {name} is not a bounded regular file"
        )));
    }
    let bytes = tokio::time::timeout_at(deadline, tokio::fs::read(&path))
        .await
        .map_err(|_| CopyInitValidationError::StateChanged)?
        .map_err(|error| {
            CopyInitValidationError::Invalid(format!(
                "could not read emitted object {name}: {error}"
            ))
        })?;
    if bytes.len() as u64 != metadata.len()
        || bytes.len() as u64 > plurx_core::transcode::manifest::MAX_OBJECT_BYTES
    {
        return Err(CopyInitValidationError::Invalid(format!(
            "emitted object {name} changed during validation"
        )));
    }
    if !copy_init_attempt_is_current(session, producer_attempt) {
        return Err(CopyInitValidationError::StateChanged);
    }
    Ok(bytes)
}

/// Validate the copy output immediately before the actor's first-media handoff.
/// FFmpeg creates init.mp4 empty and writes it in place even with `temp_file`.
/// The first completed segment is renamed only after the init is finished, so
/// wait for that publication before reading the decoder configuration.
pub(super) async fn validate_copy_init_before_publication(
    session: &Session,
    producer_attempt: u64,
    deadline: Instant,
) -> Result<plurx_core::fmp4::HevcSampleEntryLayout, CopyInitValidationError> {
    if !copy_init_attempt_is_current(session, producer_attempt) {
        return Err(CopyInitValidationError::StateChanged);
    }
    let init_name = init_object_name(
        session
            .takeover
            .as_ref()
            .map(|takeover| takeover.owner_epoch),
    );
    let deadline = tokio::time::Instant::from_std(deadline);
    let first_index = session
        .takeover
        .as_ref()
        .map_or(0, |takeover| takeover.media_sequence);
    let first_index = u64::try_from(first_index)
        .map_err(|_| CopyInitValidationError::Invalid("negative first media sequence".into()))?;
    let segment_name = plurx_core::fmp4::segment_name(first_index);
    let segment =
        read_bounded_copy_object(session, producer_attempt, &segment_name, deadline).await?;
    let bytes = read_bounded_copy_object(session, producer_attempt, &init_name, deadline).await?;

    let mut reader = plurx_core::fmp4::FragmentReader::new();
    reader.push(&bytes);
    let init = match reader.next_unit().map_err(|error| {
        CopyInitValidationError::Invalid(format!("emitted init {init_name} is malformed: {error}"))
    })? {
        Some(plurx_core::fmp4::Unit::Init(init)) => init,
        Some(_) | None => {
            return Err(CopyInitValidationError::Invalid(format!(
                "emitted init {init_name} does not contain a complete initialization segment"
            )));
        }
    };
    let layout = plurx_core::fmp4::validate_hevc_sample_entries(&init).map_err(|error| {
        CopyInitValidationError::Invalid(format!(
            "emitted init {init_name} has an unusable decoder configuration: {error}"
        ))
    })?;
    let expects_hevc = session
        .frozen_presentation
        .as_ref()
        .and_then(|presentation| presentation.file.video_codec.as_deref())
        == Some("hevc");
    if expects_hevc && layout == plurx_core::fmp4::HevcSampleEntryLayout::NotHevc {
        return Err(CopyInitValidationError::Invalid(
            "emitted init omitted the frozen HEVC video track".into(),
        ));
    }
    if expects_hevc {
        reader.push(&segment);
        let first = match reader.next_unit().map_err(|error| {
            CopyInitValidationError::Invalid(format!(
                "first media segment {segment_name} is malformed: {error}"
            ))
        })? {
            Some(plurx_core::fmp4::Unit::Fragment(fragment)) => fragment,
            Some(_) | None => {
                return Err(CopyInitValidationError::Invalid(format!(
                    "first media segment {segment_name} has no complete fragment"
                )));
            }
        };
        plurx_core::fmp4::validate_hevc_sample_description_reference(&init, &first).map_err(
            |error| {
                CopyInitValidationError::Invalid(format!(
                    "first media segment {segment_name} has an unusable decoder reference: {error}"
                ))
            },
        )?;
    }
    Ok(layout)
}

/// The fMP4 init object a given ownership generation publishes.
///
/// Epoch 1 — every session that has never been taken over — keeps the
/// historical `init.mp4`, so no existing URL changes meaning. A fenced
/// successor names its own object: §7.3 forbids overwriting an init a client
/// may already have cached, and requires every URI in a newly served playlist
/// to stay retrievable once the old disk is gone.
pub(crate) fn init_object_name(owner_epoch: Option<i64>) -> String {
    match owner_epoch {
        Some(epoch) if epoch > 1 => format!("init-e{epoch}.mp4"),
        _ => "init.mp4".to_owned(),
    }
}

/// True for the init object of any generation. Every filename allowlist,
/// codec probe, and Apple tier rewrite compares through this, so a
/// generation-specific init is never mistaken for an arbitrary file.
pub(crate) fn is_init_object(name: &str) -> bool {
    if name == "init.mp4" {
        return true;
    }
    name.strip_prefix("init-e")
        .and_then(|rest| rest.strip_suffix(".mp4"))
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Cheap identity used to choose the globally newest activity before walking
/// any session telemetry locks.
#[derive(Clone, Debug)]
pub struct DeliveryCandidate {
    pub id: String,
    pub started_unix: i64,
}

/// The durable identity a session reserves its recovery budget against.
///
/// Three values that travel together because they are one thing: the ledger
/// `media_session_producer_recovery` is keyed by `(user_id, playback_id,
/// recovery_epoch)`, and a reservation additionally names the incarnation that
/// failed. `playback_id` is already on [`SessionRequest`] because a client
/// supplies it; these three are not, and deliberately.
///
/// **Not on [`SessionRequest`].** That type is `deny_unknown_fields` and it
/// crosses the cluster relay, so a field added to it is refused outright by a
/// node that has not been upgraded — a fleet rollout, taken on behalf of a
/// value the client never sends. Its own doc says what it carries: what a
/// client asked for. The recovery epoch is server-minted and is never
/// client-supplied, which is the entire property that makes it a budget rather
/// than a suggestion.
///
/// The epoch may be empty, for a session that predates the column. An empty
/// epoch is refused by the store's own validator, so it means *this playback
/// has no budget* rather than *this playback has an unused one* — a caller
/// must treat it as "no reservation is possible" and not as a fresh grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRecoveryIdentity {
    pub user_id: i64,
    /// The coordination identity of the generation being started, which is
    /// what a reservation records as the attempt that failed.
    pub incarnation_id: String,
    /// The server-owned budget identity. Empty means no budget.
    pub recovery_epoch: String,
}

/// What a client asked for, normalised. Two requests with the same
/// fingerprint would produce byte-identical output, which is what makes a
/// repeated create safe to answer with the session that already exists.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRequest {
    pub file_id: i64,
    /// Stable for one player instance; the supersession key.
    pub playback_id: String,
    /// Optional idempotency key for one creation attempt.
    pub request_id: Option<String>,
    /// The control exchange this start was decided from, when the client has
    /// one. Orders this ask against the destination the client has since
    /// settled on; `None` means there is no earlier exchange to be stale
    /// against, so the work is done.
    pub control_sequence: Option<u64>,
    /// True when the VIEWER chose Auto and left the rung to server policy.
    /// The resolved numeric height alone cannot distinguish Auto from a
    /// viewer's sticky manual pick, and neither can the presence of `height`
    /// on the wire — a subtitle burn sends the source height as a promise
    /// about the output, not as a quality answer. `CreateSession::quality_auto`
    /// carries the real intent; wire presence is only the fallback.
    pub automatic: bool,
    /// The exact predecessor a recovery is bound to. Present only with a
    /// typed [`ReopenReason`].
    pub previous_session_id: Option<String>,
    pub reopen_reason: Option<ReopenReason>,
    pub kind: SessionKind,
    pub start_seconds: f64,
    pub audio_index: Option<i64>,
    /// Subtitle stream to burn into the picture, chosen by the viewer.
    ///
    /// Only ever a *burn*: a text subtitle the client can render itself never
    /// comes through here — it fetches the VTT and shows it locally, which
    /// costs the server nothing and can be toggled without restarting a
    /// stream. This is for the ones that have no other way in, above all the
    /// PGS tracks a UHD Blu-ray remux carries and which plurx simply had no
    /// answer for before.
    pub subtitle_burn: Option<i64>,
    /// Manual A/V correction for this playback only (positive delays audio).
    pub audio_offset_ms: i64,
    /// The client asked for the HDR10 re-encode grade rather than the SDR
    /// ladder: it decodes and presents HEVC Main10 PQ (`hdr10t=1` at
    /// `/decision`, `CreateSession::hdr10` here).
    ///
    /// A request, not a verdict. The server still refuses it for any source
    /// that does not *prove* a Dolby Vision RPU through the production
    /// renderer — see `TranscodeManager::hdr10_grade_for`. A client that
    /// asks for it on an HDR10 source gets today's tone-mapped SDR rung,
    /// because the passthrough filter on a non-DV input emits a broken
    /// picture at exit 0 rather than failing (measured).
    pub hdr10: bool,
    /// Which presentation this durable request names. `Live` remains the
    /// serde default solely so recipes written by an older binary retain
    /// their identity and can be refused rather than misread as VOD. Public
    /// session ingress maps an omitted field to `Vod`; no new live request is
    /// admitted.
    #[serde(default, skip_serializing_if = "Presentation::is_live")]
    pub presentation: Presentation,
    /// Client's ceiling for one blocking segment GET, in seconds. Clamped to
    /// the server's own cap; `None` takes the server default. VOD only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_budget_secs: Option<f64>,
    /// How this client will play and tear down the stream — `"hlsjs"`,
    /// `"native"`, or absent.
    ///
    /// Retention-relevant request semantics, carried in the durable recipe so
    /// a remote owner, a replay and an owner takeover all read the same class
    /// as the create did. Deliberately **not** part of the intent
    /// fingerprint: an added field there would change every existing
    /// fingerprint and make a mixed-version cluster disagree about request
    /// identity, and the durable recipe already answers "what class is this
    /// exact session" without that cost.
    ///
    /// Absent, unknown, or anything but the audited hls.js contract reads as
    /// [`ReleaseClass::Conservative`]. Omitted when absent so an old node's
    /// recipe and a new node's recipe for the same request are byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
}

/// How long a retired presentation is kept readable after the exact viewer
/// releases it.
///
/// This is a *release* class, not a capability label. It says what is known
/// about one client's teardown ordering and retry tail, and nothing else. A
/// class is admitted to the short window only when the client's own release
/// site has been audited to destroy its player before it sends the DELETE.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseClass {
    /// Nothing is known, or what is known does not bound the retry tail:
    /// native HLS, AirPlay, Apple, a legacy client that sends no transport,
    /// an unrecognized value, or a session whose transport could still fall
    /// back. Original grace.
    #[default]
    Conservative,
    /// hls.js on the web player. `retirePlaybackPredecessor` destroys the
    /// instance before `releaseSession` sends the DELETE
    /// (`decode-margin.js:399-408`), and a destroyed instance issues no
    /// further requests, so its promise can end one segment target after the
    /// release rather than one whole advertised playlist later.
    HlsJs,
}

/// One session segment target. The allowance for a class whose teardown is
/// known: enough for a request already in flight when the DELETE was sent,
/// and no more. The 20 s server `SEGMENT_WAIT` is a server-side blocking
/// budget, not a client retry allowance, and is deliberately not used here.
pub(crate) const EXACT_RELEASE_ALLOWANCE: Duration =
    Duration::from_secs(plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS as u64);

impl ReleaseClass {
    /// Map the create-time transport metadata to a class.
    ///
    /// Everything that is not exactly the audited hls.js contract is
    /// conservative — an omitted field, an unknown string, a native
    /// transport. Missing information never shortens a promise.
    pub(crate) fn from_transport(transport: Option<&str>) -> Self {
        match transport {
            Some("hlsjs") => Self::HlsJs,
            _ => Self::Conservative,
        }
    }

    pub(super) fn allowance(self) -> Option<Duration> {
        match self {
            Self::Conservative => None,
            Self::HlsJs => Some(EXACT_RELEASE_ALLOWANCE),
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Conservative => "conservative",
            Self::HlsJs => "hlsjs",
        }
    }
}

/// True for a transport string this build is willing to store.
///
/// Bounded and lower-case-ascii so an arbitrary client string cannot become
/// a metric label or a log injection. An unrecognized but well-formed value
/// is accepted and read as [`ReleaseClass::Conservative`]; a malformed one is
/// refused at the door with every other invalid field.
pub(crate) fn session_transport_is_valid(transport: &str) -> bool {
    (1..=32).contains(&transport.len())
        && transport
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
}

/// The retired object promise, shared between the session and its cleanup
/// owner so an exact same-viewer release can shorten it — once.
pub(super) struct RetiredRelease {
    /// The promise the retirement computed, and the only value cleanup
    /// sleeps on. `None` until retirement sets it.
    serve_until: std::sync::Mutex<Option<Instant>>,
    /// The accepted release: when it arrived and what its class allows. Held
    /// for the case where the release lands before retirement finishes, so
    /// `prepare_retired_object_promise` can publish an already-short promise
    /// rather than racing the cleanup owner's sleep.
    released: std::sync::Mutex<Option<(Instant, Duration)>>,
    /// First writer wins. A duplicate DELETE cannot extend, renew or
    /// re-shorten the deadline.
    shortened: AtomicBool,
    /// Wakes the cleanup owner when the deadline moves in.
    pub(super) wake: tokio::sync::Notify,
}

impl RetiredRelease {
    pub(super) fn new() -> Self {
        Self {
            serve_until: std::sync::Mutex::new(None),
            released: std::sync::Mutex::new(None),
            shortened: AtomicBool::new(false),
            wake: tokio::sync::Notify::new(),
        }
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        *self
            .serve_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn publish(&self, serve_until: Instant) {
        *self
            .serve_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(serve_until);
    }

    /// Record an accepted release. Returns true only for the first one, so a
    /// duplicate DELETE can neither extend nor re-shorten the deadline.
    pub(super) fn accept_release(&self, at: Instant, allowance: Duration) -> bool {
        if self.shortened.swap(true, AcqRel) {
            return false;
        }
        *self
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((at, allowance));
        true
    }

    pub(super) fn released(&self) -> Option<(Instant, Duration)> {
        *self
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Pull the published deadline in, never out. Returns the new value when
    /// it actually moved.
    pub(super) fn shorten_to(&self, deadline: Instant) -> Option<Instant> {
        let mut serve_until = self
            .serve_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = (*serve_until)?;
        if deadline >= current {
            return None;
        }
        *serve_until = Some(deadline);
        drop(serve_until);
        self.wake.notify_waiters();
        Some(deadline)
    }
}

/// The two presentations a session can be created under (plan §2.7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presentation {
    /// Today's behaviour: an EVENT playlist that grows, prunes and rewrites.
    #[default]
    Live,
    /// The film-addressed immutable playlist served from a segment plan.
    Vod,
}

impl Presentation {
    /// Serialization skips the default so every legacy request and stored
    /// recipe stays byte-identical to what an older binary wrote — a rolling
    /// upgrade's old workers and a rollback's takeover parses both read
    /// `deny_unknown_fields` envelopes, and only a genuine VOD request should
    /// ever be new to them.
    fn is_live(&self) -> bool {
        *self == Presentation::Live
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionKind {
    Transcode {
        height: i64,
    },
    /// Copy the source video; `aac` re-encodes the audio the client can't take.
    Copy {
        aac: bool,
        preserve_dolby_vision: bool,
        /// Rewrite Profile 7 RPUs to Profile 8.1 on the way through.
        ///
        /// Beside `preserve_dolby_vision` rather than inside it because they
        /// answer different questions of the same copy: preserving decides
        /// whether the RPU NAL units survive the bitstream filter, converting
        /// decides whether they are rewritten in the fragments that copy
        /// writes. Only ever true when preserving is — there is nothing to
        /// rewrite in a stream the filter removed.
        convert_dolby_vision: bool,
    },
}

/// Why a client is replacing an existing session. This is deliberately typed
/// even while `stall` is the only server-normalized cause: an unknown future
/// value must be refused, not accidentally treated as ordinary create.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReopenReason {
    Stall,
}

impl ReopenReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stall => "stall",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CopySessionOptions {
    pub(super) transcode_audio: bool,
    pub(super) preserve_dolby_vision: bool,
    /// Rewrite Profile 7 RPUs to Profile 8.1 in the fragments the copy pipe
    /// writes. Only ever set beside `preserve_dolby_vision`.
    pub(super) convert_dolby_vision: bool,
}

#[derive(Clone, Copy)]
pub(super) struct SessionOwner<'a> {
    pub(super) user_name: &'a str,
    pub(super) supersession_user: &'a str,
    pub(super) playback_id: &'a str,
    pub(super) automatic: bool,
}

impl SessionRequest {
    /// The client's request intent before server-owned Auto normalization.
    /// This is the only idempotency identity: it is what `claim_request`
    /// compares when a `request_id` is replayed, so every field that changes
    /// the output bytes has to be in here. Start position is included — two
    /// creates at different offsets are different streams, and answering the
    /// second with the first would silently seek the viewer somewhere they
    /// didn't ask to be.
    ///
    /// For an Auto transcode the numeric height is excluded on purpose: a
    /// network-prior refresh between transport attempts may recompute it, but
    /// the same `request_id` must still recover the first persisted answer.
    fn intent_fingerprint_with_user_scope(&self, user_name: Option<&str>) -> String {
        let kind = match self.kind {
            SessionKind::Transcode { height: _ } if self.automatic => "ta".to_owned(),
            SessionKind::Transcode { height } => format!("t{height}"),
            SessionKind::Copy {
                aac,
                preserve_dolby_vision,
                convert_dolby_vision,
            } => {
                // A converted stream is different bytes, so it must not answer
                // a replay for an unconverted one. Appended rather than folded
                // into the existing digits so an unconverted request
                // fingerprints to exactly the string it always did — a replay
                // in flight across a deploy still recovers its session.
                let base = format!("c{}d{}", u8::from(aac), u8::from(preserve_dolby_vision));
                if convert_dolby_vision {
                    format!("{base}+p81")
                } else {
                    base
                }
            }
        };
        // A different grade is different bytes, so it cannot share an
        // idempotency identity. Appended rather than folded into every arm so
        // that an SDR request fingerprints to exactly the string it always
        // did — a replay in flight across a deploy still recovers its session.
        let kind = if self.hdr10 {
            format!("{kind}+hdr10")
        } else {
            kind
        };
        // Same append-only discipline: a VOD-presented session answers a
        // different playlist shape, so it cannot recover a live session's id
        // (or vice versa), while every legacy request keeps the exact
        // fingerprint it always had.
        let kind = if self.presentation == Presentation::Vod {
            format!("{kind}+vod")
        } else {
            kind
        };
        // These strings are client-controlled. A typed JSON tuple keeps a
        // colon inside a username, playback id, or session id from producing
        // the same fingerprint as a different set of fields.
        serde_json::json!([
            user_name,
            self.file_id,
            self.playback_id,
            u8::from(self.automatic),
            kind,
            format!("{:.3}", self.start_seconds),
            self.audio_index,
            self.subtitle_burn,
            self.audio_offset_ms,
            self.previous_session_id,
            self.reopen_reason.map(ReopenReason::as_str),
        ])
        .to_string()
    }

    pub(super) fn intent_fingerprint(&self, user_name: &str) -> String {
        self.intent_fingerprint_with_user_scope(Some(user_name))
    }

    /// Fixed-width durable identity used by the replicated session claim.
    ///
    /// Replicated request rows are already scoped by the immutable user id.
    /// Exclude the mutable username so an account rename cannot turn an
    /// otherwise identical idempotent replay into a conflict. The established
    /// process-local identity above keeps the username scope it has always had.
    pub(crate) fn durable_intent_fingerprint(&self, user_id: i64) -> String {
        let durable = serde_json::json!([user_id, self.intent_fingerprint_with_user_scope(None),]);
        hex::encode(Sha256::digest(durable.to_string().as_bytes()))
    }
}
