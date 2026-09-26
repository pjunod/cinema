use super::*;

/// Everything a client must say to open a stream.
///
/// A body rather than a query string, and a POST rather than a GET, because
/// this call spawns a process and kills its predecessor. A GET that does that
/// is a trap: GET is idempotent by definition, so anything entitled to replay
/// one — a retry, a prefetch, an intermediary — could spawn a second encoder
/// and orphan the first.
#[derive(Deserialize)]
pub struct CreateSession {
    /// Stable for one player instance. Supersession is keyed by it, so two
    /// devices on one account no longer kill each other's streams.
    pub playback_id: String,
    /// Optional idempotency key for this one attempt.
    pub request_id: Option<String>,
    /// The control exchange this restart was decided from, when the client has
    /// one. A seek storm asks for a session per destination; the sequence is
    /// what orders those asks against the destination the client has since
    /// settled on, so a restart the viewer has already scrolled past can be
    /// skipped rather than produced and thrown away.
    ///
    /// Absent means "do the work", which is what every client did before this
    /// field existed and what a first play still means -- there is no earlier
    /// exchange to be stale against.
    pub control_sequence: Option<u64>,
    /// Exact predecessor for a typed recovery. Both fields are required for a
    /// stall reopen, which also requires `request_id`; ordinary seeks and
    /// track changes omit them.
    pub previous_session_id: Option<String>,
    pub reopen_reason: Option<crate::transcode::ReopenReason>,
    /// Target height for a transcode. Ignored when `copy` is set. Omitted
    /// means Auto, and Auto is the SERVER's decision: the rung depends on
    /// which encoder wins, and the player only learns that from the response
    /// to this request (see `TranscodeManager::auto_height`).
    pub height: Option<i64>,
    /// Whether the VIEWER chose Auto quality. This is what decides stall
    /// stickiness, and it is not the same question as "did this body carry a
    /// `height`".
    ///
    /// A client sends a height for reasons unrelated to the quality menu: a
    /// subtitle burn and Quality = Original both send the source's own height
    /// as a promise about the output. Android's `sessionHeight` answers the
    /// burn case *before* it consults quality, so an Auto viewer watching with
    /// a burned subtitle posts a height and would otherwise be permanently
    /// ineligible for the stall step-down — while the same viewer on Apple,
    /// which never names a height, would be eligible. Same intent, opposite
    /// recovery, decided by a field neither of them meant as a quality answer.
    ///
    /// Omitted keeps the old inference (`height` absent means Auto), so every
    /// shipped client behaves exactly as before. A client that can state the
    /// viewer's choice should send it, and then it is authoritative.
    pub quality_auto: Option<bool>,
    /// Subtitle stream to burn into the picture. Only for the ones a client
    /// cannot render itself — a bitmap track (PGS/VobSub) has no text to send,
    /// so the only way to show it is to draw it into the frames.
    pub subtitle_burn: Option<i64>,
    /// Explicit acknowledgement that the already-selected playback plan
    /// delivers SDR. This distinguishes a forced bitmap subtitle added to an
    /// existing HDR-to-SDR transcode from a subtitle that would silently
    /// downgrade an otherwise-HDR delivery. Absent stays fail-closed for old
    /// clients.
    pub subtitle_burn_sdr: Option<bool>,
    /// Advertise the source's lossless WebVTT-convertible tracks in an HLS
    /// master playlist. Native Apple clients opt in; older HLS consumers keep
    /// receiving the media playlist shape they already understand.
    pub native_subtitles: Option<bool>,
    /// Initially selected native rendition. This changes only HLS metadata,
    /// never the video recipe or the encoder choice.
    pub subtitle: Option<i64>,
    pub start: Option<f64>,
    pub audio: Option<i64>,
    /// Copy the source video into HLS rather than re-encoding it.
    pub copy: Option<bool>,
    /// With `copy`: re-encode the audio the client can't take.
    pub aac: Option<bool>,
    /// With `copy`: retain Dolby Vision signaling and dynamic metadata because
    /// the decision established that this client supports the source profile.
    pub preserve_dolby_vision: Option<bool>,
    /// For a transcode: this client decodes and presents HEVC Main10 PQ, so a
    /// Dolby Vision source it cannot decode may be re-encoded to HDR10 rather
    /// than tone-mapped to SDR. The create-body counterpart of `/decision`'s
    /// `hdr10t=1`, exactly as `preserve_dolby_vision` is the counterpart of
    /// `dv`.
    ///
    /// A request, never a verdict. The server refuses it for any source that
    /// does not prove a Dolby Vision RPU through the production renderer, for
    /// any rung other than the measured 1080p software/QSV or 2160p QSV
    /// points, and for any build whose boot probe did not clear the exact
    /// graph — every refusal lands on today's SDR ladder. Absent means no,
    /// which is what every client that predates it sends.
    pub hdr10: Option<bool>,
    /// Manual A/V correction for this playback attempt only. Positive delays
    /// audio. It is carried into every seek/reopen by the client and is never
    /// written back to the media file.
    pub audio_offset_ms: Option<i64>,
    /// The same capabilities document the client posted to `/decision`
    /// (PLAYBACK-CAPS-V2-PLAN §4.5).
    ///
    /// When present, create re-derives the plan from it and the two fields
    /// above become *assertions* rather than instructions — a disagreement is
    /// logged and counted, and the server's plan is used. When absent, the
    /// echo is trusted exactly as it always was, so every shipped build keeps
    /// working unchanged; that path is counted too, so the fleet can be
    /// watched for stragglers.
    pub caps: Option<plurx_core::playback::DeviceCaps>,
    /// The named reasons this body may legitimately differ from the plan the
    /// server derives from `caps`.
    pub overrides: Option<CreateOverrides>,
    /// The only accepted value is `"vod"`. Omitted also means VOD so clients
    /// predating this field cannot accidentally enter the removed growing
    /// live-HLS path. An explicit legacy value receives a typed refusal.
    pub presentation: Option<String>,
    /// With `presentation:"vod"`: the client's ceiling for one blocking
    /// segment fetch, in seconds. Clamped server-side.
    pub block_budget_secs: Option<f64>,
    /// What the viewer is asking for, as an orderable ask rather than as the
    /// flat scalars above.
    ///
    /// The scalars stay, and they still drive the pipeline — this does not
    /// replace them and a client that sends neither behaves exactly as before.
    /// What it adds is the one thing they cannot express: a complete normalized
    /// selection that keeps Auto, Original and Manual apart, carrying a
    /// revision that says which of two asks is later.
    ///
    /// It is on *create* and not only on control because the control protocol
    /// can be switched off, and a session created that way has no control
    /// channel at all. In that configuration the create body is the only thing
    /// the server ever learns about what the viewer wants, so an ask that
    /// arrives only through control is, there, an ask that never arrives.
    pub intent: Option<plurx_core::playback::MediaIntentEnvelope>,
    /// How this client will play and tear down the stream.
    ///
    /// `"hlsjs"` names the one audited teardown contract: the web player
    /// destroys its hls.js instance before it sends the release, so its
    /// retired objects can be dropped a segment after the DELETE instead of
    /// a whole advertised playlist later. `"native"`, an unrecognized value
    /// and an absent field all keep the original promise.
    ///
    /// The client must send what it actually selected, not a guess from the
    /// user agent, and must choose the conservative class whenever its
    /// transport could still fall back within the session.
    pub transport: Option<String>,
}

/// Test-only seam that freezes one create after it has recorded its viewer's
/// ask and before its activation runs.
///
/// The property it exists to prove cannot be observed any other way. `create`
/// carries the revision it *recorded* into the activation rather than one read
/// at activation time, and those two values are identical except in the window
/// between them — so a test that cannot stop inside that window cannot tell the
/// correct implementation from the broken one. Modelled on the replicated
/// store's `ACTIVATION_POINTER_READ_PAUSE`, which exists for the same reason.
///
/// One waiter at a time, taken rather than cloned, so a test that forgets to
/// arm it cannot accidentally inherit another test's pause.
#[cfg(test)]
type CreateAskRecordedPause = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);

#[cfg(test)]
pub(super) static CREATE_ASK_RECORDED_PAUSE: std::sync::LazyLock<
    std::sync::Mutex<Option<CreateAskRecordedPause>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

impl CreateSession {
    /// `height` is initially resolved by the caller — Auto answered, explicit
    /// rungs snapped, the source-height promise honored. A bound stall reopen
    /// is the one later normalization: `claim_request` replaces this value
    /// from the predecessor's resolved rung and persists it before supersede.
    pub(super) fn into_request(
        self,
        file_id: i64,
        height: i64,
    ) -> crate::transcode::SessionRequest {
        use crate::transcode::SessionKind;
        // Viewer intent when the client states it; otherwise the old
        // wire-presence inference, which is right for every client that has
        // no separate quality answer to give.
        let automatic = self.quality_auto.unwrap_or(self.height.is_none());
        let kind = if self.copy == Some(true) {
            SessionKind::Copy {
                aac: self.aac == Some(true),
                preserve_dolby_vision: self.preserve_dolby_vision == Some(true),
                // Never taken from the body. A client cannot ask to be handed
                // a conversion — whether one happens is decided from its caps
                // and the node's, and create overwrites this from the plan it
                // re-derives (`review_client_plan`). A wire field here would
                // be a claim the client has no way to be right about.
                convert_dolby_vision: false,
            }
        } else {
            SessionKind::Transcode { height }
        };
        // Refuse a malformed value rather than storing it; an unrecognized
        // but well-formed one is kept and simply reads as conservative.
        let transport = self
            .transport
            .filter(|transport| crate::transcode::session_transport_is_valid(transport));
        crate::transcode::SessionRequest {
            file_id,
            playback_id: self.playback_id,
            request_id: self.request_id,
            control_sequence: self.control_sequence,
            automatic,
            previous_session_id: self.previous_session_id,
            reopen_reason: self.reopen_reason,
            kind,
            start_seconds: self.start.unwrap_or(0.0).max(0.0),
            audio_index: self.audio.filter(|a| *a >= 0),
            subtitle_burn: self.subtitle_burn.filter(|s| *s >= 0),
            audio_offset_ms: self.audio_offset_ms.unwrap_or(0).clamp(-15_000, 15_000),
            hdr10: self.hdr10 == Some(true),
            presentation: crate::transcode::Presentation::Vod,
            block_budget_secs: self.block_budget_secs.filter(|s| s.is_finite() && *s > 0.0),
            transport,
        }
    }
}

/// Process-lifetime counters for what create did with the plan it was handed.
///
/// The seam this closes (PLAYBACK-CAPS-V2-PLAN E4) was never a live bug — all
/// three shipped clients propagate `/decision`'s answer faithfully. It was a
/// *trust* relationship with no way to tell whether it still held. These four
/// numbers are that way: `legacy_trusted` falling to zero is how the fleet
/// proves every build has moved to caps v2, and `mismatched` rising is how a
/// client that started lying about itself gets noticed before a viewer files
/// a bug about a black screen.
pub mod plan_derivation {
    use std::sync::atomic::{AtomicU64, Ordering};

    static LEGACY_TRUSTED: AtomicU64 = AtomicU64::new(0);
    static UNUSABLE_CAPS: AtomicU64 = AtomicU64::new(0);
    static REDERIVED: AtomicU64 = AtomicU64::new(0);
    static MISMATCHED: AtomicU64 = AtomicU64::new(0);
    static OVERRIDDEN: AtomicU64 = AtomicU64::new(0);

    pub(in crate::http::hls) fn count_legacy_trusted() {
        LEGACY_TRUSTED.fetch_add(1, Ordering::Relaxed);
    }

    pub(in crate::http::hls) fn count_unusable_caps() {
        UNUSABLE_CAPS.fetch_add(1, Ordering::Relaxed);
    }

    pub(in crate::http::hls) fn count_rederived() {
        REDERIVED.fetch_add(1, Ordering::Relaxed);
    }

    pub(in crate::http::hls) fn count_mismatched() {
        MISMATCHED.fetch_add(1, Ordering::Relaxed);
    }

    pub(in crate::http::hls) fn count_overridden() {
        OVERRIDDEN.fetch_add(1, Ordering::Relaxed);
    }

    /// `(legacy_trusted, unusable_caps, rederived, mismatched, overridden)`,
    /// this process.
    ///
    /// `rederived` is loaded FIRST and incremented BEFORE its own outcome
    /// counters, so a concurrent create can only ever make the total look
    /// larger than its parts — never `mismatched > rederived`, which reads as
    /// a bug in the server rather than in a client.
    pub fn snapshot() -> (u64, u64, u64, u64, u64) {
        let rederived = REDERIVED.load(Ordering::Relaxed);
        (
            LEGACY_TRUSTED.load(Ordering::Relaxed),
            UNUSABLE_CAPS.load(Ordering::Relaxed),
            rederived,
            MISMATCHED.load(Ordering::Relaxed),
            OVERRIDDEN.load(Ordering::Relaxed),
        )
    }
}

/// The named reasons a create body is *allowed* to disagree with the plan the
/// server derives from the same capabilities.
///
/// Named, because the alternative is a client that can silently opt out of
/// the server's verdict by echoing whatever it likes — which is the seam this
/// milestone exists to close. Each override that fires appends its own reason
/// to the session's notes, so the badge shows why the delivery is not the one
/// the caps alone would have produced.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CreateOverrides {
    /// Apple's `forceCompatibleHDRBase` retry (`PlayerController.swift:2373`):
    /// the client decoded the Dolby Vision stream, failed, and is asking for
    /// the HDR10-compatible base instead. A legitimate decline of a plan the
    /// server derived correctly from capabilities that were themselves
    /// correct — the device really does claim the profile, and really did
    /// fail on this title.
    #[serde(default)]
    pub compatible_hdr_base: Option<bool>,
    /// The quality menu: `"auto" | "original" | "transcode"`. It is fed into
    /// the re-derivation itself rather than compared against it, so a forced
    /// rung produces the server's plan *for that force* — but it still leaves
    /// a note, because a viewer who forced a transcode and then reads an SDR
    /// badge deserves to see the two facts next to each other.
    #[serde(default)]
    pub force: Option<String>,
}

/// What create decided about the plan the client echoed, and what to say
/// about it.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PlanReview {
    /// The value create must use, whatever the body asked for.
    pub preserve_dolby_vision: bool,
    /// Whether the copy converts Profile 7 to 8.1 on the way through.
    ///
    /// Not clamped against anything the client asked for, because there is
    /// nothing to clamp: the conversion is not a wire field and a client has
    /// no way to be right or wrong about it. It follows
    /// `preserve_dolby_vision` instead — a client that declines Dolby Vision
    /// declines the converted kind too, and there is no such thing as
    /// converting RPUs a stripping filter has already removed.
    pub convert_dolby_vision: bool,
    /// Likewise for the HDR10 re-encode request. Still only a *request*:
    /// `TranscodeManager::hdr10_grade_for` refuses it for any source, rung, or
    /// build that did not prove the chain, and that refusal is unchanged.
    pub hdr10: bool,
    /// Reasons, in the order they were established. Empty means the client's
    /// echo and the server's derivation agreed and no override fired — the
    /// case that should be every case.
    pub notes: Vec<String>,
    /// A disagreement no override explained. Counted and logged; never a
    /// refusal (Paul, 2026-08-29: "I don't see a reason for it to prevent
    /// functionality").
    pub mismatched: bool,
}

