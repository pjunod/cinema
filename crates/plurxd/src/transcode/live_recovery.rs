use super::*;

/// What made a session enter the retained engine rather than immutable VOD.
///
/// `RequestedLive` is the one worth watching. It is not a fallback decision at
/// all: it is a `SessionRequest` that arrived already naming the live
/// presentation, which today means the peer takeover path, whose recipe
/// validation requires it. That path consults no setting, so a node can serve
/// live-HLS sessions with the fallback switched off — visible here and
/// nowhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveRecoveryReason {
    RequestedLive,
    IndexPending,
    TranscodeUnavailable,
    SubtitleBurnUnavailable,
    SourceUnsupported,
    HevcConfigurationUnverified,
    HevcConfigurationUnsupported,
    FrameCadenceUnknown,
    VideoGeometryUnknown,
    VodDisabled,
    IndexUnavailable,
}

impl LiveRecoveryReason {
    pub(crate) const COUNT: usize = 11;

    pub(super) fn index(self) -> usize {
        match self {
            Self::RequestedLive => 0,
            Self::IndexPending => 1,
            Self::TranscodeUnavailable => 2,
            Self::SubtitleBurnUnavailable => 3,
            Self::SourceUnsupported => 4,
            Self::HevcConfigurationUnverified => 5,
            Self::HevcConfigurationUnsupported => 6,
            Self::FrameCadenceUnknown => 7,
            Self::VideoGeometryUnknown => 8,
            Self::VodDisabled => 9,
            Self::IndexUnavailable => 10,
        }
    }

    /// Each fallback reason is labelled with the refusal code that chose it.
    pub(crate) const LABELS: [&'static str; Self::COUNT] = [
        "requested_live",
        "vod_index_pending",
        "vod_transcode_unavailable",
        "vod_subtitle_burn_unavailable",
        "vod_source_unsupported",
        "hevc_configuration_unverified",
        "hevc_configuration_unsupported",
        "vod_frame_cadence_unknown",
        "vod_video_geometry_unknown",
        "vod_disabled",
        "vod_index_unavailable",
    ];

    pub(super) fn label(self) -> &'static str {
        Self::LABELS[self.index()]
    }

    /// Whether a VOD refusal may fall back to the retained engine, and the
    /// reason it is counted under. [`VOD_REFUSAL_CODES`] decides; a code it
    /// does not name never falls back (and fails
    /// `every_vod_refusal_code_is_classified`).
    pub(super) fn from_refusal(code: &str) -> Option<Self> {
        match vod_refusal_class(code)? {
            VodRefusalClass::LiveRecovery(reason) => Some(reason),
            VodRefusalClass::Refuse => None,
        }
    }
}

/// What a VOD refusal code means for playback.
///
/// The owner's rule (S-05, FFMPEG-SPAWN-UNIFICATION §5.3.1): plurx plays any
/// title through either immutable VOD or the retained rolling engine, so a
/// VOD prerequisite failing is not a reason to fail playback. Every code is
/// one of two things, and [`VOD_REFUSAL_CODES`] says which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VodRefusalClass {
    /// Only the immutable VOD presentation cannot build this; the rolling
    /// engine can play the title, so the start falls back to it (unless the
    /// request is VOD-only or the operator switched live recovery off).
    LiveRecovery(LiveRecoveryReason),
    /// No engine would serve this request as asked: the request itself is
    /// invalid or names a track/route/session that does not exist, the
    /// source must be rescanned first, or the request is a VOD-only service
    /// policy. Falling back would either fail the same way or serve
    /// something other than what was asked for. Also a VOD answer that is
    /// only "not yet" (`vod_engine_unattested`), which the client waits out
    /// and VOD then serves.
    Refuse,
}

