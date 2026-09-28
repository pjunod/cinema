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
}

impl LiveRecoveryReason {
    pub(super) fn index(self) -> usize {
        match self {
            Self::RequestedLive => 0,
            Self::IndexPending => 1,
            Self::TranscodeUnavailable => 2,
            Self::SubtitleBurnUnavailable => 3,
            Self::SourceUnsupported => 4,
        }
    }

    pub(crate) const LABELS: [&'static str; 5] = [
        "requested_live",
        "vod_index_pending",
        "vod_transcode_unavailable",
        "vod_subtitle_burn_unavailable",
        "vod_source_unsupported",
    ];

    pub(super) fn label(self) -> &'static str {
        Self::LABELS[self.index()]
    }

    pub(super) fn from_refusal(code: &str) -> Option<Self> {
        match code {
            "vod_index_pending" => Some(Self::IndexPending),
            "vod_transcode_unavailable" => Some(Self::TranscodeUnavailable),
            "vod_subtitle_burn_unavailable" => Some(Self::SubtitleBurnUnavailable),
            "vod_source_unsupported" => Some(Self::SourceUnsupported),
            _ => None,
        }
    }
}

pub(super) fn record_live_recovery(reason: LiveRecoveryReason) {
    LIVE_RECOVERY_SESSIONS[reason.index()].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Sessions the retained engine served since start, by reason.
pub(crate) fn live_recovery_snapshot() -> [u64; 5] {
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