/// Put the reconciled plan onto the request that will actually be built, and
/// hand back the notes that explain it.
///
/// Separate from `create` because this is the only place the server's own
/// derivation reaches the session, and every field it copies has a different
/// way of going wrong if it does not:
///
/// - `preserve_dolby_vision` reverts to the client's echo, which is the
///   pre-caps-v2 bug this milestone exists to close;
/// - `convert_dolby_vision` is never set at all — `into_request` leaves it
///   false, because a client has no way to ask for a conversion — so the whole
///   feature silently does nothing on every session;
/// - `hdr10` reverts to a claim the caps did not support.
pub(super) fn apply_plan_review(
    request: &mut crate::transcode::SessionRequest,
    review: PlanReview,
) -> Vec<String> {
    if let crate::transcode::SessionKind::Copy {
        preserve_dolby_vision,
        convert_dolby_vision,
        ..
    } = &mut request.kind
    {
        *preserve_dolby_vision = review.preserve_dolby_vision;
        // The only place this is ever set. It is derived, never echoed.
        *convert_dolby_vision = review.convert_dolby_vision;
    }
    request.hdr10 = review.hdr10;
    review.notes
}

/// The plan for a build that sends no caps document: its echo, plus the one
/// field its echo cannot carry.
///
/// Everything a client can state, it states, and this trusts it — that is what
/// the legacy path *is*, and there is no capability document here to re-derive
/// from. The conversion is the exception, because no client has ever been able
/// to ask for one: `CreateSession` has no such field, so the echo is silent
/// and [`crate::transcode::SessionKind::Copy`] keeps the `false` it was built
/// with. Silence then reads as "no", which quietly overrides the *server's*
/// own answer — `/decision` says "Profile 7 converted to Profile 8.1 for this
/// device" and the session a second later preserves raw Profile 7.
///
/// That is the one delivery nothing plays: a dual-layer stream no consumer
/// decoder outside Blu-ray hardware takes. Observed in production as Safari
/// answering `stream_rejected ... browser refused the remux stream`, followed
/// by a fallback that tonemapped the title to SDR.
///
/// Only the file half of [`plurx_core::playback::dolby_vision_converts_to_p81`]
/// is asked here. Its client half ("takes 8 but not 7") exists to keep an
/// enhancement layer for a client that *enumerated* Profile 7, and it only
/// ever narrows who converts — a build sending no caps document enumerated
/// nothing, so there is no such claim to honor. Handing it the layer is not
/// the more conservative answer; it is the unplayable one.
///
/// The conversion still follows the preservation, for the same reason
/// [`review_client_plan`] ends by clamping it: a client that declined Dolby
/// Vision must not be handed a converted stream by a flag nobody looked at.
///
/// **And the preservation is not trusted all the way either.** Everything
/// above is about the conversion, which the echo cannot carry. This is about
/// the one thing the echo *does* carry that must still not be taken at face
/// value.
///
/// `preserve_dolby_vision: true` means "send me the Dolby Vision", and for
/// every single-layer profile that is a claim the client is entitled to make.
/// For a dual-layer source it is not a claim the client ever made: it echoed
/// a `true` the *server* produced, and the server produced it expecting to
/// convert. When the conversion then does not happen — an operator turned it
/// off, or the row is label-only and
/// [`plurx_core::playback::file_can_convert_to_p81`] refuses it for want of
/// the columns to build a record from — trusting the echo delivers raw
/// dual-layer to a build that enumerated nothing.
///
/// So the echo is clamped by the same rule
/// `DeviceProfile::decodes_dolby_vision_as_copied` applies to a document that
/// *did* enumerate: dual-layer needs either a conversion or an enumeration,
/// and a build sending no readable capabilities has neither.
///
/// Not counted as a `plan_mismatch`. That counter measures a client claiming
/// more than its own document supports, and there is no document here to
/// exceed; the note carries the explanation instead.
pub(super) fn legacy_trusted_review(
    file: &MediaFile,
    node: &plurx_core::playback::RenderCaps,
    asked_preserve_dolby_vision: bool,
    asked_hdr10: bool,
) -> PlanReview {
    let convert_dolby_vision = asked_preserve_dolby_vision
        && node.dolby_vision_convert
        && plurx_core::playback::file_can_convert_to_p81(file);
    let dual_layer_stays_dual_layer =
        !convert_dolby_vision && plurx_core::playback::dolby_vision_is_dual_layer(file);
    let preserve_dolby_vision = asked_preserve_dolby_vision && !dual_layer_stays_dual_layer;
    let mut notes = Vec::new();
    if asked_preserve_dolby_vision && !preserve_dolby_vision {
        // Says what was *decided*, not what will be delivered. Whether the
        // base layer survives as HDR10, as HLG, or not at all is
        // `dv_handling`'s answer a layer down, and a note that guessed it
        // would be wrong for a Profile 7 over an SDR base.
        notes.push(
            "Dolby Vision declined: this build sent no capability document, and its \
             dual-layer stream is not one this copy can convert or any client that did \
             not name the profile can decode"
                .to_owned(),
        );
    }
    PlanReview {
        preserve_dolby_vision,
        convert_dolby_vision,
        hdr10: asked_hdr10,
        notes,
        mismatched: false,
    }
}

/// Re-derive the plan from the capabilities the client sent, and reconcile it
/// with what the client asked for.
///
/// **The server's plan is a ceiling, not a floor.** A client may always ask
/// for *less* than the derivation allows — that is a client declining
/// something it is entitled to decline, and the reason it must stay allowed
/// is Apple's `forceCompatibleHDRBase` retry (`PlayerController.swift:2373`):
/// the device really does take Profile 8, so the derivation really does say
/// "preserve", and forcing that answer back onto a retry hands the client the
/// exact stream it just failed on, forever. What the client may *not* do is
/// claim more than its own document supports — and that direction is the
/// whole of E4, because it is the one that ends in a black screen.
///
/// So a downward echo is honoured silently (a named override still says why,
/// when there is one), and an upward echo is clamped, logged, counted and
/// reported. No shipped build is affected either way: a client that sends no
/// `caps` never reaches this function.
///
/// The two fields are different *kinds* of claim and are reconciled
/// differently:
///
/// - `preserve_dolby_vision` is a **per-title** verdict. The derivation
///   answers it directly.
/// - `hdr10` is a **per-title request** for the HDR10 re-encode rung, and the
///   caps document only says whether the client could present one at all. So
///   the document is a *permission*, not an answer: the request stands when
///   the document backs it, and is refused when it does not. Deriving it from
///   the document alone would set `hdr10: true` on every create from a
///   PQ-capable client, including every SDR title — which would both change
///   the ladder ceiling (`capability_height_ceiling_for_request`) for requests
///   that never asked, and pin the §4.5 mismatch counter permanently off zero,
///   making it useless for the one thing it exists to detect.
pub(crate) fn review_client_plan(
    caps: &plurx_core::playback::DeviceCaps,
    overrides: Option<&CreateOverrides>,
    file: &MediaFile,
    node: &plurx_core::playback::RenderCaps,
    asked_preserve_dolby_vision: bool,
    asked_hdr10: bool,
    now_ms: i64,
) -> PlanReview {
    review_client_plan_inner(
        caps,
        overrides,
        file,
        node,
        asked_preserve_dolby_vision,
        asked_hdr10,
        now_ms,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn review_client_plan_inner(
    caps: &plurx_core::playback::DeviceCaps,
    overrides: Option<&CreateOverrides>,
    file: &MediaFile,
    node: &plurx_core::playback::RenderCaps,
    asked_preserve_dolby_vision: bool,
    asked_hdr10: bool,
    now_ms: i64,
    count_create_metrics: bool,
) -> PlanReview {
    use plurx_core::playback::{decide_forced, DeviceProfile, Force};

    if count_create_metrics {
        plan_derivation::count_rederived();
    }
    let mut profile = DeviceProfile::from_caps_v2(caps);
    // The same clock the decision was taken under. Without this a create
    // would apply a learned limit that `/decision` had already let expire,
    // and the two halves of one playback would disagree about the plan for
    // the reason the re-derivation exists to eliminate.
    profile.retain_applicable_learned_limits(now_ms);
    let named_force = overrides.and_then(|o| o.force.as_deref());
    let force = named_force.map(Force::parse).unwrap_or(Force::Auto);
    let derived = decide_forced(file, &profile, force, node);
    let mut review = PlanReview {
        preserve_dolby_vision: derived.preserve_dolby_vision,
        convert_dolby_vision: derived.convert_dolby_vision,
        hdr10: asked_hdr10 && profile.supports_hdr10_transcode,
        notes: Vec::new(),
        mismatched: false,
    };
    let mut overridden = false;

    if matches!(force, Force::Original | Force::Transcode) {
        review
            .notes
            .push(format!("override force={}", named_force.unwrap_or("auto")));
        overridden = true;
    }

    // The compatible-base retry only ever *lowers* the plan, and only for
    // Dolby Vision. It cannot be used to claim a profile the caps deny.
    let compatible_hdr_base = overrides
        .and_then(|o| o.compatible_hdr_base)
        .unwrap_or(false);
    if compatible_hdr_base {
        overridden = true;
        if review.preserve_dolby_vision {
            review.preserve_dolby_vision = false;
            review.notes.push(
                "override compatible_hdr_base: Dolby Vision declined by the client".to_owned(),
            );
        } else {
            // Harmless, and worth saying: a client retrying a title the server
            // was never going to send as Dolby Vision is a client chasing the
            // wrong failure.
            review.notes.push(
                "override compatible_hdr_base had nothing to decline: the plan was not \
                 Dolby Vision"
                    .to_owned(),
            );
        }
    }
    if overridden && count_create_metrics {
        plan_derivation::count_overridden();
    }

    // Clamp, then report. Only the upward direction is a mismatch: `asked`
    // above `derived` is a claim the client's own document does not support.
    for (field, asked, derived) in [
        (
            "preserve_dolby_vision",
            asked_preserve_dolby_vision,
            &mut review.preserve_dolby_vision,
        ),
        ("hdr10", asked_hdr10, &mut review.hdr10),
    ] {
        if asked && !*derived {
            review.mismatched = true;
            review.notes.push(format!(
                "plan_mismatch: client asked {field}=true, server derived false"
            ));
        } else if !asked && *derived {
            // The client declined something it could have had. Its answer
            // stands; a named override, if one fired, has already said why.
            *derived = false;
        }
    }
    // Whatever the clamps and the overrides settled, the conversion follows
    // the preservation. A `compatible_hdr_base` retry that declined Dolby
    // Vision, or a client that never asked for it, must not be handed a
    // converted stream by a flag nobody looked at.
    review.convert_dolby_vision &= review.preserve_dolby_vision;
    if review.mismatched && count_create_metrics {
        plan_derivation::count_mismatched();
    }
    review
}

/// Who the create was, for the three log lines the arms below emit.
///
/// A struct rather than three parameters because none of them changes what is
/// derived — they only say whose create it was — and a signature that mixes
/// the two invites a future reader to derive something from `user_id`.
pub(crate) struct ReviewContext<'a> {
    pub file_id: i64,
    pub user_id: i64,
    pub client_build: &'a str,
}

/// Which derivation a create with a source row gets, and the counter that
/// says so.
///
/// This is a function rather than a `match` inside `create` because the arm
/// *is* the behaviour. `legacy_trusted_review` has been correct since #842 and
/// is well covered; what shipped broken before #842, and what a future edit
/// can silently restore, is the arm returning `None` for a build that sends no
/// caps document — after which `apply_plan_review` never runs and
/// `SessionKind::Copy` keeps the `convert_dolby_vision: false` it was built
/// with, which is a raw Profile 7 stream no consumer decoder takes. A test
/// that hands `resolve_plan` a review it built itself cannot see that; a test
/// on this function can, and does
/// (`a_create_that_sends_no_caps_document_still_gets_a_review`).
///
/// `create` keeps the fourth case — no source row — because it is the one that
/// must not ask the node what it can render on the way to a 404.
///
/// The node's render caps are therefore resolved once, by the caller, for
/// every create that has a file. That is one extra cheap read on the
/// unusable-caps path, which is a straggler population on its way to zero, and
/// it buys a derivation that takes no `AppState`.
#[allow(clippy::too_many_arguments)] // one create's worth of inputs
pub(super) fn plan_review_for(
    caps: Option<&plurx_core::playback::DeviceCaps>,
    overrides: Option<&CreateOverrides>,
    file: &MediaFile,
    node: &plurx_core::playback::RenderCaps,
    asked_preserve_dolby_vision: bool,
    asked_hdr10: bool,
    now_ms: i64,
    log: &ReviewContext<'_>,
) -> Option<PlanReview> {
    match caps {
        Some(caps) if caps.v == plurx_core::playback::DeviceCaps::VERSION && !caps.is_empty() => {
            let review = review_client_plan(
                caps,
                overrides,
                file,
                node,
                asked_preserve_dolby_vision,
                asked_hdr10,
                now_ms,
            );
            if review.mismatched {
                tracing::warn!(
                    target: "plurxd::http::hls",
                    file_id = log.file_id,
                    user_id = log.user_id,
                    client_build = %log.client_build,
                    asked_preserve_dolby_vision,
                    derived_preserve_dolby_vision = review.preserve_dolby_vision,
                    asked_hdr10,
                    derived_hdr10 = review.hdr10,
                    "plan_mismatch: the create body claims more than its own caps support; \
                     proceeding with the server's plan"
                );
            }
            Some(review)
        }
        // A document this server cannot read is worth exactly as much as no
        // document: its fields may not mean what v2's mean. It falls back to
        // the trust path rather than being refused, because a create is not
        // the place to fail a client over metadata — but it is counted
        // separately, because it is not the same population as a build that
        // predates caps v2 and it must not pollute the number that says the
        // migration is finished.
        //
        // "The trust path" is `legacy_trusted_review`, not the absence of a
        // review — and getting that wrong left this arm as the only create
        // path that could still build the delivery this milestone exists to
        // stop. `None` skips `apply_plan_review` entirely: the session keeps
        // the `convert_dolby_vision: false` `into_request` built it with *and*
        // the client's echoed `preserve_dolby_vision`, unclamped. For a
        // dual-layer source that is raw Profile 7 on the wire, reachable from
        // any client that posts a document this build cannot read beside
        // `preserve_dolby_vision: true` — a v1 body, an empty one, or every
        // client in the fleet at once on the day `DeviceCaps::VERSION` moves
        // to 3.
        //
        // The earlier reasoning for `None` was that an unreadable document
        // "may well have enumerated Profile 7", so deriving a conversion for
        // it overrides an answer its own document might have given. That reads
        // the risk backwards. A document that cannot be parsed enumerated
        // nothing *this server can act on*, which is the same position a build
        // sending no document at all is in; and the two outcomes are not
        // symmetric. Guessing "convert" costs a client that really did declare
        // 7 its enhancement layer, and it still plays. Guessing "preserve"
        // hands dual-layer to everyone else, and that does not.
        Some(caps) => {
            plan_derivation::count_unusable_caps();
            tracing::warn!(
                target: "plurxd::http::hls",
                file_id = log.file_id,
                client_build = %log.client_build,
                caps_version = caps.v,
                caps_empty = caps.is_empty(),
                "create could not read the caps document; trusting the client's echo"
            );
            Some(legacy_trusted_review(
                file,
                node,
                asked_preserve_dolby_vision,
                asked_hdr10,
            ))
        }
        None => {
            plan_derivation::count_legacy_trusted();
            tracing::warn!(
                target: "plurxd::http::hls",
                file_id = log.file_id,
                client_build = %log.client_build,
                "create trusted the client's plan echo: this build sends no caps document"
            );
            // Trusting the echo is right for everything the client can
            // actually state. It is wrong for the conversion, which no client
            // has ever been able to ask for: `CreateSession` carries no such
            // field, so the echo says nothing and `SessionKind::Copy` keeps
            // the `false` it was built with. That silently downgrades a
            // *server* decision — `/decision` answers "Profile 7 converted to
            // Profile 8.1 for this device" and the session one second later
            // preserves raw Profile 7, which is the one delivery no consumer
            // decoder outside Blu-ray hardware takes. Observed in production:
            // Safari answered `stream_rejected ... browser refused the remux
            // stream`, and the fallback tonemapped the title to SDR.
            //
            // So this arm derives the one field the echo cannot carry, and
            // nothing else.
            Some(legacy_trusted_review(
                file,
                node,
                asked_preserve_dolby_vision,
                asked_hdr10,
            ))
        }
    }
}

/// How to name the build in a create log line.
///
/// The v2 document names itself (`client.kind`/`client.build`), which is the
/// whole point of it carrying a `client` block at all. A body without one
/// leaves the User-Agent, truncated: these lines are for counting builds, not
/// for fingerprinting devices, and an unbounded header does not belong in a
/// log a support request gets pasted into.
pub(super) fn client_build_label(
    caps: Option<&plurx_core::playback::DeviceCaps>,
    headers: &HeaderMap,
) -> String {
    if let Some(client) = caps.and_then(|caps| caps.client.as_ref()) {
        let named = match (bound(&client.kind), bound(&client.build)) {
            (None, None) => None,
            (None, Some(build)) => Some(build),
            (Some(kind), None) => Some(kind),
            (Some(kind), Some(build)) => Some(format!("{kind}/{build}")),
        };
        if let Some(named) = named {
            return named;
        }
    }
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .and_then(bound)
        .unwrap_or_else(|| "unknown".to_owned())
}

/// A log-safe rendering of one caller-supplied identity string.
///
/// Control characters go first, then the length. The order matters: a body
/// field is as attacker-controlled as a header, and a newline inside it is
/// how one log line becomes two forged ones. `None` for anything that had no
/// printable content to begin with, so the caller can fall through to its
/// next source rather than logging an empty name.
fn bound(value: &str) -> Option<String> {
    let cleaned: String = value.chars().filter(|c| !c.is_control()).take(48).collect();
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

pub(super) const HDR_SUBTITLE_BURN_REFUSAL: &str =
    "That subtitle requires an SDR burn-in. HDR playback was kept unchanged.";

/// Would building this session with its requested burn cost the viewer the
/// dynamic range they would otherwise be getting?
///
/// This asks [`plurx_core::playback::burn_would_discard_hdr`] — the one guard
/// — about the grade this exact request resolves to **with no burn**, which
/// is the only grade the question has an honest answer against.
///
/// It replaces a predicate that keyed on the *source* `hdr` column and
/// demanded a `subtitle_burn_sdr` acknowledgement from the client. That
/// failed closed on sessions which were already being tone-mapped: a display
/// sending `hdr=0` gets an SDR transcode, drawing a forced PGS track into it
/// takes nothing away, and the server refused it anyway because the *file*
/// was HDR. The web client never sent the acknowledgement at all and Apple
/// computed it from the wrong range, so in practice the only way past the
/// guard was to not be an HDR title.
///
/// The grade comes from [`crate::transcode::TranscodeManager::grade_preview`]
/// rather than `Decision::transcode_grade`: that field ignores encoder proof
/// and can say HDR10 where `start` will deliver SDR, which would judge the
/// request by a different function than the one that delivers it.
pub(super) async fn burn_would_discard_this_session_hdr(
    state: &AppState,
    source: Option<&MediaFile>,
    request: &crate::transcode::SessionRequest,
    hdr10_requested: bool,
    height: i64,
) -> bool {
    let Some(file) = source else {
        // No source row: this request is on its way to a 404, and there is
        // nothing honest to say about a file we cannot see.
        return false;
    };
    let Some(index) = request.subtitle_burn.filter(|index| *index >= 0) else {
        return false;
    };
    let base_grade = state
        .transcode
        .grade_preview(file, hdr10_requested, height, None)
        .await;
    let (method, preserve, _) = session_delivery_shape(&request.kind);
    let base_range =
        plurx_core::playback::delivered_dynamic_range(file, method, preserve, base_grade);
    if !plurx_core::playback::burn_would_discard_hdr(base_range, true) {
        return false;
    }
    // Asked only on the way to a refusal: a track the store holds as a real
    // track with no cues has nothing to burn, so the session keeps its range
    // and starts without an overlay. The lookup order is the store's own —
    // the switch, the MPEG-TS rule, the manifest on the cache disk, and only
    // then the source's `fstat`.
    !crate::subtitle_source::stored_as_empty(
        &state.subtitle_source_access(),
        file,
        index,
        crate::subtitle_source::Live::Path(&file.path),
    )
    .await
}

/// The dynamic range this session puts on the wire, read off the session it
/// actually built rather than the decision that suggested it — a burn or a
/// forced rung produces a transcode `/decision` never promised, and the
/// badge has to follow the session (MEDIA-BADGES-PLAN §3.2).
///
/// `None` only when the source row could not be loaded; there is nothing
/// honest to say about a file we cannot see.
pub(super) fn session_delivered_dynamic_range(
    source: Option<&MediaFile>,
    kind: &crate::transcode::SessionKind,
    grade: plurx_core::transcode::OutputGrade,
) -> Option<&'static str> {
    let file = source?;
    let (method, preserve, _) = session_delivery_shape(kind);
    Some(plurx_core::playback::delivered_dynamic_range(
        file, method, preserve, grade,
    ))
}

/// The Dolby Vision profile this session's bytes carry, read off the session
/// it actually built — same rule, same reason, as the range beside it.
pub(super) fn session_delivered_dolby_vision_profile(
    source: Option<&MediaFile>,
    kind: &crate::transcode::SessionKind,
) -> Option<u8> {
    let file = source?;
    let (method, preserve, convert) = session_delivery_shape(kind);
    plurx_core::playback::delivered_dolby_vision_profile(file, method, preserve, convert)
}

/// What a session kind means to the two badge helpers.
///
/// One reading for both, so the range and the profile can never disagree
/// about the same delivery — a session badged `dolby_vision` with no profile,
/// or a profile on a stream whose range says HDR10, is a worse answer than
/// either field alone.
pub(super) fn session_delivery_shape(
    kind: &crate::transcode::SessionKind,
) -> (PlaybackMethod, bool, bool) {
    use crate::transcode::SessionKind;
    match kind {
        SessionKind::Copy {
            preserve_dolby_vision,
            convert_dolby_vision,
            ..
        } => (
            PlaybackMethod::Remux,
            *preserve_dolby_vision,
            *convert_dolby_vision,
        ),
        SessionKind::Transcode { .. } => (PlaybackMethod::Transcode, false, false),
    }
}

/// POST /api/v1/files/:id/hls/sessions — create a stream, or recover the one
/// an identical request already created.
/// Whether a restart has been superseded by the destination the client has
/// since settled on.
///
/// One expression rather than a predicate spread through the handler, because
/// the handler is not reachable from a test: it needs a store, a transcode
/// manager and an authenticated user. Everything that decides the outcome
/// lives here, so a test on this function pins the decision rather than
/// leaving the call site free to compute the anchor differently.
///
/// `false` for every absence. No `control_sequence` means a client that does
/// not send one, which is every client before the field existed and every
/// first play since — absent means *do the work*. No settled target means no
/// session, a retired actor, or a client that has not exchanged yet, and in
/// all three there is nothing to order against.
pub(super) fn restart_is_superseded(
    settled: Option<crate::playback_control::SettledTarget>,
    control_sequence: Option<u64>,
    start_seconds: f64,
) -> bool {
    let (Some(settled), Some(control_sequence)) = (settled, control_sequence) else {
        return false;
    };
    // A non-finite start is not a destination. Treating it as the head is the
    // conservative reading: it can only make this look *less* superseded, so a
    // malformed body cannot cancel a session the viewer still wants. The float
    // cast saturates rather than wrapping, so an absurd start clamps instead of
    // becoming a small anchor that would compare equal to a real one.
    let requested_anchor_ms = if start_seconds.is_finite() {
        (start_seconds.max(0.0) * 1_000.0) as i64
    } else {
        0
    };
    settled.supersedes(control_sequence, requested_anchor_ms)
}

/// What an admitted restart carries into its activation.
#[derive(Debug)]
pub(super) struct RestartAdmission {
    pub(super) expected_predecessor_incarnation_id: Option<String>,
    pub(super) fence_predecessor: bool,
}

/// Refuse a restart whose destination the viewer has already left, and say how
/// the activation that follows an admitted one is fenced.
///
/// Both halves live here because both are part of one claim M7's acceptance
/// makes: a storm starts work only for the settled target, and each successor
/// that does start takes the playback pointer from the exact predecessor it
/// replaces. `create` needs a store, a transcode manager and an authenticated
/// user, so neither half is reachable from a test through the handler — and a
/// predicate returning `bool` would let the acceptance assert about the
/// decision without ever seeing the refusal a client receives or the fence its
/// successor carries. This returns both.
pub(super) fn admit_restart(
    settled: Option<crate::playback_control::SettledTarget>,
    control_sequence: Option<u64>,
    start_seconds: f64,
    predecessor_incarnation_id: Option<&str>,
) -> Result<RestartAdmission, ApiError> {
    if restart_is_superseded(settled, control_sequence, start_seconds) {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "playback_target_superseded",
            "a later seek replaced this destination, so no session was started for it",
        ));
    }
    Ok(RestartAdmission {
        expected_predecessor_incarnation_id: predecessor_incarnation_id.map(str::to_owned),
        // Every activation is a predecessor CAS, an ordinary start included:
        // `Some` requires the playback pointer to name that exact incarnation
        // and `None` requires it to be absent, so a storm's losing restart can
        // never take the pointer from the successor that already has it.
        fence_predecessor: true,
    })
}