/// Every code `vod_refusal_error` is called with, classified. A new code must
/// be added here: `every_vod_refusal_code_is_classified` walks the production
/// sources for every code passed to `vod_refusal_error` and fails on one this
/// table does not name.
pub(crate) const VOD_REFUSAL_CODES: &[(&str, VodRefusalClass)] = {
    use LiveRecoveryReason as R;
    use VodRefusalClass::{LiveRecovery as Live, Refuse};
    &[
        // ---- Only VOD cannot build this: fall back to rolling. ----
        ("vod_index_pending", Live(R::IndexPending)),
        // This node's indexer recorded that it cannot index this pipeline
        // (the failure is terminal); rolling plays it regardless.
        ("vod_index_unavailable", Live(R::IndexUnavailable)),
        ("vod_transcode_unavailable", Live(R::TranscodeUnavailable)),
        (
            "vod_subtitle_burn_unavailable",
            Live(R::SubtitleBurnUnavailable),
        ),
        ("vod_source_unsupported", Live(R::SourceUnsupported)),
        // A VOD copy deletes in-band HEVC parameter sets and so needs a proof
        // that doing so is safe; the rolling copy keeps them and needs none
        // (`hevc_rolling_copy_keeps_its_parameter_sets_instead_of_refusing`).
        (
            "hevc_configuration_unverified",
            Live(R::HevcConfigurationUnverified),
        ),
        (
            "hevc_configuration_unsupported",
            Live(R::HevcConfigurationUnsupported),
        ),
        // VOD needs an immutable frame grid and a fixed raster before the
        // first segment; the rolling encoder takes cadence and size from the
        // decoded stream.
        ("vod_frame_cadence_unknown", Live(R::FrameCadenceUnknown)),
        ("vod_video_geometry_unknown", Live(R::VideoGeometryUnknown)),
        // VOD switched off (`playback.vod_presentation=0`): rolling is the
        // only presentation left. `vod_disabled` still reaches a client when
        // the operator also switched live recovery off, and on a VOD-only
        // (compatibility) request.
        ("vod_disabled", Live(R::VodDisabled)),
        // ---- Not yet: a retryable 503 the client waits out. ----
        // The encoded engine (executable, dependency closure, font inputs)
        // could not be attested yet, typically in the first moments after a
        // restart. Every client's create-retry ladder waits it out
        // (`create_503_not_yet`, `START_NOT_YET_CODES`, the Jellyfin retry
        // delay), and a failed font attestation must refuse the burn rather
        // than render with other fonts (FONT-ATTESTATION-AND-BLOCKING-IO), so
        // falling back to the rolling engine here would be the wrong answer
        // twice over.
        ("vod_engine_unattested", Refuse),
        // ---- No engine would serve this request as asked: refuse. ----
        // Invalid or VOD-only service requests (passive retention, finite
        // bitrate ceilings, continuous-media families exist only in VOD).
        ("vod_invalid_height", Refuse),
        ("vod_passive_policy_invalid", Refuse),
        ("vod_passive_capacity", Refuse),
        ("vod_passive_route_expired", Refuse),
        ("vod_output_budget_refused", Refuse),
        ("vod_processed_contract_invalid", Refuse),
        ("vod_continuous_recipe_invalid", Refuse),
        ("vod_continuous_audio_invalid", Refuse),
        ("vod_family_invalid", Refuse),
        ("vod_family_capacity", Refuse),
        ("vod_family_catalog_missing", Refuse),
        // The selected track does not exist in the source.
        ("vod_audio_track_missing", Refuse),
        ("vod_subtitle_track_missing", Refuse),
        // The source no longer matches its scan; every engine plans from the
        // scan, so it must be rescanned first.
        ("vod_source_rescan_required", Refuse),
        // Session and candidate identity: the request names a session,
        // route, retained artifact or candidate that no longer exists or no
        // longer matches; the client must create again.
        ("vod_reopen_required", Refuse),
        // A preparation no longer proves custody of the exact parent/root.
        // Another engine cannot restore that proof; require a new create.
        ("vod_preparation_changed", Refuse),
        ("vod_session_ended", Refuse),
        ("vod_session_released", Refuse),
        ("live_presentation_removed", Refuse),
        ("retained_artifact_unavailable", Refuse),
        ("candidate_recipe_changed", Refuse),
        ("candidate_recipe_unavailable", Refuse),
        // VOD-internal planning and cache-head failures. Kept terminal, as
        // before, until each is shown to be VOD-only (S-05 follow-up): a
        // decoder plan or DV plan the rolling engine would make from the
        // same facts may fail the same way.
        ("vod_decoder_plan_refused", Refuse),
        ("vod_dv_planning_invalid", Refuse),
        ("vod_audio_recipe_invalid", Refuse),
        ("vod_recipe_unresolved", Refuse),
        ("vod_source_head_pending", Refuse),
        ("vod_head_regeneration_busy", Refuse),
        ("vod_head_regeneration_oversize", Refuse),
    ]
};

pub(crate) fn vod_refusal_class(code: &str) -> Option<VodRefusalClass> {
    VOD_REFUSAL_CODES
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, class)| *class)
}

pub(super) fn record_live_recovery(reason: LiveRecoveryReason) {
    LIVE_RECOVERY_SESSIONS[reason.index()].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Sessions the retained engine served since start, by reason.
pub(crate) fn live_recovery_snapshot() -> [u64; LiveRecoveryReason::COUNT] {
    std::array::from_fn(|index| {
        LIVE_RECOVERY_SESSIONS[index].load(std::sync::atomic::Ordering::Relaxed)
    })
}

pub(crate) fn live_recovery_prometheus() -> String {
    let counts = live_recovery_snapshot();
    let mut output = String::from(
        "# HELP plurx_live_hls_recovery_sessions_total Sessions served by the retained live-HLS engine, by why it was chosen.\n\
         # TYPE plurx_live_hls_recovery_sessions_total counter\n",
    );
    for (index, label) in LiveRecoveryReason::LABELS.iter().enumerate() {
        output.push_str(&format!(
            "plurx_live_hls_recovery_sessions_total{{reason=\"{label}\"}} {}\n",
            counts[index]
        ));
    }
    output
}