/// The height a request resolves to, which is not always the one asked for.
///
/// Three arms and three different promises, which is why this is one function
/// rather than three lines at a call site. Splitting it is how one of them
/// gets lost:
///
/// * **Auto is never snapped.** The server's own choice already lands where it
///   means to, and snapping would re-decide policy — a 900p source
///   deliberately transcodes at 900, with no scaler in the chain at all. This
///   is also the only arm that reads the network prior or the HDR10 ask.
/// * **The source's own height is a promise**, not a request: it is the
///   Original/forced-burn path the player reads as `sessionHeight`, and it is
///   never snapped and never downgraded.
/// * **An explicit rung from a menu snaps onto the ladder**, so a stray number
///   lands where the encoder has rungs — while an above-ladder height passes
///   through as what it is.
///
/// The clamp bounds the result and nothing else. It never binds downward,
/// because the snap has already put a below-ladder ask on the lowest rung; it
/// is the only thing bounding an above-ladder one.
///
/// Shared by `resolve_plan` and by M6's candidate, because it is the one part
/// of resolving a recipe that needs the store, the ladder ceiling and the
/// network prior — see
/// [M6-CALLER-HANDOFF.md](../../../../docs/playback-control/M6-CALLER-HANDOFF.md) §3.3 on why
/// the rest of `resolve_plan` is not what a candidate wants.
pub(crate) async fn resolve_height(
    state: &AppState,
    source: Option<&MediaFile>,
    network_prior: Option<&plurx_core::domain::NetworkPrior>,
    hdr10_requested: bool,
    asked: Option<i64>,
) -> i64 {
    let source_height = source.and_then(|f| f.height);
    match asked {
        None => {
            state
                .transcode
                .auto_height_for_request(source, network_prior, hdr10_requested)
                .await
        }
        Some(h) if Some(h) == source_height => h,
        Some(h) => crate::transcode::snap_height(h),
    }
    .clamp(crate::transcode::MIN_HEIGHT, crate::transcode::MAX_HEIGHT)
}

/// One resolved plan: the recipe a create body would produce right now.
///
/// M6's preparation decision has to ask *"if this client's selection were
/// honoured, what would we deliver?"* without creating anything, and that is
/// the same question `create` answers on its way to admission. Answered once,
/// here, rather than twice in two places that agree today —
/// [M6-CALLER-HANDOFF.md](../../../../docs/playback-control/M6-CALLER-HANDOFF.md) §3.2. **Do
/// not grow a second resolver.** The drift would be invisible, because both
/// sides would look correct in isolation.
pub(crate) struct ResolvedPlan {
    pub request: crate::transcode::SessionRequest,
    /// The height this plan resolved to, which is not always the one asked
    /// for.
    pub height: i64,
    /// The client's intent **as sent**, fingerprinted before the plan review
    /// is applied.
    ///
    /// Taken inside this function rather than by the caller, because the order
    /// is the contract: a transport retry that lands on a node running a
    /// different binary — or after a rescan changed the file's HDR facts —
    /// must fingerprint identically and get its own session back rather than a
    /// 409. A caller taking it afterwards would take a different fingerprint
    /// and have no way to notice.
    pub intent_fingerprint: String,
    pub plan_notes: Vec<String>,
    /// The client asked for a native text track rather than a burn, and which
    /// one. Resolved here because it is validated here — an index that names
    /// no track, or one that needs burning in, is refused before a recipe
    /// exists.
    pub native_subtitles: bool,
    pub native_subtitle: Option<i64>,
}

pub(crate) struct PlanInputs<'a> {
    pub state: &'a AppState,
    pub user_id: i64,
    pub file_id: i64,
    pub source: Option<&'a MediaFile>,
    pub network_prior: Option<&'a plurx_core::domain::NetworkPrior>,
}

/// Resolve a create body into the recipe it would produce.
///
/// `review` is a parameter rather than derived here, on purpose: deriving it
/// counts `plan_derivation` metrics, and those measure *what create did with
/// the plan it was handed*. A second caller counting them would stop the
/// number that says the caps-v2 migration is finished from meaning that.
///
/// Everything derivable *from* the review is derived here rather than passed
/// alongside it. `hdr10_requested` was a parameter for one revision, and that
/// was the drift seam this whole extraction exists to close: a second caller
/// passing the body's own `hdr10` where the review had said `false` would
/// resolve an Auto height against a ceiling `create` would never have used,
/// and nothing — not the type system, not a test — would notice.
///
/// This deliberately does **not** answer the delivered dynamic range. That
/// grade comes from the pipeline the session gets at start, so a recipe that
/// will never be built has none — which is why M6 reads the grade axis off the
/// request instead, as `GradeIntent`. See §3.2.1 of the caller handoff for the
/// claim that was withdrawn here.
pub(crate) async fn resolve_plan(
    inputs: PlanInputs<'_>,
    review: Option<PlanReview>,
    body: CreateSession,
) -> Result<ResolvedPlan, ApiError> {
    let PlanInputs {
        state,
        user_id,
        file_id,
        source,
        network_prior,
    } = inputs;
    let hdr10_requested = review
        .as_ref()
        .map(|review| review.hdr10)
        .unwrap_or(body.hdr10 == Some(true));
    let height = resolve_height(state, source, network_prior, hdr10_requested, body.height).await;
    let native_subtitles = body.native_subtitles == Some(true);
    let native_subtitle = body.subtitle.filter(|s| *s >= 0);
    if native_subtitles {
        if let Some(index) = native_subtitle {
            let track = source
                .and_then(|f| f.subtitle_streams.get(index as usize))
                .ok_or_else(|| ApiError::BadRequest("unknown native subtitle track".into()))?;
            if !is_native_text_subtitle(&track.codec) {
                return Err(ApiError::BadRequest(
                    "the selected subtitle requires burn-in".into(),
                ));
            }
        }
    }
    let mut request = body.into_request(file_id, height);
    if request
        .request_id
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > 128)
    {
        return Err(ApiError::BadRequest(
            "request_id must contain 1 to 128 characters".to_owned(),
        ));
    }
    if !worker_session_request_is_valid(&request) {
        return Err(ApiError::BadRequest(
            "media session request exceeds the supported cluster contract".to_owned(),
        ));
    }
    // The fingerprint is the client's intent as sent, which is what makes a
    // retry of the same body recover the same session no matter which binary
    // answers it. Only after it is taken does the server's reconciliation
    // apply to the request that will actually be built.
    let fingerprint = request.durable_intent_fingerprint(user_id);
    let plan_notes = match review {
        Some(review) => apply_plan_review(&mut request, review),
        None => Vec::new(),
    };
    Ok(ResolvedPlan {
        request,
        height,
        intent_fingerprint: fingerprint,
        plan_notes,
        native_subtitles,
        native_subtitle,
    })
}

/// A copy-video request is already choosing HLS, but an explicit progressive
/// sample-entry constraint still has to prove that HLS was claimed. Do this
/// before durable request admission so an incompatible plan cannot allocate a
/// session and only then discover that the client has no safe transport.
pub(super) async fn validate_hevc_copy_transport(
    state: &AppState,
    source: &MediaFile,
    caps: &plurx_core::playback::DeviceCaps,
    request: &crate::transcode::SessionRequest,
) -> Result<(), ApiError> {
    let crate::transcode::SessionKind::Copy {
        preserve_dolby_vision,
        ..
    } = request.kind
    else {
        return Ok(());
    };
    if caps.progressive_hevc_sample_entries.is_none() {
        return Ok(());
    }
    let probe_json = crate::hevc_census::probe_json_for_copy(state.store.as_ref(), source).await?;
    let promotes =
        plurx_core::transcode::hevc_parameter_set_promotion_required(source, probe_json.as_deref());
    let Some(actual) =
        super::super::stream::progressive_hevc_output_tag(source, preserve_dolby_vision, promotes)
    else {
        return Ok(());
    };
    if !caps.transports.iter().any(|transport| transport == "hls") {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "unsupported_hevc_delivery",
            format!("HEVC copy-HLS output uses {actual}, but HLS was not claimed by this client"),
        ));
    }
    super::super::stream::hevc_copy_requires_hls(source, caps, preserve_dolby_vision, promotes)?;
    Ok(())
}

pub async fn create(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    AxPath(id): AxPath<i64>,
    headers: HeaderMap,
    super::super::network::RemoteAddress(remote): super::super::network::RemoteAddress,
    Json(req): Json<CreateSession>,
) -> Result<Json<StartResponse>, ApiError> {
    create_with_purpose(user, state, id, headers, remote, req, None).await
}

/// The finite-session application service used by Library channels after it
/// has authoritatively resolved an occurrence. The purpose is not a public
/// field on ordinary VOD creation: old servers must reject the dedicated
/// route instead of silently turning a channel tune into history-writing VOD.
pub(crate) async fn create_for_library_channel(
    user: plurx_core::domain::User,
    state: AppState,
    file_id: i64,
    headers: HeaderMap,
    remote: Option<std::net::SocketAddr>,
    req: CreateSession,
    purpose: crate::http::library_channels::LibraryChannelPlaybackPurpose,
) -> Result<StartResponse, ApiError> {
    create_with_purpose(user, state, file_id, headers, remote, req, Some(purpose))
        .await
        .map(|Json(response)| response)
}

async fn create_with_purpose(
    user: plurx_core::domain::User,
    state: AppState,
    id: i64,
    headers: HeaderMap,
    remote: Option<std::net::SocketAddr>,
    req: CreateSession,
    library_channel: Option<crate::http::library_channels::LibraryChannelPlaybackPurpose>,
) -> Result<Json<StartResponse>, ApiError> {
    if let Some(caps) = req.caps.as_ref() {
        super::super::stream::validate_device_caps(caps)?;
    }
    let planning_caps = req
        .caps
        .as_ref()
        .filter(|caps| caps.v == plurx_core::playback::DeviceCaps::VERSION && !caps.is_empty())
        .cloned();
    let planning_overrides = planning_caps.as_ref().and(req.overrides.as_ref()).cloned();
    if !valid_playback_id(&req.playback_id) {
        return Err(ApiError::BadRequest(
            "playback_id must contain 1 to 128 safe characters".into(),
        ));
    }
    if req
        .presentation
        .as_deref()
        .is_some_and(|presentation| presentation != "vod")
    {
        return Err(ApiError::typed(
            StatusCode::GONE,
            "live_presentation_removed",
            "the growing live HLS presentation has been removed; request VOD",
        ));
    }
    // The ask is made durable before anything is built for it, and before this
    // request can be answered.
    //
    // Placed here, above every other decision, rather than beside the response:
    // a create that goes on to fail is still a viewer who asked, and the row
    // records the asking, not the outcome. The order that matters is the one
    // §1 names — canonical desired ownership persisted *before* an
    // intent-changing request is reported accepted — and answering first would
    // tell a client its selection was taken while nothing anywhere had recorded
    // it. A restart or an owner change in that window loses the ask entirely.
    //
    // A failed write refuses the create for the same reason the control
    // exchange refuses: a retry costs one request, and a phantom acceptance
    // costs a viewer their selection with nothing to point at.
    // `None` for a body that carried no ask: a playback with nothing recorded
    // must still activate, or the first play of every title would be refused.
    let mut recorded_ask_revision: Option<i64> = None;
    if let Some(intent) = req.intent.as_ref() {
        intent
            .validate()
            .map_err(|error| ApiError::BadRequest(format!("intent: {error}")))?;
        let selection = intent.selection;
        recorded_ask_revision = Some(
            state
                .store
                .record_desired_selection(
                    user.id,
                    &req.playback_id,
                    &intent.digest(),
                    &selection.canonical_form(),
                    unix_ms(),
                )
                .await
                .map_err(|error| {
                    tracing::warn!(
                        target: "plurxd::http::hls",
                        playback = %req.playback_id,
                        "recording the viewer's selection on create failed: {error}"
                    );
                    ApiError::ServiceUnavailable(
                        "this selection could not be recorded; retry shortly".to_owned(),
                    )
                })?
                .revision,
        );
        #[cfg(test)]
        {
            let pause = {
                let mut slot = CREATE_ASK_RECORDED_PAUSE
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                slot.take()
            };
            if let Some((reached, release)) = pause {
                let _ = reached.send(());
                let _ = release.await;
            }
        }
    }
    let ingress_serving_authority = state.serving.authority();
    let ingress_serving_generation = ingress_serving_authority.admit().ok_or_else(|| {
        ApiError::ServiceUnavailable("the ingress node has no serving authority".to_owned())
    })?;
    // The source height answers three things now: Auto, the ladder in the
    // response, and the snap's source-height escape. One read, from the read
    // pool.
    let source = state
        .store
        .get_file(id)
        .await
        .map_err(|error| session_store_error("reading the source file", error))?;
    // The HDR subtitle-burn guard used to stand here, keyed on the source's
    // own `hdr` column. It now runs after `resolve_plan`, against the grade
    // this request actually resolves to — see
    // `burn_would_discard_this_session_hdr`. Nothing between here and there
    // opens an encoder or takes a durable admission row: the ladder ceiling,
    // the network prior, `resolve_plan` and `validate_hevc_copy_transport`
    // are reads, and `claim_media_session_request` is the first write, after
    // it. `plan_review_for` is the one exception and it is not a write
    // either — it bumps the process-lifetime caps-migration counters, so a
    // create that this guard now refuses is counted as a straggler where
    // before it was refused first. That is a metric inflating slightly, not
    // state being left behind.
    //
    // `subtitle_burn_sdr` stays on the wire for clients that still send it,
    // and is no longer load-bearing. It was an acknowledgement the web client
    // never sent and Apple computed from the wrong range, so consulting it
    // decided nothing except which clients could burn at all.
    if req.subtitle_burn_sdr.is_some() {
        tracing::debug!(
            target: "plurxd::http::hls",
            file_id = id,
            subtitle_burn = req.subtitle_burn,
            subtitle_burn_sdr = req.subtitle_burn_sdr,
            "client sent the legacy SDR burn acknowledgement; the session's own grade decides"
        );
    }
    // Whose build this is, for every line below. The v2 document names
    // itself; a client that sends none leaves only its User-Agent, which is
    // exactly the population these lines exist to count down to zero.
    let client_build = client_build_label(req.caps.as_ref(), &headers);
    // E4: the client's echo stops being an instruction the moment it sends
    // the capabilities the plan was derived from. Everything below reads the
    // reconciled values, so there is exactly one decider again.
    // The reconciled plan, and the notes that explain it. `req` is left
    // untouched on purpose: the durable intent fingerprint below has to be a
    // pure function of the body as the client sent it, or a transport retry
    // that lands on a node running a different binary (or after a rescan
    // changed the file's HDR facts) fingerprints differently and gets a 409
    // where it should have got its own session back. The reconciled values
    // are applied to the built request afterwards.
    let review = match source.as_ref() {
        // No source row yet. This request is on its way to a 404; re-deriving
        // a plan for a file that is not there would say nothing, and counting
        // it would let any client hold the straggler metric off zero forever.
        // This is the one arm that stays here, because it is the only one that
        // must not ask the node what it can render.
        None => None,
        Some(file) => plan_review_for(
            req.caps.as_ref(),
            req.overrides.as_ref(),
            file,
            &super::super::stream::render_caps(&state).await,
            req.preserve_dolby_vision == Some(true),
            req.hdr10 == Some(true),
            unix_ms(),
            &ReviewContext {
                file_id: id,
                user_id: user.id,
                client_build: &client_build,
            },
        ),
    };
    let hdr10_requested = review
        .as_ref()
        .map(|review| review.hdr10)
        .unwrap_or(req.hdr10 == Some(true));
    let source_height = source.as_ref().and_then(|f| f.height);
    let ladder_ceiling = state
        .transcode
        .capability_height_ceiling_for_request(source.as_ref(), hdr10_requested)
        .await;
    let mut identity = super::super::network::identity(&headers, remote);
    if let Some(ref mut id) = identity {
        id.user_id = Some(user.id);
        id.credential_generation = Some(plurx_core::domain::CredentialGeneration::derive(
            user.id,
            user.created_at,
            &user.password_hash,
        ));
    }
    let network_prior =
        super::super::network::stored_prior(state.store.as_ref(), identity.as_ref())
            .await
            .map_err(|error| {
                ApiError::ServiceUnavailable(format!("reading the network prior: {error:?}"))
            })?;
    let resolved = resolve_plan(
        PlanInputs {
            state: &state,
            user_id: user.id,
            file_id: id,
            source: source.as_ref(),
            network_prior: network_prior.as_ref(),
        },
        review,
        req,
    )
    .await?;
    let request = resolved.request;
    if let (Some(source), Some(caps)) = (source.as_ref(), planning_caps.as_ref()) {
        validate_hevc_copy_transport(&state, source, caps, &request).await?;
    }
    let height = resolved.height;
    if burn_would_discard_this_session_hdr(
        &state,
        source.as_ref(),
        &request,
        hdr10_requested,
        height,
    )
    .await
    {
        return Err(ApiError::Unprocessable(serde_json::json!({
            "code": "hdr_subtitle_burn_refused",
            "error": HDR_SUBTITLE_BURN_REFUSAL,
        })));
    }
    let fingerprint = match library_channel.as_ref() {
        Some(purpose) => purpose.bind_session_fingerprint(&resolved.intent_fingerprint),
        None => resolved.intent_fingerprint,
    };
    let plan_notes = resolved.plan_notes;
    let native_subtitles = resolved.native_subtitles;
    let native_subtitle = resolved.native_subtitle;
    let now_ms = unix_ms();
    let mut incarnation_id = uuid::Uuid::new_v4().to_string();
    // Every create occupies a durable admission row. A caller-supplied key
    // keeps public idempotency; an ordinary create uses its unguessable
    // incarnation internally so omitting request_id cannot bypass the
    // in-flight cap or allocate encoders before admission.
    let request_claim_id = request
        .request_id
        .clone()
        .unwrap_or_else(|| incarnation_id.clone());
    match state
        .store
        .claim_media_session_request(
            user.id,
            &request_claim_id,
            &fingerprint,
            &request.playback_id,
            &incarnation_id,
            now_ms,
            now_ms.saturating_add(60_000),
        )
        .await
        .map_err(|error| session_store_error("claiming the media-session request", error))?
    {
        MediaSessionRequestClaim::Acquired {
            incarnation_id: acquired,
        } => incarnation_id = acquired,
        MediaSessionRequestClaim::Resolved(route) if resolved_replay_is_live(&route, unix_ms()) => {
            let replay_deadline = tokio::time::Instant::now() + ACTIVATION_STORE_DEADLINE;
            let _response_publication = ingress_serving_authority
                .commit_guard_before(ingress_serving_generation, replay_deadline.into_std())
                .await
                .ok_or_else(|| {
                    ApiError::ServiceUnavailable(
                        "the ingress lost serving authority before resolved replay".to_owned(),
                    )
                })?;
            let route = tokio::time::timeout_at(
                replay_deadline,
                state
                    .store
                    .media_session_route_by_incarnation(&route.incarnation_id),
            )
            .await
            .map_err(|_| {
                ApiError::ServiceUnavailable(
                    "resolved media-session replay freshness check timed out".to_owned(),
                )
            })?
            .map_err(|error| session_store_error("rechecking the resolved replay", error))?
            .filter(|current| {
                replay_route_identity_matches(current, &route)
                    && resolved_replay_is_live(current, unix_ms())
            })
            .ok_or_else(|| {
                ApiError::ServiceUnavailable(
                    "the resolved media-session replay is no longer current".to_owned(),
                )
            })?;
            let mut response = serde_json::from_str::<StartResponse>(&route.response_json)?;
            // Same-session owner takeover advances only the control epoch.
            // Replaying the persisted create must not hand a restarted client
            // the stale epoch embedded when owner 1 first activated.
            if let Some(control) = response.control.as_ref() {
                response.control =
                    control.refreshed(&route.session_id, &route.incarnation_id, route.owner_epoch);
            }
            return Ok(Json(response));
        }
        MediaSessionRequestClaim::Resolved(route)
            if route.state == "active" && route.publication_ready_at_ms != 0 =>
        {
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "media_session_handoff_pending",
                "the predecessor owner has not completed session handoff yet; retry shortly",
            ));
        }
        MediaSessionRequestClaim::Resolved(_) => {
            return Err(ApiError::typed(
                StatusCode::GONE,
                "media_session_ended",
                "this idempotent session was already released",
            ));
        }
        MediaSessionRequestClaim::InFlight {
            incarnation_id: in_flight_incarnation,
            ..
        } => {
            let observed_at_ms = unix_ms();
            let route = tokio::time::timeout(
                ACTIVATION_STORE_DEADLINE,
                state
                    .store
                    .media_session_route_by_incarnation(&in_flight_incarnation),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
            .filter(|route| {
                route.user_id == user.id
                    && route.playback_id == request.playback_id
                    && route.request_fingerprint == fingerprint
                    && route.state == "active"
                    && route.publication_ready_at_ms == 0
                    && route.lease_expires_at_ms > observed_at_ms
            });
            if let Some(route) = route {
                let publication_deadline = tokio::time::Instant::now() + ACTIVATION_STORE_DEADLINE;
                let _response_publication = ingress_serving_authority
                    .commit_guard_before(
                        ingress_serving_generation,
                        publication_deadline.into_std(),
                    )
                    .await
                    .ok_or_else(|| {
                        ApiError::ServiceUnavailable(
                            "the ingress lost serving authority before replay publication"
                                .to_owned(),
                        )
                    })?;
                let route = tokio::time::timeout_at(
                    publication_deadline,
                    state.store.publish_media_session_activation(
                        user.id,
                        &request_claim_id,
                        &in_flight_incarnation,
                        unix_ms(),
                    ),
                )
                .await
                .map_err(|_| {
                    ApiError::ServiceUnavailable(
                        "media-session replay publication exceeded its fixed deadline".to_owned(),
                    )
                })?
                .map_err(|error| session_store_error("publishing the session replay", error))?
                .filter(|published| replay_publication_matches(published, &route))
                .ok_or_else(|| {
                    ApiError::ServiceUnavailable(
                        "the media-session replay lost its exact activation".to_owned(),
                    )
                })?;
                state.media_sessions.cache_route(route.clone()).await;
                if route.owner_node_id == state.node_id {
                    state.media_sessions.seed_owned_lease(&route).await;
                }
                let mut response = serde_json::from_str::<StartResponse>(&route.response_json)?;
                if let Some(control) = response.control.as_ref() {
                    response.control = control.refreshed(
                        &route.session_id,
                        &route.incarnation_id,
                        route.owner_epoch,
                    );
                }
                return Ok(Json(response));
            }
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "media_session_starting",
                "an identical session request is still starting; retry shortly",
            ));
        }
        MediaSessionRequestClaim::Conflict => {
            return Err(ApiError::Conflict(
                "request_id was already used for a different session intent".to_owned(),
            ));
        }
        MediaSessionRequestClaim::Overloaded => {
            return Err(ApiError::ServiceUnavailable(
                "too many session starts are already active for this user".to_owned(),
            ));
        }
    }
    // From this point every early return must settle the exact durable claim.
    // Once worker placement returns, its StartedSessionGuard takes over the
    // same responsibility together with exact worker ownership.
    let mut request_guard = MediaSessionRequestGuard::new(
        state.clone(),
        user.id,
        request_claim_id.clone(),
        incarnation_id.clone(),
    );

    let advertise_control = plurx_core::store::stored_switch(
        state
            .store
            .get_setting(plurx_core::store::keys::PLAYBACK_CONTROL_PROTOCOL_V1)
            .await
            .map_err(|error| session_store_error("reading the control protocol setting", error))?
            .as_deref(),
        true,
    );

    let mut worker_request = request.clone();
    worker_request.request_id = Some(incarnation_id.clone());
    let remote_request = RemoteStartRequest {
        protocol_version: crate::media_pool::PROTOCOL_VERSION,
        incarnation_id: incarnation_id.clone(),
        user_id: user.id,
        // The source snapshot a later takeover must match exactly (§7.3). A
        // row we could not read records an impossible snapshot rather than a
        // plausible one, so takeover refuses instead of reproducing a session
        // against a file it never verified.
        source_size: source.as_ref().map_or(0, |f| f.size),
        source_mtime: source.as_ref().map_or(0, |f| f.mtime),
        // Recorded from the same decision the worker will make, so a later
        // takeover can tell whether this URL was ever serving a shape a
        // successor is allowed to continue.
        typeless_playlist: state.transcode.cluster_playlist_is_typeless().await,
        library_channel: library_channel.as_ref().map(|purpose| {
            serde_json::to_value(purpose).expect("bounded Library-channel purpose serializes")
        }),
        request: worker_request,
    };
    let recipe_json = serde_json::to_string(&remote_request)?;
    if library_channel.is_some()
        && !state
            .store
            .record_library_channel_session_recipe(
                user.id,
                &request_claim_id,
                &incarnation_id,
                &recipe_json,
                unix_ms(),
            )
            .await
            .map_err(|error| session_store_error("recording the channel session purpose", error))?
    {
        return Err(ApiError::ServiceUnavailable(
            "the channel session purpose could not be recorded; retry shortly".to_owned(),
        ));
    }
    let placement_deadline = super::super::peer_transport::deadline_after(START_DEADLINE);

    // Every activation is a predecessor CAS, including an ordinary start.
    // Capturing the exact route before worker placement gives a
    // commit-unknown reconciler the identity it must terminalize; allowing an
    // unfenced last-writer-wins activation loses that identity after the
    // pointer moves to the successor.
    let activation_predecessor = state
        .store
        .media_session_route_for_playback(user.id, &request.playback_id)
        .await
        .map_err(|error| session_store_error("reading the predecessor route", error))?;
    // One mint for this start, bound here rather than called twice.
    //
    // `recovery_epoch_for` is not a pure function: with no predecessor it
    // draws a fresh UUID, which is the whole point of it — a deliberate new
    // play is a new budget, and `recovery_epoch_for`'s own test asserts that
    // two calls with `None` differ. So calling it once for the session and
    // again for the activation would give a new play two epochs: the live
    // session would believe in one that exists in no durable row, and the
    // next continuation would inherit the other and find it unspent. That is
    // one automatic recovery per attempt against a file that cannot be
    // decoded, which is the loop the budget exists to close.
    //
    // Bound above placement because the session that carries it is built by
    // the task the placement loop spawns, 268 lines before the activation
    // literal is reached. The only input is the predecessor route, already
    // read above, so this costs one binding and no store read.
    let recovery_epoch = recovery_epoch_for(activation_predecessor.as_ref());
    if activation_predecessor
        .as_ref()
        .is_some_and(|route| route.state == "active" && route.publication_ready_at_ms != 0)
    {
        return Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "media_session_handoff_pending",
            "the current session is still completing its predecessor handoff; retry shortly",
        ));
    }
    // A seek storm asks for one session per destination. The control actor
    // already knows which destination the client settled on, and the sequence
    // on this request says where in that ordering this ask belongs -- so a
    // restart for a target the viewer has since scrolled past can be refused
    // before it spawns a producer.
    //
    // Refused, not deferred: waiting to see whether a newer exchange arrives
    // would add latency to every honest seek, and the ordering is already
    // decided. Only a strictly later exchange naming a different destination
    // supersedes, so the honest seek arriving next is never the one skipped.
    let settled_target = match activation_predecessor.as_ref() {
        // No predecessor is a first play: there is no ordering for it to be
        // stale against, and no session to ask.
        Some(predecessor) => {
            state
                .transcode
                .settled_target_for_session(&predecessor.session_id)
                .await
        }
        None => None,
    };
    let admission = admit_restart(
        settled_target,
        request.control_sequence,
        request.start_seconds,
        activation_predecessor
            .as_ref()
            .map(|route| route.incarnation_id.as_str()),
    )?;
    let RestartAdmission {
        expected_predecessor_incarnation_id,
        fence_predecessor,
    } = admission;
    let pinned_owner = if let Some(previous_session_id) = request
        .previous_session_id
        .as_deref()
        .filter(|value| uuid::Uuid::parse_str(value).is_ok())
    {
        if let Some(route) = activation_predecessor.as_ref() {
            if route.session_id != previous_session_id
                || route.user_id != user.id
                || route.state != "active"
                || route.lease_expires_at_ms <= unix_ms()
            {
                return Err(ApiError::typed(
                    StatusCode::CONFLICT,
                    "media_session_superseded",
                    "the session being reopened is no longer current",
                ));
            }
            Some(route.owner_node_id.clone())
        } else if state
            .transcode
            .active_session_ids()
            .await
            .iter()
            .any(|session_id| session_id == previous_session_id)
        {
            // A session created by the pre-P5 binary has no durable route.
            // Its reopen state is nevertheless process-local, so pin that
            // rolling-upgrade legacy predecessor to this node rather than
            // ranking a peer that cannot possess it.
            Some(state.node_id.clone())
        } else {
            None
        }
    } else {
        None
    };
    let mut owner_candidates = if let Some(owner) = pinned_owner {
        vec![owner]
    } else if !state.media_pool.remote_placement_ready(&state).await {
        vec![state.node_id.clone()]
    } else {
        match MediaOfferRequest::new(
            source.as_ref().ok_or(ApiError::NotFound("file"))?,
            height,
            (request.start_seconds * 1_000.0).round().max(0.0) as i64,
            request.audio_index,
            request.subtitle_burn,
            request.hdr10,
        ) {
            Ok(offer_request) => state
                .media_pool
                .offers(&state, offer_request)
                .await
                .offers
                .into_iter()
                .filter(|offer| offer.eligible)
                .fold(Vec::new(), |mut candidates, offer| {
                    if !candidates.contains(&offer.node_id) {
                        candidates.push(offer.node_id);
                    }
                    candidates
                }),
            Err(_) => Vec::new(),
        }
    };
    // Cold snapshots and one-node recovery retain the established local
    // behavior. A non-empty ranked list is authoritative: an ineligible local
    // offer must not bypass an eligible peer or capacity refusal.
    if owner_candidates.is_empty() {
        owner_candidates.push(state.node_id.clone());
    }

    let mut started = None;
    let mut last_error = None;
    for candidate in owner_candidates {
        if tokio::time::Instant::now() >= placement_deadline {
            last_error = Some(ApiError::ServiceUnavailable(
                "media worker placement exceeded its common deadline".to_owned(),
            ));
            break;
        }
        let result = if candidate == state.node_id {
            if !ingress_serving_authority.is_current(ingress_serving_generation) {
                last_error = Some(ApiError::ServiceUnavailable(
                    "the local media worker has no serving authority".to_owned(),
                ));
                continue;
            }
            let admitted_serving_generation = ingress_serving_generation;
            // Session creation owns a child process before publishing the map
            // entry. An owned task reaches a verdict even if this request is
            // cancelled, and its returned guard cleans the exact late worker
            // if nobody receives it.
            let transcode = Arc::clone(&state.transcode);
            let worker_request = remote_request.request.clone();
            let user_name = user.username.clone();
            let guard_state = state.clone();
            let guard_owner = candidate.clone();
            let guard_incarnation = incarnation_id.clone();
            let guard_request = request_claim_id.clone();
            let guard_user = user.id;
            let worker_recovery = crate::transcode::SessionRecoveryIdentity {
                user_id: user.id,
                incarnation_id: incarnation_id.clone(),
                recovery_epoch: recovery_epoch.clone(),
            };
            let worker_serving_authority = ingress_serving_authority.clone();
            let mut start_task = tokio::spawn(async move {
                let started = transcode
                    .create_cluster_session(
                        &worker_request,
                        &worker_recovery,
                        &user_name,
                        placement_deadline,
                        admitted_serving_generation,
                    )
                    .await?;
                let crate::transcode::ClusterSessionStart {
                    info,
                    replacement,
                    created,
                } = started;
                let session_id = info.session_id.clone();
                let response = RemoteStartResponse::from(info);
                let guard = Some(if created {
                    StartedSessionGuard::new(
                        guard_state,
                        guard_owner,
                        guard_incarnation,
                        session_id,
                        guard_user,
                        guard_request,
                        Some(replacement),
                    )
                } else {
                    StartedSessionGuard::recovered(
                        guard_state,
                        guard_owner,
                        guard_incarnation,
                        session_id,
                        guard_user,
                        guard_request,
                        Some(replacement),
                    )
                });
                if !worker_serving_authority.is_current(admitted_serving_generation) {
                    return Err(crate::transcode::serving_fence_error(
                        "the local worker lost authority before shared-cache pinning",
                    ));
                }
                Ok((response, guard, Some(admitted_serving_generation)))
            });
            match tokio::time::timeout_at(placement_deadline, &mut start_task).await {
                Ok(Ok(result)) => result.map_err(|error| session_start_error(id, error)),
                Ok(Err(error)) => Err(ApiError::Internal(format!(
                    "local media worker task failed: {error}"
                ))),
                Err(_) => {
                    tokio::spawn(async move {
                        // Dropping a successful output drops its armed guard.
                        let _ = start_task.await;
                    });
                    // Deliberately still codeless, i.e. still not retried.
                    //
                    // Both 50-second failures on m6 came through here rather
                    // than through `session_start_error`, so naming it was
                    // tempting. But with the burn sidecar bounded above, a cold
                    // sidecar no longer reaches this arm at all — what does is
                    // a start that spent 50 s queuing for an encoder slot, and
                    // there the codeless 503 is correct back-pressure. Making
                    // it retryable would add three more create posts per viewer
                    // at exactly the moment the node is saturated, and the
                    // abandoned start is detached rather than aborted, so the
                    // node still pays for every one of them.
                    Err(ApiError::ServiceUnavailable(
                        "local media worker exceeded the placement deadline".to_owned(),
                    ))
                }
            }
        } else {
            match state
                .media_sessions
                .start_remote(&candidate, &remote_request, placement_deadline)
                .await
            {
                Ok(started) => {
                    let target_generation = started.info.activation_generation;
                    let guard = match started.ownership {
                        crate::media_sessions::RemoteSessionStartOwnership::Created
                        | crate::media_sessions::RemoteSessionStartOwnership::Recovered => {
                            Some(StartedSessionGuard::claim_only(
                                state.clone(),
                                candidate.clone(),
                                incarnation_id.clone(),
                                started.info.session_id.clone(),
                                user.id,
                                request_claim_id.clone(),
                            ))
                        }
                        crate::media_sessions::RemoteSessionStartOwnership::LegacyAmbiguous => {
                            return Err(ApiError::ServiceUnavailable(format!(
                                "media worker {candidate} does not support safe activation ownership"
                            )));
                        }
                    };
                    let Some(target_generation) = target_generation else {
                        return Err(ApiError::ServiceUnavailable(format!(
                            "media worker {candidate} did not return an activation generation"
                        )));
                    };
                    Ok((started.info, guard, Some(target_generation)))
                }
                Err(error) => Err(ApiError::ServiceUnavailable(format!(
                    "media worker {candidate} could not start the session: {error:?}"
                ))),
            }
        };
        match result {
            Ok((info, guard, serving_generation)) if info.is_valid() => {
                started = Some((candidate, info, guard, serving_generation));
                break;
            }
            Ok((_info, _guard, _serving_generation)) => {
                // The armed guard aborts an invalid local result just as the
                // peer transport rejects and aborts an invalid remote result.
                tracing::warn!(
                    target: "plurxd::http::hls",
                    owner_node_id = %candidate,
                    "media worker returned an invalid start contract"
                );
                last_error = Some(ApiError::ServiceUnavailable(format!(
                    "media worker {candidate} returned an invalid start contract"
                )));
            }
            Err(error) => {
                tracing::debug!(
                    target: "plurxd::http::hls",
                    owner_node_id = %candidate, "media worker start attempt failed"
                );
                last_error = Some(error);
            }
        }
    }
    let Some((owner_node_id, info, guard, local_serving_generation)) = started else {
        return Err(last_error.unwrap_or_else(|| {
            ApiError::ServiceUnavailable("no eligible media worker was available".to_owned())
        }));
    };
    request_guard.disarm();
    if !ingress_serving_authority.is_current(ingress_serving_generation) {
        return Err(ApiError::ServiceUnavailable(
            "the ingress node lost serving authority during worker placement".to_owned(),
        ));
    }
    if owner_node_id == state.node_id {
        let provisional_pin_ms =
            i64::try_from(REMOTE_ACTIVATION_CONFIRMATION_WINDOW.as_millis()).unwrap_or(i64::MAX);
        if !pin_shared_session_for_local_start(
            placement_deadline,
            state.transcode.pin_shared_session(
                &info.session_id,
                &incarnation_id,
                1,
                unix_ms().saturating_add(provisional_pin_ms),
            ),
        )
        .await?
        {
            return Err(ApiError::ServiceUnavailable(
                "shared cache generation changed before session activation".to_owned(),
            ));
        }
        if owner_node_id == state.node_id
            && local_serving_generation
                .is_some_and(|generation| !ingress_serving_authority.is_current(generation))
        {
            return Err(ApiError::ServiceUnavailable(
                "the local media worker lost serving authority before activation".to_owned(),
            ));
        }
    }
    if !ingress_serving_authority.is_current(ingress_serving_generation) {
        return Err(ApiError::ServiceUnavailable(
            "the ingress node lost serving authority before owner assignment".to_owned(),
        ));
    }
    match tokio::time::timeout(
        OWNER_ASSIGNMENT_DEADLINE,
        state.store.assign_media_session_request_owner(
            user.id,
            &request_claim_id,
            &incarnation_id,
            &owner_node_id,
            unix_ms(),
        ),
    )
    .await
    {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            // The armed guard aborts the exact worker and fails this claim.
            return Err(ApiError::ServiceUnavailable(
                "session ownership changed while placement was being committed".to_owned(),
            ));
        }
        Ok(Err(error)) => {
            return Err(session_store_error(
                "assigning the media-session owner",
                error,
            ));
        }
        Err(_) => {
            return Err(ApiError::ServiceUnavailable(
                "session ownership assignment timed out".to_owned(),
            ));
        }
    }
    if !ingress_serving_authority.is_current(ingress_serving_generation) {
        return Err(ApiError::ServiceUnavailable(
            "the ingress node lost serving authority before session activation".to_owned(),
        ));
    }
    // The grade the session actually built, not the one the body asked for:
    // the server refuses the HDR10 rung for a source or a rung that cannot
    // prove it, and the badge has to follow the encoder.
    let delivered = session_delivered_dynamic_range(source.as_ref(), &info.kind, info.grade);
    // A session being created is playback beginning — the honest moment for
    // the scrobble that used to fire from `/decision`. Read the normalized
    // route from the result: a bound Auto stall may have changed a copy request
    // into a lower transcode before the session was spawned.
    let method = match info.kind {
        crate::transcode::SessionKind::Copy { .. } => crate::delivery::Method::HlsCopy,
        crate::transcode::SessionKind::Transcode { .. } => crate::delivery::Method::Transcode,
    };
    let playlist_url = if native_subtitles {
        // Give the multivariant playlist its own path. AVPlayer caches HLS
        // resources by URL and can otherwise conflate `index.m3u8?native=1`
        // with the child `index.m3u8` media playlist referenced by that
        // master. Keep the query form in `playlist` for clients holding an
        // older session response, but new sessions use the unambiguous path.
        let master = format!("/api/v1/hls/{}/master.m3u8", info.session_id);
        match native_subtitle {
            Some(index) => format!("{master}?subtitle={index}"),
            None => master,
        }
    } else {
        info.playlist_url
    };
    let response = StartResponse {
        session_id: info.session_id.clone(),
        playlist_url,
        duration_ms: info.duration_ms,
        start_seconds: info.start_seconds,
        media_origin_ms: Some((info.media_origin_seconds * 1000.0).round() as i64),
        height: info.target_height,
        encoder: info.encoder.clone(),
        vod: info.vod,
        // Capped at what this node can actually serve for this file, not just
        // at the source height: an advertised rung is a promise, and the web
        // ABR controller upgrades into any rung the ladder lists. See
        // `capability_height_ceiling`.
        ladder: crate::transcode::advertised_ladder(source_height, ladder_ceiling),
        prior_kbps: network_prior.and_then(|prior| prior.sustained_kbps),
        delivered_dynamic_range: delivered.map(str::to_owned),
        delivered_dolby_vision_profile: session_delivered_dolby_vision_profile(
            source.as_ref(),
            &info.kind,
        ),
        control: advertise_control.then(|| {
            crate::playback_control::ControlBootstrap::new(
                &info.session_id,
                &incarnation_id,
                1,
                info.control_lease_timeout_ms,
            )
            .expect("new media-session owner epochs begin at one")
        }),
        plan_notes,
    };
    // `response_json` is the canonical durable session recipe/response. Keep
    // channel purpose in that row so restart and owner takeover cannot turn a
    // following session into ordinary VOD. `StartResponse` deliberately
    // ignores additive fields, preserving every existing recovery reader.
    let response_json =
        if library_channel.is_some() || planning_caps.is_some() || planning_overrides.is_some() {
            let mut value = serde_json::to_value(&response)?;
            let object = value
                .as_object_mut()
                .expect("StartResponse serializes as an object");
            if let Some(purpose) = library_channel.as_ref() {
                object.insert("library_channel".to_owned(), serde_json::to_value(purpose)?);
            }
            if let Some(caps) = planning_caps.as_ref() {
                object.insert(
                    PLANNING_CAPS_RESPONSE_FIELD.to_owned(),
                    serde_json::to_value(caps)?,
                );
            }
            if let Some(overrides) = planning_overrides.as_ref() {
                object.insert(
                    PLANNING_OVERRIDES_RESPONSE_FIELD.to_owned(),
                    serde_json::to_value(overrides)?,
                );
            }
            serde_json::to_string(&value)?
        } else {
            serde_json::to_string(&response)?
        };
    let activation_now_ms = unix_ms();
    let activation = MediaSessionActivation {
        // The one mint for this start, from the local bound above placement.
        //
        // This used to call `recovery_epoch_for` here, with a comment saying a
        // local copy could be believed after the store had ruled otherwise.
        // That reasoning is about a *read*: the store decides the row's epoch,
        // an activation that loses a race keeps whatever the winning row had,
        // and `activation_route_matches` does not compare this field — all
        // still true, and all reasons to trust the route the store hands back
        // rather than this value. It was never a reason to mint twice, and
        // minting twice is what calling it here a second time now does, since
        // the session start above needs the same epoch this row is about to
        // be given.
        recovery_epoch: recovery_epoch.clone(),
        // The revision this create recorded, not the one that is current now.
        //
        // Re-reading here would defeat the compare: a control exchange that
        // advanced the ask while this create was in flight would be read back
        // as the expectation, and the pointer would advance to a session built
        // for the selection the viewer has left. What this activation was
        // decided against is what create wrote at its very first step.
        expected_desired_revision: recorded_ask_revision,
        incarnation_id: incarnation_id.clone(),
        session_id: info.session_id.clone(),
        user_id: user.id,
        playback_id: request.playback_id.clone(),
        expected_predecessor_incarnation_id: expected_predecessor_incarnation_id.clone(),
        fence_predecessor,
        request_id: Some(request_claim_id.clone()),
        request_fingerprint: fingerprint,
        owner_node_id: owner_node_id.clone(),
        lease_expires_at_ms: activation_now_ms.saturating_add(LEASE_TTL_MS),
        recipe_json,
        response_json,
        publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms: (info.media_origin_seconds * 1_000.0).round() as i64,
        now_ms: activation_now_ms,
    };
    let publication_activation = activation.clone();
    let mut publication_guard =
        ActivationPublicationGuard::new(state.clone(), publication_activation.clone());
    // The detached owner keeps both serving serialization and exact worker
    // cleanup until the route is publishable or its fixed activation lease
    // expires. The Store future itself may outlive its caller, so every first
    // commit is BLOCKED and cannot be renewed or taken over before this owner
    // confirms it.
    let owner_activation_generation = local_serving_generation.ok_or_else(|| {
        ApiError::ServiceUnavailable("media worker omitted its activation authority".to_owned())
    })?;
    let mut activation_task = if owner_node_id == state.node_id {
        let activation_state = state.clone();
        let activation_authority = ingress_serving_authority.clone();
        tokio::spawn(async move {
            activate_session_under_authority(
                activation_state,
                activation,
                activation_predecessor,
                expected_predecessor_incarnation_id,
                activation_authority,
                owner_activation_generation,
                guard,
            )
            .await
            .map(|_| ())
        })
    } else {
        let activation_state = state.clone();
        let activation_owner = owner_node_id.clone();
        tokio::spawn(async move {
            let request = crate::media_sessions::RemoteActivateRequest {
                target_generation: owner_activation_generation,
                activation,
            };
            let activation_deadline = tokio::time::Instant::now()
                + Duration::from_millis(u64::try_from(LEASE_TTL_MS).unwrap_or_default());
            activation_state
                .media_sessions
                .activate_remote(&activation_owner, &request, activation_deadline)
                .await
                .map_err(|error| {
                    ApiError::ServiceUnavailable(format!(
                        "media worker {activation_owner} could not activate the session: {error:?}"
                    ))
                })?;
            let mut guard = guard;
            let route = match wait_for_publishable_activation(
                &activation_state,
                &request.activation,
                activation_deadline,
            )
            .await
            {
                PublishableActivationWait::Ready(route) => *route,
                PublishableActivationWait::Pending => {
                    if let Some(guard) = guard.as_mut() {
                        guard.disarm();
                    }
                    return Err(ApiError::typed(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "media_session_handoff_pending",
                        "the predecessor handoff remains durably owned; retry this request",
                    ));
                }
                PublishableActivationWait::Gone => return Err(ApiError::ServiceUnavailable(format!(
                    "media worker {activation_owner} did not confirm the session before its owner lease"
                ))),
            };
            activation_state.media_sessions.cache_route(route).await;
            if let Some(guard) = guard.as_mut() {
                guard.disarm();
            }
            Ok(())
        })
    };
    let activation_deadline = activation_lease_deadline(&publication_activation);
    match tokio::time::timeout_at(activation_deadline, &mut activation_task).await {
        Ok(Ok(Ok(_))) => {}
        Ok(Ok(Err(error))) => {
            if matches!(
                &error,
                ApiError::Typed {
                    code: "media_session_handoff_pending",
                    ..
                }
            ) {
                publication_guard.disarm();
            }
            return Err(error);
        }
        Ok(Err(error)) => {
            return Err(ApiError::Internal(format!(
                "media activation task failed: {error}"
            )))
        }
        Err(_) => {
            // The HTTP deadline never cancels either ownership decision. The
            // detached activation keeps its worker guard; this publication
            // guard abandons a completed-but-unpublished route, but disarms
            // when the durable predecessor handoff is intentionally pending.
            tokio::spawn(async move {
                match activation_task.await {
                    Ok(Ok(_)) => {}
                    Ok(Err(error))
                        if matches!(
                            &error,
                            ApiError::Typed {
                                code: "media_session_handoff_pending",
                                ..
                            }
                        ) =>
                    {
                        publication_guard.disarm();
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(
                            target: "plurxd::http::hls",
                            ?error, "detached media activation settled unsuccessfully"
                        );
                    }
                    Err(error) => {
                        tracing::error!(
                            target: "plurxd::http::hls",
                            %error, "detached media activation task failed"
                        );
                    }
                }
            });
            return Err(ApiError::ServiceUnavailable(
                "session activation is still being reconciled".to_owned(),
            ));
        }
    }
    // The selected target owns and authority-fences the durable activation.
    // The ingress needs only to linearize publication of this successful
    // response against its own loss transition; retaining an ingress read
    // guard across remote RPC/polling would delay failover for the whole owner
    // lease. A retry through a healthy ingress can publish the starting claim if
    // this exact ingress loses authority before response publication.
    let publication_deadline = tokio::time::Instant::now() + ACTIVATION_STORE_DEADLINE;
    let _response_publication = ingress_serving_authority
        .commit_guard_before(ingress_serving_generation, publication_deadline.into_std())
        .await
        .ok_or_else(|| {
            ApiError::ServiceUnavailable(
                "the ingress lost serving authority before start response publication".to_owned(),
            )
        })?;
    let published_route = tokio::time::timeout_at(
        publication_deadline,
        state.store.publish_media_session_activation(
            user.id,
            &request_claim_id,
            &incarnation_id,
            unix_ms(),
        ),
    )
    .await
    .map_err(|_| {
        ApiError::ServiceUnavailable(
            "media-session response publication exceeded its fixed deadline".to_owned(),
        )
    })?
    .map_err(|error| session_store_error("publishing the media-session response", error))?
    .filter(|route| {
        route_matches_activation(route, &publication_activation)
            && route.publication_ready_at_ms == 0
    })
    .ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "media-session response publication lost its exact activation".to_owned(),
        )
    })?;
    state
        .media_sessions
        .cache_route(published_route.clone())
        .await;
    if published_route.owner_node_id == state.node_id {
        state
            .media_sessions
            .seed_owned_lease(&published_route)
            .await;
    }
    if library_channel.is_none() {
        crate::playstart::note_playback_started(
            &state,
            user.id,
            &user.username,
            id,
            method,
            Some(&request.playback_id),
        );
    }
    publication_guard.disarm();
    Ok(Json(response))
}

pub(super) fn resolved_replay_is_live(route: &MediaSessionRoute, now_ms: i64) -> bool {
    route.state == "active"
        && route.publication_ready_at_ms == 0
        && route.lease_expires_at_ms > now_ms.saturating_add(MIN_RESOLVED_REPLAY_REMAINING_MS)
}

pub(super) fn route_matches_activation(
    route: &MediaSessionRoute,
    activation: &MediaSessionActivation,
) -> bool {
    route.incarnation_id == activation.incarnation_id
        && route.session_id == activation.session_id
        && route.user_id == activation.user_id
        && route.playback_id == activation.playback_id
        && route.request_fingerprint == activation.request_fingerprint
        && route.owner_node_id == activation.owner_node_id
        && route.owner_epoch == 1
        && route.state == "active"
        && route.lease_expires_at_ms > unix_ms()
        && route.recipe_json == activation.recipe_json
        && route.response_json == activation.response_json
}

pub(super) fn replay_publication_matches(
    published: &MediaSessionRoute,
    observed: &MediaSessionRoute,
) -> bool {
    replay_route_identity_matches(published, observed)
        && published.state == "active"
        && published.publication_ready_at_ms == 0
        && published.lease_expires_at_ms > unix_ms()
}

fn replay_route_identity_matches(
    current: &MediaSessionRoute,
    observed: &MediaSessionRoute,
) -> bool {
    current.incarnation_id == observed.incarnation_id
        && current.session_id == observed.session_id
        && current.user_id == observed.user_id
        && current.playback_id == observed.playback_id
        && current.request_fingerprint == observed.request_fingerprint
        && current.recipe_json == observed.recipe_json
        && current.response_json == observed.response_json
        && current.media_origin_ms == observed.media_origin_ms
}

/// Lease time held back from a retried activation confirmation so the
/// committed-write recovery read always has budget.
///
/// The bounded idempotent-write retry can spend the entire remaining owner
/// lease on consensus timeouts. Spending it here is what
/// [`wait_for_confirmed_activation`] exists to survive, and that reader is
/// entered with the same lease deadline: without a reservation it observes
/// `now >= deadline`, returns `None` on its first statement, and abandons an
/// activation whose confirmation had in fact committed.
///
/// This is one complete consistent-read *attempt*, not a whole read: an
/// authority read that times out retries once more inside the Store, and
/// reserving that full envelope would leave the write itself under two
/// attempts. The reservation is therefore a target rather than a floor -- see
/// [`activation_confirmation_deadline`], which never takes more than half of
/// whatever lease is actually left, so a confirmation entered late keeps a
/// proportional share for recovery instead of losing the reservation entirely.
const ACTIVATION_CONFIRMATION_RECOVERY_MARGIN: Duration = ACTIVATION_STORE_DEADLINE;

const _: () = assert!(
    ACTIVATION_CONFIRMATION_RECOVERY_MARGIN.as_millis() * 2 <= LEASE_TTL_MS as u128,
    "the confirmation recovery reservation must leave most of the owner lease for the write"
);

/// Split the remaining owner lease between the confirmation write and the
/// recovery read that has to survive it.
///
/// Taking the margin only when the full margin fits is what an earlier cut of
/// this did, and it removed the reservation in exactly the case that needs it:
/// a preparation that ran long leaves under a margin of lease, the confirmation
/// takes all of it, and the recovery read is entered already expired. Half of
/// what is left is always available, so the reservation degrades instead.
fn activation_confirmation_deadline(lease_deadline: tokio::time::Instant) -> tokio::time::Instant {
    let remaining = lease_deadline.saturating_duration_since(tokio::time::Instant::now());
    lease_deadline - ACTIVATION_CONFIRMATION_RECOVERY_MARGIN.min(remaining / 2)
}

fn activation_lease_deadline(activation: &MediaSessionActivation) -> tokio::time::Instant {
    let remaining_ms = activation
        .lease_expires_at_ms
        .saturating_sub(activation.now_ms)
        .clamp(0, LEASE_TTL_MS);
    tokio::time::Instant::now()
        + Duration::from_millis(u64::try_from(remaining_ms).unwrap_or_default())
}

fn spawn_activation_abandonment(state: AppState, activation: MediaSessionActivation) {
    tokio::spawn(async move {
        if let Err(error) = state
            .store
            .settle_media_session_activation(
                &activation,
                MediaSessionActivationSettlement::Abandon,
                unix_ms(),
            )
            .await
        {
            tracing::warn!(
                target: "plurxd::http::hls",
                %error, "provisional media activation abandonment failed"
            );
        }
    });
}

pub(super) async fn settle_activation_publication_cleanup(
    state: AppState,
    activation: MediaSessionActivation,
) {
    let remaining_ms = activation
        .lease_expires_at_ms
        .saturating_sub(unix_ms())
        .clamp(0, LEASE_TTL_MS);
    let deadline = tokio::time::Instant::now()
        + Duration::from_millis(u64::try_from(remaining_ms).unwrap_or_default());
    let mut settlement_attempt = 0_u64;
    loop {
        settlement_attempt = settlement_attempt.saturating_add(1);
        let now = tokio::time::Instant::now();
        let attempt_deadline = now + ACTIVATION_STORE_DEADLINE;
        match tokio::time::timeout_at(
            attempt_deadline,
            state.store.settle_media_session_activation(
                &activation,
                MediaSessionActivationSettlement::Abandon,
                unix_ms(),
            ),
        )
        .await
        {
            // Atomic response publication won the race. Preserve its worker;
            // an idempotent retry is now entitled to recover this route.
            Ok(Ok(Some(route)))
                if route.state == "active"
                    && route.publication_ready_at_ms == 0
                    && route.lease_expires_at_ms > unix_ms() =>
            {
                return;
            }
            // Abandonment won, the route disappeared, or ownership advanced.
            // Exact worker cleanup is idempotent in all three cases.
            Ok(Ok(_)) => {
                abort_started_session(
                    &state,
                    &activation.owner_node_id,
                    &activation.incarnation_id,
                    &activation.session_id,
                )
                .await;
                return;
            }
            Ok(Err(error)) => {
                if settlement_attempt == 1 || settlement_attempt.is_multiple_of(15) {
                    tracing::warn!(
                        target: "plurxd::http::hls",
                        %error,
                        settlement_attempt,
                        "activation response reaper is retrying exact settlement"
                    );
                }
            }
            Err(_) => {
                if settlement_attempt == 1 || settlement_attempt.is_multiple_of(15) {
                    tracing::warn!(
                        target: "plurxd::http::hls",
                        settlement_attempt,
                        "activation response reaper timed out exact settlement"
                    );
                }
            }
        }
        let retry_delay = if tokio::time::Instant::now() < deadline {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(1)
        };
        tokio::time::sleep(retry_delay).await;
    }
}

pub(in crate::http) async fn activate_session_under_authority(
    state: AppState,
    activation: MediaSessionActivation,
    reconciled_predecessor: Option<MediaSessionRoute>,
    predecessor_incarnation: Option<String>,
    authority: crate::serving_fence::ServingAuthority,
    admitted_generation: u64,
    guard: Option<StartedSessionGuard>,
) -> Result<MediaSessionActivationOutcome, ApiError> {
    let lease_deadline = activation_lease_deadline(&activation);
    let serving_transition = authority
        .commit_guard_before(admitted_generation, lease_deadline.into_std())
        .await
        .ok_or_else(|| {
            ApiError::ServiceUnavailable(
                "the media owner lost serving authority before durable activation".to_owned(),
            )
        })?;
    let prepared = tokio::time::timeout_at(
        lease_deadline,
        state.store.activate_media_session(&activation),
    )
    .await;
    let mut outcome = match prepared {
        Ok(Ok(Some(outcome))) if route_matches_activation(&outcome.route, &activation) => outcome,
        Ok(Ok(Some(_))) | Ok(Ok(None)) => {
            spawn_activation_abandonment(state.clone(), activation.clone());
            drop(serving_transition);
            return Err(ApiError::ServiceUnavailable(
                "session ownership could not be prepared".to_owned(),
            ));
        }
        Ok(Err(store_error)) => {
            let error = session_store_error("preparing the media session", store_error);
            let Some(route) = wait_for_exact_activation(&state, &activation, lease_deadline).await
            else {
                spawn_activation_abandonment(state.clone(), activation.clone());
                drop(serving_transition);
                return Err(error);
            };
            MediaSessionActivationOutcome {
                route,
                predecessor: reconciled_predecessor,
            }
        }
        Err(_) => {
            // Prepare and Abandon are mutually excluding Store transactions.
            // If the late prepare wins it remains BLOCKED until Abandon
            // tombstones it; if Abandon wins the request predicate prevents
            // the late prepare from publishing any route.
            spawn_activation_abandonment(state.clone(), activation.clone());
            drop(serving_transition);
            return Err(ApiError::ServiceUnavailable(
                "session preparation exceeded its fixed owner lease".to_owned(),
            ));
        }
    };
    if tokio::time::Instant::now() >= lease_deadline {
        spawn_activation_abandonment(state.clone(), activation.clone());
        drop(serving_transition);
        return Err(ApiError::ServiceUnavailable(
            "session preparation exhausted its publication lease".to_owned(),
        ));
    }
    let confirmation_now_ms = unix_ms();
    let publication_ready_at_ms = predecessor_incarnation.as_ref().map_or(0, |_| {
        confirmation_now_ms.saturating_add(
            i64::try_from(TERMINAL_PROJECTION_SAFETY_WINDOW.as_millis()).unwrap_or(i64::MAX),
        )
    });
    // The confirmation write may retry inside the Store; the recovery read
    // below is the only path that can still observe a write that committed
    // behind a lost response, so it keeps its own reserved share of the lease.
    let confirmation_deadline = activation_confirmation_deadline(lease_deadline);
    let confirmation = tokio::time::timeout_at(
        confirmation_deadline,
        state.store.settle_media_session_activation(
            &activation,
            MediaSessionActivationSettlement::Confirm {
                publication_ready_at_ms,
            },
            confirmation_now_ms,
        ),
    )
    .await;
    let route = match confirmation {
        Ok(Ok(Some(route))) if route_matches_activation(&route, &activation) => route,
        Ok(Ok(Some(_))) | Ok(Ok(None)) | Ok(Err(_)) | Err(_) => {
            let confirmed =
                wait_for_confirmed_activation(&state, &activation, lease_deadline).await;
            let Some(route) = confirmed else {
                spawn_activation_abandonment(state.clone(), activation.clone());
                drop(serving_transition);
                return Err(ApiError::ServiceUnavailable(
                    "session confirmation did not settle before its fixed owner lease".to_owned(),
                ));
            };
            route
        }
    };
    outcome.route = route.clone();
    // Serving loss needs the write side of this transition, so the atomic
    // BLOCKED-to-ready/armed confirmation above linearizes before any loss.
    drop(serving_transition);
    state.media_sessions.cache_route(route.clone()).await;
    settle_activation_predecessor(
        &state,
        predecessor_incarnation,
        route,
        authority,
        admitted_generation,
        guard,
    )
    .await?;
    Ok(outcome)
}

async fn wait_for_exact_activation(
    state: &AppState,
    activation: &MediaSessionActivation,
    deadline: tokio::time::Instant,
) -> Option<MediaSessionRoute> {
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return None;
        }
        match tokio::time::timeout_at(
            deadline,
            state
                .store
                .media_session_route_by_incarnation(&activation.incarnation_id),
        )
        .await
        {
            Ok(Ok(Some(route))) if route_matches_activation(&route, activation) => {
                return Some(route)
            }
            Ok(Ok(Some(_))) => return None,
            Ok(Ok(None)) | Ok(Err(_)) => {}
            Err(_) => return None,
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250).min(remaining)).await;
    }
}

pub(in crate::http) async fn wait_for_confirmed_activation(
    state: &AppState,
    activation: &MediaSessionActivation,
    deadline: tokio::time::Instant,
) -> Option<MediaSessionRoute> {
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return None;
        }
        match tokio::time::timeout_at(
            deadline,
            state
                .store
                .media_session_route_by_incarnation(&activation.incarnation_id),
        )
        .await
        {
            Ok(Ok(Some(route)))
                if route_matches_activation(&route, activation)
                    && route.publication_ready_at_ms != MEDIA_SESSION_PUBLICATION_BLOCKED =>
            {
                return Some(route)
            }
            Ok(Ok(Some(route)))
                if route_matches_activation(&route, activation)
                    && route.publication_ready_at_ms == MEDIA_SESSION_PUBLICATION_BLOCKED => {}
            Ok(Ok(Some(_))) => return None,
            Ok(Ok(None)) | Ok(Err(_)) => {}
            Err(_) => return None,
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250).min(remaining)).await;
    }
}

enum PublishableActivationWait {
    Ready(Box<MediaSessionRoute>),
    Pending,
    Gone,
}

async fn wait_for_publishable_activation(
    state: &AppState,
    activation: &MediaSessionActivation,
    deadline: tokio::time::Instant,
) -> PublishableActivationWait {
    let mut observed_pending_handoff = false;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return if observed_pending_handoff {
                PublishableActivationWait::Pending
            } else {
                PublishableActivationWait::Gone
            };
        }
        match tokio::time::timeout_at(
            deadline,
            state
                .store
                .media_session_route_by_incarnation(&activation.incarnation_id),
        )
        .await
        {
            Ok(Ok(Some(route)))
                if route_matches_activation(&route, activation)
                    && route.publication_ready_at_ms == 0 =>
            {
                return PublishableActivationWait::Ready(Box::new(route));
            }
            Ok(Ok(Some(route)))
                if route_matches_activation(&route, activation)
                    && route.publication_ready_at_ms != MEDIA_SESSION_PUBLICATION_BLOCKED =>
            {
                observed_pending_handoff = true;
            }
            Ok(Ok(Some(route)))
                if route_matches_activation(&route, activation)
                    && route.publication_ready_at_ms == MEDIA_SESSION_PUBLICATION_BLOCKED => {}
            Ok(Ok(Some(_))) => return PublishableActivationWait::Gone,
            Ok(Ok(None)) | Ok(Err(_)) => {}
            Err(_) => {
                return if observed_pending_handoff {
                    PublishableActivationWait::Pending
                } else {
                    PublishableActivationWait::Gone
                };
            }
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return if observed_pending_handoff {
                PublishableActivationWait::Pending
            } else {
                PublishableActivationWait::Gone
            };
        }
        tokio::time::sleep(Duration::from_millis(250).min(remaining)).await;
    }
}

/// The recovery budget's identity for one activation.
///
/// Decided from the playback pointer, because that is the only thing that
/// knows whether this is a deliberate new play or a continuation of one
/// already in progress. A new play mints; every continuation — an automatic
/// reopen, a seek, a track change, an ownership handoff — carries its
/// predecessor's, and that is the entire difference between one automatic
/// decode recovery per playback and one per attempt.
///
/// What the epoch buys, stated exactly: under one `playback_id`, a new request
/// id, a new client session id, or a different owner node cannot present
/// themselves as a fresh playback and be granted a second budget. It does not
/// bound a client that varies `playback_id` itself — the ledger's key contains
/// `playback_id`, so a fresh one lands on a fresh row whatever the epoch is.
/// That client is bounded by the ordinary session caps (`MAX_CURRENT_PER_USER`
/// and the per-node active count), not by this.
///
/// An empty predecessor epoch is inherited as empty rather than replaced.
/// Those are sessions that started before the column existed and never had a
/// budget; minting here would hand a fresh allowance to every continuation of
/// one, which is exactly the failure this prevents, arriving through an
/// upgrade instead of through a client. They gain an epoch the next time
/// somebody deliberately presses play.
pub(super) fn recovery_epoch_for(predecessor: Option<&MediaSessionRoute>) -> String {
    predecessor.map_or_else(
        || uuid::Uuid::new_v4().to_string(),
        |route| route.recovery_epoch.clone(),
    )
}

pub(super) fn valid_playback_id(playback_id: &str) -> bool {
    !playback_id.trim().is_empty()
        && playback_id.len() <= 128
        && !playback_id
            .bytes()
            .any(|byte| matches!(byte, b'\r' | b'\n' | 0))
}

pub(super) async fn abort_started_session(
    state: &AppState,
    owner_node_id: &str,
    incarnation_id: &str,
    session_id: &str,
) {
    if owner_node_id == state.node_id {
        if !state
            .transcode
            .stop_session_for_owner(incarnation_id, session_id, 1, "cluster start aborted")
            .await
        {
            state
                .transcode
                .stop_vod_session_for_request(incarnation_id, session_id, "cluster start aborted")
                .await;
        }
    } else if let Err(error) = state
        .media_sessions
        .abort_remote(
            owner_node_id,
            &RemoteAbortRequest {
                incarnation_id: incarnation_id.to_owned(),
                session_id: session_id.to_owned(),
                expected_owner_epoch: 1,
                reason: None,
            },
        )
        .await
    {
        tracing::warn!(
            target: "plurxd::http::hls",
            error = ?error, "remote cluster-start abort did not settle"
        );
    }
}

/// Prime one rolling prepared successor under its already-reserved durable
/// capability.
///
/// Ordinary cluster creation deliberately mints a process-local session id.
/// Preparation has already minted the public id in the Store, so the worker
/// is adopted under that id before its cancellation guard is disarmed. The
/// guard moves through the same registry transaction: cancellation before the
/// move cleans the provisional id, and cancellation after it cleans the
/// durable id. Only a successfully adopted worker backed by the reserved row
/// is left for the preparation deadline/abort owner to settle.
pub(in crate::http) async fn prime_live_prepared_session(
    state: &AppState,
    recipe: &RemoteStartRequest,
    durable_session_id: &str,
    recovery_epoch: &str,
    expected_owner_epoch: i64,
    deadline: tokio::time::Instant,
) -> bool {
    if recipe.request.presentation != crate::transcode::Presentation::Live
        || tokio::time::Instant::now() >= deadline
    {
        return false;
    }
    let Some(_restart_admission) = state.serving.try_restart_admission().await else {
        return false;
    };
    let authority = state.serving.authority();
    let Some(admitted_generation) = authority.admit() else {
        return false;
    };
    let user = match state.store.get_user(recipe.user_id).await {
        Ok(Some(user)) => user,
        _ => return false,
    };
    let recovery = crate::transcode::SessionRecoveryIdentity {
        user_id: recipe.user_id,
        incarnation_id: recipe.incarnation_id.clone(),
        recovery_epoch: recovery_epoch.to_owned(),
    };
    let started = match state
        .transcode
        .create_cluster_prepared_session(
            &recipe.request,
            &recovery,
            &user.username,
            deadline,
            admitted_generation,
        )
        .await
    {
        Ok(started) => started,
        Err(error) => {
            tracing::warn!(
                target: "plurxd::http::hls",
                %error, "rolling prepared successor could not start"
            );
            return false;
        }
    };
    let crate::transcode::ClusterSessionStart {
        info,
        replacement,
        created,
    } = started;
    let mut guard = if created {
        StartedSessionGuard::worker_only(
            state.clone(),
            state.node_id.clone(),
            recipe.incarnation_id.clone(),
            info.session_id.clone(),
            recipe.user_id,
            recipe.incarnation_id.clone(),
            Some(replacement),
        )
    } else {
        StartedSessionGuard::replayed(
            state.clone(),
            state.node_id.clone(),
            recipe.incarnation_id.clone(),
            info.session_id.clone(),
            recipe.user_id,
            recipe.incarnation_id.clone(),
            Some(replacement),
        )
    };
    if !authority.is_current(admitted_generation) {
        return false;
    }
    let expires_at_ms = unix_ms().saturating_add(
        i64::try_from(
            deadline
                .saturating_duration_since(tokio::time::Instant::now())
                .as_millis(),
        )
        .unwrap_or(i64::MAX),
    );
    let pinned = super::super::internal_media_sessions::pin_shared_session_before_deadline(
        deadline,
        state.transcode.pin_shared_session(
            &info.session_id,
            &recipe.incarnation_id,
            expected_owner_epoch,
            expires_at_ms,
        ),
    )
    .await
    .unwrap_or(false);
    if !pinned || !authority.is_current(admitted_generation) {
        return false;
    }
    let Some(adoption) = state.transcode.session_adoption_token(durable_session_id) else {
        return false;
    };
    guard = match state
        .transcode
        .adopt_session_id_with_owner(&info.session_id, durable_session_id, adoption, guard)
        .await
    {
        Ok(guard) => guard,
        Err(_) => return false,
    };
    if !authority.is_current(admitted_generation) {
        return false;
    }
    guard.disarm();
    true
}

pub(super) async fn settle_activation_predecessor(
    state: &AppState,
    predecessor_incarnation: Option<String>,
    successor: MediaSessionRoute,
    authority: crate::serving_fence::ServingAuthority,
    admitted_generation: u64,
    mut guard: Option<StartedSessionGuard>,
) -> Result<(), ApiError> {
    // Before the early return, because a plain create for this playback is a
    // supersession too: a client that reopened without naming its predecessor
    // has still gone around the handoff, and the successor it abandoned would
    // otherwise run to the deadline. One call site rather than the two the plan
    // named — every activation reaches this function, and `keep` is what makes
    // the committed successor's own activation safe here.
    cancel_preparations_for_superseded_predecessor(
        &successor.playback_id,
        Some(successor.incarnation_id.as_str()),
    );
    let Some(predecessor_incarnation) = predecessor_incarnation else {
        if let Some(guard) = guard.as_mut() {
            guard.disarm();
        }
        return Ok(());
    };
    debug_assert_ne!(
        successor.publication_ready_at_ms,
        MEDIA_SESSION_PUBLICATION_BLOCKED
    );
    let fast_deadline = tokio::time::Instant::now() + PREDECESSOR_PROJECTION_FAST_WINDOW;
    if project_activation_predecessor_until(state, &predecessor_incarnation, fast_deadline).await {
        match complete_activation_handoff_until(
            state,
            &successor,
            MediaSessionProjectionCompletion::PredecessorAcknowledged,
            &authority,
            admitted_generation,
            fast_deadline,
        )
        .await
        {
            ActivationHandoffVerdict::Ready => {
                if let Some(guard) = guard.as_mut() {
                    guard.disarm();
                }
                return Ok(());
            }
            ActivationHandoffVerdict::SuccessorGone => {
                return Err(media_session_ended());
            }
            ActivationHandoffVerdict::Pending => {}
            ActivationHandoffVerdict::AuthorityLost => {
                return Err(ApiError::ServiceUnavailable(
                    "the media owner lost serving authority during session handoff".to_owned(),
                ));
            }
        }
    }

    // Keep the activated successor's cleanup guard and replacement permit in
    // a cancellation-independent owner. The caller gets a typed retry while
    // this owner continues; publication cannot proceed until either the exact
    // predecessor acknowledges terminal control or every response admitted
    // by that generation has crossed the safety boundary.
    let projection_state = state.clone();
    tokio::spawn(async move {
        let settled = settle_armed_activation_handoff(
            &projection_state,
            predecessor_incarnation,
            successor,
            authority,
            admitted_generation,
        )
        .await;
        if settled {
            if let Some(guard) = guard.as_mut() {
                guard.disarm();
            }
        }
        // Otherwise the armed guard performs exact worker cleanup. A serving
        // generation which has been lost can never publish the finite route.
    });
    Err(ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "media_session_handoff_pending",
        "the predecessor handoff remains durably owned; retry this request",
    ))
}

pub(super) async fn settle_armed_activation_handoff(
    state: &AppState,
    predecessor_incarnation: String,
    successor: MediaSessionRoute,
    authority: crate::serving_fence::ServingAuthority,
    admitted_generation: u64,
) -> bool {
    let remaining_ms = successor
        .publication_ready_at_ms
        .saturating_sub(unix_ms())
        .max(0);
    let boundary_deadline = tokio::time::Instant::now()
        + Duration::from_millis(u64::try_from(remaining_ms).unwrap_or(u64::MAX));
    let acknowledged =
        project_activation_predecessor_until(state, &predecessor_incarnation, boundary_deadline)
            .await;
    if acknowledged
        && matches!(
            complete_activation_handoff_until(
                state,
                &successor,
                MediaSessionProjectionCompletion::PredecessorAcknowledged,
                &authority,
                admitted_generation,
                boundary_deadline,
            )
            .await,
            ActivationHandoffVerdict::Ready | ActivationHandoffVerdict::SuccessorGone
        )
    {
        return true;
    }
    tracing::warn!(
        target: "plurxd::http::hls",
        incarnation = %predecessor_incarnation,
        safety_window_seconds = TERMINAL_PROJECTION_SAFETY_WINDOW.as_secs(),
        "predecessor acknowledgement was unavailable through the response-lifetime safety boundary"
    );
    matches!(
        complete_activation_handoff_until(
            state,
            &successor,
            MediaSessionProjectionCompletion::SafetyBoundaryElapsed {
                expected_not_before_ms: successor.publication_ready_at_ms,
            },
            &authority,
            admitted_generation,
            tokio::time::Instant::now() + ACTIVATION_STORE_DEADLINE,
        )
        .await,
        ActivationHandoffVerdict::Ready | ActivationHandoffVerdict::SuccessorGone
    )
}

pub(super) enum ActivationHandoffVerdict {
    Ready,
    SuccessorGone,
    Pending,
    AuthorityLost,
}

pub(super) async fn complete_activation_handoff_until(
    state: &AppState,
    successor: &MediaSessionRoute,
    proof: MediaSessionProjectionCompletion,
    authority: &crate::serving_fence::ServingAuthority,
    admitted_generation: u64,
    deadline: tokio::time::Instant,
) -> ActivationHandoffVerdict {
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return ActivationHandoffVerdict::Pending;
        }
        let attempt_deadline = (now + ACTIVATION_STORE_DEADLINE).min(deadline);
        let Some(serving_transition) = authority
            .commit_guard_before(admitted_generation, attempt_deadline.into_std())
            .await
        else {
            return ActivationHandoffVerdict::AuthorityLost;
        };
        match tokio::time::timeout_at(
            attempt_deadline,
            state.store.complete_media_session_handoff(
                &successor.incarnation_id,
                &successor.owner_node_id,
                successor.owner_epoch,
                proof,
                unix_ms(),
            ),
        )
        .await
        {
            Ok(Ok(Some(route))) => {
                drop(serving_transition);
                state.media_sessions.cache_route(route).await;
                return ActivationHandoffVerdict::Ready;
            }
            Ok(Ok(None)) => {
                drop(serving_transition);
                // A terminal or replaced successor no longer needs a
                // publication handoff; the armed cleanup guard may settle it.
                if let Ok(Ok(Some(route))) = tokio::time::timeout_at(
                    attempt_deadline,
                    state
                        .store
                        .media_session_route_by_incarnation(&successor.incarnation_id),
                )
                .await
                {
                    if route.state != "active"
                        || route.owner_node_id != successor.owner_node_id
                        || route.owner_epoch != successor.owner_epoch
                    {
                        return ActivationHandoffVerdict::SuccessorGone;
                    }
                }
            }
            Ok(Err(error)) => {
                drop(serving_transition);
                tracing::warn!(
                    target: "plurxd::http::hls",
                    error = ?error,
                    incarnation = %successor.incarnation_id,
                    "successor publication-fence completion is retrying"
                );
            }
            Err(_) => drop(serving_transition),
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return ActivationHandoffVerdict::Pending;
        }
        tokio::time::sleep(PREDECESSOR_PROJECTION_RETRY_DELAY.min(remaining)).await;
    }
}

pub(super) async fn project_activation_predecessor_until(
    state: &AppState,
    predecessor_incarnation: &str,
    deadline: tokio::time::Instant,
) -> bool {
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let read_deadline = (now + ACTIVATION_STORE_DEADLINE).min(deadline);
        let route = tokio::time::timeout_at(
            read_deadline,
            state
                .store
                .media_session_route_by_incarnation(predecessor_incarnation),
        )
        .await;
        if let Ok(Ok(Some(route))) = route {
            if route.incarnation_id == predecessor_incarnation && route.state == "ended" {
                let Some(terminal) = crate::vodserve::Terminal::from_durable_reason(
                    route.terminal_reason.as_deref(),
                ) else {
                    tracing::error!(
                        target: "plurxd::http::hls",
                        incarnation = %predecessor_incarnation,
                        terminal_reason = ?route.terminal_reason,
                        "ended activation predecessor has no valid durable terminal cause"
                    );
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        return false;
                    }
                    tokio::time::sleep(PREDECESSOR_PROJECTION_RETRY_DELAY.min(remaining)).await;
                    continue;
                };
                let projection_deadline = (tokio::time::Instant::now()
                    + PREDECESSOR_PROJECTION_FAST_WINDOW)
                    .min(deadline);
                if matches!(
                    tokio::time::timeout_at(
                        projection_deadline,
                        stop_owned_session_because(
                            state,
                            &route,
                            terminal,
                            terminal.control_reason(),
                        ),
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    if let Some(proof) = state
                        .media_sessions
                        .complete_terminal_projection(
                            &route,
                            MediaSessionProjectionCompletion::PredecessorAcknowledged,
                        )
                        .await
                        .filter(|proof| proof.terminal_projection_complete())
                    {
                        state.transcode.complete_session_release_durable(&proof);
                        return true;
                    }
                }
            }
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        tokio::time::sleep(PREDECESSOR_PROJECTION_RETRY_DELAY.min(remaining)).await;
    }
}

/// Same teardown with an honest reason: a client DELETE is a release, not a
/// supersession, and the VOD tombstone cause keys off this string.
pub(super) async fn stop_owned_session_because(
    state: &AppState,
    route: &MediaSessionRoute,
    terminal: crate::vodserve::Terminal,
    reason: &'static str,
) -> Result<(), PeerTransportError> {
    if route.owner_node_id == state.node_id {
        state
            .transcode
            .begin_session_terminal(&route.session_id, terminal, reason)
            .await;
        state.transcode.complete_session_release(&route.session_id);
        Ok(())
    } else {
        state
            .media_sessions
            .abort_remote(
                &route.owner_node_id,
                &RemoteAbortRequest {
                    incarnation_id: route.incarnation_id.clone(),
                    session_id: route.session_id.clone(),
                    expected_owner_epoch: route.owner_epoch,
                    reason: Some(reason.to_owned()),
                },
            )
            .await
    }
}

/// The real mapper, reachable from the accounting regressions in
/// `transcode.rs` so a genuinely returned refusal can be asserted end to end
/// rather than a hand-built string that already carries the class.
#[cfg(test)]
pub(crate) fn session_start_error_status_for_test(file_id: i64, error: String) -> StatusCode {
    session_start_error(file_id, error).into_response().status()
}

/// What the durable recipe says about this session's client teardown.
///
/// The recipe is the serialized create request, so this is the transport the
/// client named at create and nothing else — never the user agent, never a
/// guess from the playlist shape. A recipe an older node wrote carries no
/// transport at all and reads as conservative, which is the whole point: a
/// mixed-version cluster shortens nothing it cannot account for.
pub(super) fn exact_release_class(route: &MediaSessionRoute) -> crate::transcode::ReleaseClass {
    let transport =
        serde_json::from_str::<crate::media_sessions::RemoteStartRequest>(&route.recipe_json)
            .ok()
            .and_then(|recipe| recipe.request.transport);
    crate::transcode::ReleaseClass::from_transport(transport.as_deref())
}

pub(super) fn session_start_error(file_id: i64, error: String) -> ApiError {
    if error.contains("already used") {
        return ApiError::Conflict(error);
    }
    tracing::warn!(target: "plurxd::http::hls", file = file_id, "session create failed: {error}");
    // A sidecar this start needs is still being produced. Nothing is wrong and
    // nothing about the request should change — the only useful answer is
    // "ask again", which is what `startup_timeout` already means to every
    // client's create-retry ladder ("initialization media is not ready yet").
    // Before this it fell through to the generic arm as a codeless 503, so the
    // viewer got a terminal overlay for a file that was merely still loading.
    if crate::subtitles::is_sidecar_pending_error(&error) {
        return ApiError::TypedRetry {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "startup_timeout",
            message: error,
            retry_after_seconds: SIDECAR_PENDING_RETRY_AFTER_SECS,
        };
    }
    // This player's previous start has not let go yet — a wait, and a bounded
    // one, so name it. Left as a bare `{error}` sentence this was a codeless
    // 503, which every client correctly refuses to retry because a 503 nobody
    // explained is not a "still building" answer. The viewer got a terminal
    // overlay quoting an internal sentence, over a Retry button that re-posted
    // into the same contention with no backoff. With a code it joins
    // `create_503_not_yet` and the existing 1s/2s/4s ladder waits it out.
    //
    // Deliberately narrower than the whole capacity class. Sibling refusals in
    // that class are not all waits: one tells the client to ask for a smaller
    // height, and one reports a scratch ceiling above the configured budget
    // that no retry can satisfy. Those keep the codeless 503 nothing retries.
    if crate::transcode::is_replacement_wait_error(&error) {
        return ApiError::TypedRetry {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "transcode_capacity_pending",
            message: error,
            retry_after_seconds: crate::transcode::REPLACEMENT_WAIT_RETRY_AFTER_SECS,
        };
    }
    if crate::transcode::is_serving_fence_error(&error)
        || crate::transcode::is_start_infrastructure_error(&error)
        || crate::transcode::is_retryable_capacity_error(&error)
    {
        return ApiError::ServiceUnavailable(error);
    }
    if let Some(reason) = crate::transcode::invalid_reopen_reason(&error) {
        return ApiError::BadRequest(reason.to_owned());
    }
    if let Some((code, reason)) = crate::transcode::vod_refusal(&error) {
        let (status, code) = match code {
            "vod_disabled" => (StatusCode::SERVICE_UNAVAILABLE, "vod_disabled"),
            "vod_index_pending" => (StatusCode::SERVICE_UNAVAILABLE, "vod_index_pending"),
            "hevc_configuration_unverified" => {
                (StatusCode::CONFLICT, "hevc_configuration_unverified")
            }
            "hevc_configuration_unsupported" => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "hevc_configuration_unsupported",
            ),
            "vod_transcode_unavailable" => {
                (StatusCode::NOT_IMPLEMENTED, "vod_transcode_unavailable")
            }
            "vod_subtitle_burn_unavailable" => {
                (StatusCode::NOT_IMPLEMENTED, "vod_subtitle_burn_unavailable")
            }
            "vod_source_unsupported" => {
                (StatusCode::UNPROCESSABLE_ENTITY, "vod_source_unsupported")
            }
            "vod_video_geometry_unknown" => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "vod_video_geometry_unknown",
            ),
            "vod_frame_cadence_unknown" => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "vod_frame_cadence_unknown",
            ),
            "vod_source_rescan_required" => (StatusCode::CONFLICT, "vod_source_rescan_required"),
            "vod_engine_unattested" => (StatusCode::SERVICE_UNAVAILABLE, "vod_engine_unattested"),
            "vod_audio_track_missing" => {
                (StatusCode::UNPROCESSABLE_ENTITY, "vod_audio_track_missing")
            }
            "vod_subtitle_track_missing" => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "vod_subtitle_track_missing",
            ),
            "vod_invalid_height" => (StatusCode::BAD_REQUEST, "vod_invalid_height"),
            "vod_recipe_unresolved" => (StatusCode::INTERNAL_SERVER_ERROR, "vod_recipe_unresolved"),
            "vod_reopen_required" => (StatusCode::CONFLICT, "vod_reopen_required"),
            "live_presentation_removed" => (StatusCode::GONE, "live_presentation_removed"),
            _ => return ApiError::Internal(error),
        };
        return ApiError::typed(status, code, reason);
    }
    // A build that lacks a filter is not a server fault to be swallowed as
    // "internal server error" — it is a fact about this install that the
    // person pressing play can act on (pick a different subtitle track, or
    // tell whoever runs the box to install a full ffmpeg). 501, because the
    // request is well-formed and this server simply cannot implement it.
    if let Some(reason) = crate::transcode::unsupported_build_reason(&error) {
        return ApiError::typed(StatusCode::NOT_IMPLEMENTED, "unsupported_build", reason);
    }
    ApiError::Internal(error)
}

fn session_store_error(operation: &'static str, error: plurx_core::error::StoreError) -> ApiError {
    tracing::warn!(
        target: "plurxd::http::hls",
        %error, operation, "media-session Store operation failed"
    );
    ApiError::ServiceUnavailable(format!("{operation}: {error}"))
}
