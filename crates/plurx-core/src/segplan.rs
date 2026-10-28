//! The segment plan — the single source of truth for a VOD rendition.
//!
//! [`VOD-PRESENTATION-PLAN.md`][plan] §2.2 makes this plan **normative, not
//! predictive** (ledger D10): a materializing producer cuts *at* the
//! boundaries computed here and never re-decides them. So nothing in this
//! module tries to reproduce what today's producer-driven policy would have
//! chosen. It computes boundaries that are provably clean, floor- and
//! ceiling-respecting, and stable for the life of the file.
//!
//! [plan]: ../../../docs/streaming/VOD-PRESENTATION-PLAN.md
//!
//! Three things live here, all pure:
//!
//! - the **fragment index**, one row per fragment of the production-shaped
//!   video-only copy pipe, which the daemon builds and persists;
//! - the **plan**, [`CutPolicy`] applied over that index plus the audio tail
//!   the probe's per-track durations imply;
//! - the **landing matcher**, which tells a repositioned producer where it
//!   actually landed.
//!
//! The landing matcher is the part with a scar attached. M0-P0 measured 43
//! repositions across 9 fixtures and found that a repositioned copy generation
//! emits no film time at all: `-avoid_negative_ts make_zero` rebases every
//! generation to zero, `-copyts` does not change it, and seeking to two
//! different boundaries produces the *same* timestamps. So the plan's original
//! "discard fragments until the first whose DTS equals the boundary" could
//! never converge. What does converge — 43 of 43, uniquely — is matching the
//! landing by **output byte count**, which the index already stores. See
//! plan §12.3 and §12.9 ruling A1. Where byte counts cannot decide — a run of
//! fragments that all weigh the same — the landing is placed by the source
//! time the demuxer measurably landed at ([`match_landing_at`]), and the
//! bytes must still agree.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::fmp4::{CutClass, CutPolicy, CutReason, PromotionInputs};

/// Bumped when a stored index or plan stops being readable by this binary.
/// A persisted row from a newer version is discarded and rebuilt rather than
/// misinterpreted — an index is always cheaper to rebuild than to get wrong.
///
/// v2 added `IndexRow::video_bytes`. v1 rows stored only the wire length of
/// the video-only pipe, which a production generation never reproduces, so
/// every v1 index is discarded rather than matched against.
///
/// v3 added `promotion` and `parameter_sets_constant`. A v2 index cannot
/// answer whether the film's parameter sets are constant, and defaulting that
/// to `true` on a stored row would VOD-present a title that has never been
/// checked — so v2 rows are rebuilt rather than read.
pub const SEGPLAN_VERSION: u32 = 3;

/// Proof revision for the storage-only checks a fragment-index badge cannot
/// perform without materialising the packed index.
///
/// This is deliberately separate from [`SEGPLAN_VERSION`]. The segment-plan
/// version describes what the rows mean; this marker describes which
/// publication-time validation contract proved the stored representation.
/// Bump it whenever `ROW_BYTES`, promotion decoding, or `get`'s structural
/// refusal rules change.
pub const VALIDATION_REVISION: u32 = 1;

/// How many consecutive fragments a landing match compares at least.
///
/// One fragment's byte count is not enough: two fragments of a 90-minute film
/// can plausibly weigh the same, and a confidently wrong landing puts the
/// viewer in the wrong part of the film with no error anywhere. Three is the
/// smallest window that makes a collision require three consecutive
/// coincidences, and it costs the producer nothing — it reads the fragments it
/// was going to read anyway.
///
/// It is a minimum, not the whole probe. Three equal counts are unremarkable
/// in low-entropy film — black or static scenes, slides, a test pattern — so
/// when three leave more than one possible landing, the producer keeps reading
/// and [`match_landing`] compares every fragment it has seen.
pub const LANDING_WINDOW: usize = 3;

/// Audio headroom over the pipe's *nominal* bitrate, as a fraction.
///
/// Nominal is a request, not a ceiling: M0 measured ffmpeg's AAC encoder
/// overshooting `-b:a 320k` by 1.2235× on the 5.1 branch (plan §12.4). 1.5×
/// sits above the measured worst case with room, and oversizing is safe
/// because `est_bytes` feeds admission — deciding whether a rendition may be
/// cached — and never a refusal.
pub const AUDIO_HEADROOM_NUM: u64 = 3;
pub const AUDIO_HEADROOM_DEN: u64 = 2;

/// The transcode path's fixed grid, in milliseconds.
///
/// The encoder is already told to cut here (`-force_key_frames
/// expr:gte(t,n_forced*2)` + `-hls_time 2`), so the plan describes what the
/// pipeline does rather than asking it to change.
pub const TRANSCODE_GRID_MS: i64 = 2_000;

// ---------------------------------------------------------------------------
// identity
// ---------------------------------------------------------------------------

/// What an index is keyed by.
///
/// The file's own identity is not sufficient. A Dolby Vision preservation
/// path, a different bitstream filter, or an ffmpeg upgrade all change the
/// *video* bytes the pipe emits while leaving size and mtime untouched — and
/// every byte count in the index, including the ones the landing matcher
/// compares, would then be silently wrong. So the video branch's argument
/// vector is fingerprinted into the key (plan §2.2, ruling A1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub size: u64,
    pub mtime_ms: i64,
    /// A stable digest of the video-branch argv — see [`argv_fingerprint`].
    pub argv_fingerprint: String,
}

impl SourceIdentity {
    pub fn new(size: u64, mtime_ms: i64, argv_fingerprint: impl Into<String>) -> SourceIdentity {
        SourceIdentity {
            size,
            mtime_ms,
            argv_fingerprint: argv_fingerprint.into(),
        }
    }

    /// True when a persisted index may still be used for this source.
    pub fn matches(&self, other: &SourceIdentity) -> bool {
        self == other
    }
}

/// Why one (file, pipeline) has no fragment index.
///
/// The two outcomes an index build can reach short of success, and they mean
/// opposite things about trying again. `Unsupported` is a property of the
/// bytes: this file cannot be indexed by this pipeline, and nothing short of
/// the file changing will alter that. `Truncated` is a property of the
/// *attempt* — the per-file budget is a wall-clock guess and a slow mount can
/// pass on a quiet day — so it is worth another try later, and worth recording
/// how far it got, because "truncated at 3,900 rows" and "truncated at 12" are
/// different findings about the same file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexRefusal {
    /// The build ran out of budget with this many rows in hand.
    Truncated { rows: u32 },
    /// The build cannot succeed for this source.
    Unsupported,
}

impl IndexRefusal {
    /// The stored spelling, shared with the cluster queue's `last_error_code`
    /// so an operator greps one word across both layers.
    pub const fn code(self) -> &'static str {
        match self {
            IndexRefusal::Truncated { .. } => "truncated",
            IndexRefusal::Unsupported => "unsupported",
        }
    }

    /// Whether another attempt can ever change the answer.
    pub const fn is_retryable(self) -> bool {
        matches!(self, IndexRefusal::Truncated { .. })
    }
}

/// One recorded refusal, and when the indexer may try again.
///
/// Kept for the same reason an index is: so the next pass does not repeat a
/// whole-file read that has already answered. Invalidated the same way too —
/// by identity mismatch, never by deletion — so a file the operator replaces,
/// or a pipeline whose argv changes, is simply eligible again with nothing
/// having to notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentIndexOutcome {
    pub source: SourceIdentity,
    pub refusal: IndexRefusal,
    /// The builder's own words, for the operator surface.
    pub reason: String,
    /// How many times this identity has been refused, including this one.
    pub attempts: u32,
    /// Unix ms before which the indexer must not spend another whole-file read
    /// on this identity. [`i64::MAX`] for a terminal refusal.
    pub next_attempt_at_ms: i64,
    pub updated_at_ms: i64,
    pub typed_code: Option<crate::content_analysis::IndexFailureCode>,
    pub typed_retryable: Option<bool>,
    pub retry_deadline_ms: i64,
    pub policy_revision: u32,
    pub terminal_reason: Option<String>,
    pub diagnostic: Option<crate::content_analysis::IndexDiagnostic>,
}

/// What a recorded refusal means for the indexer, now.
///
/// Exactly one of three. Before, a retryable row past its retry deadline was
/// neither due nor terminal: the indexer skipped it forever while VOD said
/// "pending" for it forever (lab5: 83 titles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOutcomeState {
    /// The indexer may spend a pass on this identity now.
    Due,
    /// Not before this Unix ms.
    WaitUntil(i64),
    /// Waiting will not change the answer; only a changed source or a newer
    /// failure policy will.
    Terminal,
}

impl FragmentIndexOutcome {
    /// The state this refusal is in at `now_ms`.
    ///
    /// The typed disposition is what the indexer decided, and the legacy
    /// `refusal` answers only for rows recorded before there was one: the
    /// legacy field carries `Truncated` for every terminal code that is not
    /// literally `Unsupported`.
    ///
    /// A budget failure recorded under an older failure policy is due once:
    /// [`crate::content_analysis::INDEX_FAILURE_POLICY_REVISION`] 2 budgets a
    /// retry from what the failed attempt measured, which is the change those
    /// rows were waiting for. Only while the row still says so: an untyped
    /// record (`record_outcome`) replaces the typed disposition, and its legacy
    /// refusal is checked too.
    ///
    /// The retry deadline alone never makes a row terminal. A retryable row
    /// past it is due: the window closes when an attempt after the deadline
    /// is recorded (`index_retry_decision` answers `IndexRetryWindowExpired`),
    /// so an indexer starved past the deadline still makes the attempt the
    /// window promised instead of declaring the title unindexable unread.
    pub fn state(&self, now_ms: i64) -> IndexOutcomeState {
        if self.typed_code == Some(crate::content_analysis::IndexFailureCode::IndexBudgetExceeded)
            && matches!(self.refusal, IndexRefusal::Truncated { .. })
            && self.policy_revision < crate::content_analysis::INDEX_FAILURE_POLICY_REVISION
        {
            return IndexOutcomeState::Due;
        }
        let retryable = self
            .typed_retryable
            .unwrap_or_else(|| self.refusal.is_retryable());
        if !retryable || self.next_attempt_at_ms == i64::MAX {
            return IndexOutcomeState::Terminal;
        }
        if now_ms >= self.next_attempt_at_ms {
            IndexOutcomeState::Due
        } else {
            IndexOutcomeState::WaitUntil(self.next_attempt_at_ms)
        }
    }

    /// Whether the indexer may spend another pass on this identity.
    pub fn is_due(&self, now_ms: i64) -> bool {
        self.state(now_ms) == IndexOutcomeState::Due
    }

    /// Whether waiting will change the answer.
    pub fn is_terminal(&self, now_ms: i64) -> bool {
        self.state(now_ms) == IndexOutcomeState::Terminal
    }
}

// ---------------------------------------------------------------------------
// timeline annotations
// ---------------------------------------------------------------------------

/// Semantic regions of a media timeline that a client may offer to skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationKind {
    Intro,
    Recap,
    Credits,
    Preview,
}

impl AnnotationKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "intro" => Some(Self::Intro),
            "recap" => Some(Self::Recap),
            "credits" => Some(Self::Credits),
            "preview" => Some(Self::Preview),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intro => "intro",
            Self::Recap => "recap",
            Self::Credits => "credits",
            Self::Preview => "preview",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Intro => "Skip Intro",
            Self::Recap => "Skip Recap",
            Self::Credits => "Skip Credits",
            Self::Preview => "Skip Preview",
        }
    }
}

/// Why an annotation exists. The order is the conflict-resolution rank used
/// when overlapping annotations of one kind are normalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationProvenance {
    Estimated,
    Detected,
    Authored,
    Manual,
}

impl AnnotationProvenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Estimated => "estimated",
            Self::Detected => "detected",
            Self::Authored => "authored",
            Self::Manual => "manual",
        }
    }
}

/// One validated semantic boundary.
///
/// Ticks are authoritative. Milliseconds are stored as a convenience for the
/// shipped clients and must be the exact integer projection of those ticks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineAnnotation {
    pub kind: AnnotationKind,
    pub start_ticks: i64,
    pub end_ticks: i64,
    pub timescale: u32,
    pub start_ms: i64,
    pub end_ms: i64,
    pub provenance: AnnotationProvenance,
    pub confidence_millis: u16,
    pub detector_version: String,
    pub manual_override_revision: Option<u64>,
}

/// The replicated semantic component of one content-analysis generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineAnnotationSet {
    pub source_identity: SourceIdentity,
    pub generation_id: String,
    pub version: u32,
    pub annotations: Vec<TimelineAnnotation>,
}

pub const MAX_TIMELINE_ANNOTATIONS: usize = 32;
pub const MAX_TIMELINE_IDENTIFIER_BYTES: usize = 128;
pub const MAX_TIMELINE_ANNOTATIONS_JSON_BYTES: usize = 64 * 1_024;

impl TimelineAnnotationSet {
    /// Validate replicated values and merge overlapping annotations of the
    /// same kind. This is deliberately called by each backend at the write
    /// boundary rather than trusting every producer to remember the rules.
    pub fn validate_and_normalize(mut self, duration_ms: i64) -> Result<Self, String> {
        if duration_ms <= 0 {
            return Err("timeline annotations require a positive duration".to_owned());
        }
        if self.generation_id.trim().is_empty() {
            return Err("timeline annotation generation_id is empty".to_owned());
        }
        if self.generation_id.len() > MAX_TIMELINE_IDENTIFIER_BYTES {
            return Err("timeline annotation generation_id is too long".to_owned());
        }
        if self.source_identity.argv_fingerprint.trim().is_empty()
            || self.source_identity.argv_fingerprint.len() > MAX_TIMELINE_IDENTIFIER_BYTES
        {
            return Err("timeline annotation source fingerprint is invalid".to_owned());
        }
        if self.version == 0 {
            return Err("timeline annotation version must be positive".to_owned());
        }
        if self.annotations.len() > MAX_TIMELINE_ANNOTATIONS {
            return Err(format!(
                "timeline annotation count exceeds {MAX_TIMELINE_ANNOTATIONS}"
            ));
        }
        for annotation in &self.annotations {
            validate_annotation(annotation, duration_ms)?;
        }

        self.annotations.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| {
                    compare_ticks(
                        left.start_ticks,
                        left.timescale,
                        right.start_ticks,
                        right.timescale,
                    )
                })
                .then_with(|| {
                    compare_ticks(
                        left.end_ticks,
                        left.timescale,
                        right.end_ticks,
                        right.timescale,
                    )
                })
        });
        let mut normalized: Vec<TimelineAnnotation> = Vec::with_capacity(self.annotations.len());
        for annotation in std::mem::take(&mut self.annotations) {
            let Some(previous) = normalized.last_mut() else {
                normalized.push(annotation);
                continue;
            };
            if previous.kind != annotation.kind
                || compare_ticks(
                    previous.end_ticks,
                    previous.timescale,
                    annotation.start_ticks,
                    annotation.timescale,
                ) != std::cmp::Ordering::Greater
            {
                normalized.push(annotation);
                continue;
            }

            let same_evidence = previous.provenance == annotation.provenance
                && previous.confidence_millis == annotation.confidence_millis
                && previous.detector_version == annotation.detector_version
                && previous.manual_override_revision == annotation.manual_override_revision;
            if same_evidence {
                let timescale = common_timescale(previous.timescale, annotation.timescale)
                    .ok_or_else(|| "timeline annotation timescales cannot be merged".to_owned())?;
                let previous_factor = i64::from(timescale / previous.timescale);
                let annotation_factor = i64::from(timescale / annotation.timescale);
                let previous_start = previous
                    .start_ticks
                    .checked_mul(previous_factor)
                    .ok_or_else(|| "timeline annotation start tick overflow".to_owned())?;
                let previous_end = previous
                    .end_ticks
                    .checked_mul(previous_factor)
                    .ok_or_else(|| "timeline annotation end tick overflow".to_owned())?;
                let annotation_start = annotation
                    .start_ticks
                    .checked_mul(annotation_factor)
                    .ok_or_else(|| "timeline annotation start tick overflow".to_owned())?;
                let annotation_end = annotation
                    .end_ticks
                    .checked_mul(annotation_factor)
                    .ok_or_else(|| "timeline annotation end tick overflow".to_owned())?;
                previous.timescale = timescale;
                previous.start_ticks = previous_start.min(annotation_start);
                previous.end_ticks = previous_end.max(annotation_end);
                previous.start_ms = ticks_to_millis(previous.start_ticks, timescale);
                previous.end_ms = ticks_to_millis(previous.end_ticks, timescale);
            } else if annotation.provenance > previous.provenance
                || (annotation.provenance == previous.provenance
                    && annotation.confidence_millis > previous.confidence_millis)
            {
                *previous = annotation;
            }
        }
        self.annotations = normalized;
        for annotation in &self.annotations {
            validate_annotation(annotation, duration_ms)?;
        }
        Ok(self)
    }
}

fn compare_ticks(
    left_ticks: i64,
    left_timescale: u32,
    right_ticks: i64,
    right_timescale: u32,
) -> std::cmp::Ordering {
    (i128::from(left_ticks) * i128::from(right_timescale))
        .cmp(&(i128::from(right_ticks) * i128::from(left_timescale)))
}

fn common_timescale(left: u32, right: u32) -> Option<u32> {
    fn gcd(mut left: u32, mut right: u32) -> u32 {
        while right != 0 {
            (left, right) = (right, left % right);
        }
        left
    }

    left.checked_div(gcd(left, right))?.checked_mul(right)
}

fn ticks_to_millis(ticks: i64, timescale: u32) -> i64 {
    ticks
        .saturating_mul(1_000)
        .saturating_div(i64::from(timescale))
}

fn validate_annotation(annotation: &TimelineAnnotation, duration_ms: i64) -> Result<(), String> {
    if annotation.timescale == 0 {
        return Err("timeline annotation timescale must be positive".to_owned());
    }
    if annotation.start_ticks < 0 || annotation.start_ticks >= annotation.end_ticks {
        return Err("timeline annotation must have start_ticks < end_ticks".to_owned());
    }
    if annotation.start_ms < 0
        || annotation.start_ms >= annotation.end_ms
        || annotation.end_ms > duration_ms
    {
        return Err(format!(
            "timeline annotation must satisfy 0 <= start < end <= {duration_ms}ms"
        ));
    }
    if ticks_to_millis(annotation.start_ticks, annotation.timescale) != annotation.start_ms
        || ticks_to_millis(annotation.end_ticks, annotation.timescale) != annotation.end_ms
    {
        return Err("timeline annotation milliseconds do not match its authoritative ticks".into());
    }
    if annotation.confidence_millis > 1_000 {
        return Err("timeline annotation confidence must be in 0..=1000".to_owned());
    }
    if annotation.detector_version.trim().is_empty() {
        return Err("timeline annotation detector_version is empty".to_owned());
    }
    if annotation.detector_version.len() > MAX_TIMELINE_IDENTIFIER_BYTES {
        return Err("timeline annotation detector_version is too long".to_owned());
    }
    match (annotation.provenance, annotation.manual_override_revision) {
        (AnnotationProvenance::Manual, Some(revision)) if revision > 0 => {}
        (AnnotationProvenance::Manual, _) => {
            return Err("manual annotation requires a positive override revision".to_owned())
        }
        (_, Some(_)) => {
            return Err("only a manual annotation may carry an override revision".to_owned())
        }
        _ => {}
    }
    Ok(())
}

/// A stable fingerprint of the arguments that decide the video bytes.
///
/// Deliberately not a hash of the whole argv: the audio branch, pacing, and
/// the output path change between runs without changing a single video
/// fragment, and an index invalidated by those would be rebuilt constantly for
/// nothing. Callers pass only the video-deciding arguments.
pub fn argv_fingerprint(video_args: &[String]) -> String {
    // FNV-1a over the joined arguments. Not cryptographic and not required to
    // be: this detects a changed pipeline, it does not defend against one.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for arg in video_args {
        for byte in arg.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0x1f;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

// ---------------------------------------------------------------------------
// the index
// ---------------------------------------------------------------------------

/// One fragment of the production-shaped video-only copy pipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexRow {
    /// First-sample decode time of the video track, in index ticks.
    pub dts: u64,
    /// The video track's sample durations, summed.
    pub duration: u64,
    /// Output bytes on the wire — `moof` + `mdat`, exactly what
    /// [`crate::fmp4::Fragment::len`] counts and what the segmenter's byte
    /// ceiling accumulates. **Not** what the landing matcher compares: this is
    /// the VIDEO-ONLY pipe's wire length, and a production generation carries
    /// audio, so its `moof` and `mdat` are tens of kilobytes larger and vary
    /// with the audio track.
    pub bytes: u32,
    /// The video track's sample sizes, summed — the payload bytes of the video
    /// `traf`'s `trun` entries, with no container overhead at all.
    ///
    /// This is what the landing matcher compares, and it is the only quantity
    /// here that survives the trip: video samples are copied, so a production
    /// generation emits byte-for-byte the same ones. M0 measured the
    /// difference and it is total — over four fixtures, the video-only pipe's
    /// wire length matched the production pipe's on 0 of 28 fragments while
    /// this sum matched on 28 of 28.
    pub video_bytes: u32,
    /// [`CutClass`] stored by label, because a segment may only begin in front
    /// of a clean fragment and the policy asks the real type.
    #[serde(with = "cut_class_serde")]
    pub class: CutClass,
}

impl IndexRow {
    pub fn clean(&self) -> bool {
        self.class.is_clean()
    }
}

mod cut_class_serde {
    use super::CutClass;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &CutClass, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(value.label())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<CutClass, D::Error> {
        let label = String::deserialize(de)?;
        match label.as_str() {
            "idr" => Ok(CutClass::CleanIdr),
            "cra" => Ok(CutClass::CleanCra),
            "dirty" => Ok(CutClass::Dirty),
            "unparseable" => Ok(CutClass::Unparseable),
            other => Err(serde::de::Error::custom(format!(
                "unknown cut class {other:?}"
            ))),
        }
    }
}

/// Every fact the plan needs about a file, computed once and persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FragmentIndex {
    pub version: u32,
    /// The video track's timescale in the pipe's own `moov`.
    pub timescale: u32,
    pub rows: Vec<IndexRow>,
    /// SHA-256 of the initialization segment this index was built against.
    /// A generation whose init differs is refused (plan §2.2).
    ///
    /// This is the **muxer** init — `Unit::Init`, ffmpeg's raw `ftyp`+`moov`,
    /// which is the byte string M0-P0 clause (d) proved stable across
    /// generations including a seeked one. It is not what a viewer receives;
    /// see [`FragmentIndex::promotion`].
    pub init_sha256: String,
    /// What promotion must copy into every generation's init, captured once
    /// from the film's opening clean fragment.
    ///
    /// The served `init.mp4` is the muxer init plus this. Capturing it here
    /// rather than reading it from whichever fragment a generation landed on
    /// is what makes the served init byte-identical across generations — see
    /// [`plurx_core::fmp4::PromotionInputs`] for the collision this dissolves.
    pub promotion: PromotionInputs,
    /// False when the film's clean fragments do not all carry the same
    /// parameter sets.
    ///
    /// A single immutable init genuinely cannot describe such a film, so it is
    /// not VOD-presentable and keeps the legacy presentation. Deciding that
    /// here — at index time, in the background, before any viewer exists — is
    /// the difference between a scan-time verdict and a `producer_failed` in
    /// the middle of someone's playback.
    pub parameter_sets_constant: bool,
    pub source: SourceIdentity,
}

impl FragmentIndex {
    pub fn new(
        timescale: u32,
        rows: Vec<IndexRow>,
        init_sha256: impl Into<String>,
        source: SourceIdentity,
    ) -> FragmentIndex {
        FragmentIndex {
            version: SEGPLAN_VERSION,
            timescale: timescale.max(1),
            rows,
            promotion: PromotionInputs::default(),
            // Vacuously true until an indexer says otherwise: a film whose
            // clean fragments were never compared has not been shown to vary.
            // The indexer sets this from what it actually walked.
            parameter_sets_constant: true,
            init_sha256: init_sha256.into(),
            source,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Total video ticks the index covers.
    pub fn ticks(&self) -> u64 {
        self.rows.iter().map(|row| row.duration).sum()
    }

    /// Usable only when the stored version matches and the source is unchanged.
    pub fn usable_for(&self, source: &SourceIdentity) -> bool {
        self.version == SEGPLAN_VERSION && self.source.matches(source) && !self.rows.is_empty()
    }
}

// ---------------------------------------------------------------------------
// the plan
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlanEntryKind {
    Video,
    /// Audio that outlives the video. `frag_keyframe` has no video keyframe
    /// left to cut on, so the production pipe emits the whole tail inside its
    /// last fragment; the plan splits it at the policy ceiling the way
    /// [`crate::fmp4::Segmenter::finish`] does (the `c58a4307` rule).
    AudioTail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlanCut {
    Clean,
    ByteCeiling,
    TimeCeiling,
    EndOfStream,
}

impl From<CutReason> for PlanCut {
    fn from(reason: CutReason) -> PlanCut {
        match reason {
            CutReason::Clean => PlanCut::Clean,
            CutReason::ByteCeiling => PlanCut::ByteCeiling,
            CutReason::TimeCeiling => PlanCut::TimeCeiling,
            CutReason::EndOfStream => PlanCut::EndOfStream,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub index: u32,
    pub kind: PlanEntryKind,
    /// Film time, in plan ticks. The materializing segmenter rebases the
    /// published segment's `tfdt` to exactly this (plan §2.2).
    pub start_ticks: u64,
    pub duration_ticks: u64,
    /// Planned size: video bytes from the index plus the audio headroom. Feeds
    /// admission, never a refusal, which is why it is deliberately generous.
    pub est_bytes: u64,
    /// Why the entry ends where it does. Kept because a `Clean` boundary is a
    /// valid reposition target for a decoder and a ceiling boundary is not.
    pub cut: PlanCut,
    /// How many index fragments this entry covers. Zero for an audio tail,
    /// which has no video fragments of its own.
    pub fragments: u32,
}

impl PlanEntry {
    pub fn end_ticks(&self) -> u64 {
        self.start_ticks + self.duration_ticks
    }

    pub fn seconds(&self, timescale: u32) -> f64 {
        self.duration_ticks as f64 / f64::from(timescale.max(1))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentPlan {
    pub version: u32,
    pub timescale: u32,
    pub entries: Vec<PlanEntry>,
    /// `#EXT-X-TARGETDURATION`, computed over **every** entry, audio-tail
    /// entries included. M0's `audiotail-2397` fixture is the reason it is
    /// spelled out: its honest target duration is 15 s where a video-only plan
    /// emits 8, and an understated target duration is a spec violation players
    /// act on (plan §12.4).
    pub target_duration: u32,
}

impl SegmentPlan {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn entry(&self, index: u32) -> Option<&PlanEntry> {
        self.entries.get(index as usize)
    }

    /// Planned total size — what admission compares against the cache budget.
    pub fn planned_bytes(&self) -> u64 {
        self.entries.iter().map(|entry| entry.est_bytes).sum()
    }

    pub fn duration_ticks(&self) -> u64 {
        self.entries.last().map(PlanEntry::end_ticks).unwrap_or(0)
    }

    /// The whole rendition as one HLS playlist, complete from the first fetch.
    ///
    /// This is the artifact the rest of this module exists to produce, and the
    /// difference between it and what the daemon serves today is the whole
    /// point of the VOD presentation: a live playlist grows, so a client can
    /// only ever see as far as the server has produced, and every seek past
    /// that is a seek into a timeline the player does not believe exists.
    /// This one lists every segment of the film before a single byte of media
    /// has been made.
    ///
    /// Three tags carry that claim, and all three are load-bearing:
    ///
    /// - `EXT-X-PLAYLIST-TYPE:VOD`, not `EVENT`. `EVENT` promises only that
    ///   segments are appended and never removed, which is what a growing
    ///   playlist can honestly say; `VOD` promises the playlist is *final*,
    ///   which is what makes the whole duration seekable.
    /// - `EXT-X-ENDLIST`, for the same reason. Without it a player treats the
    ///   end as provisional and keeps reloading.
    /// - `EXT-X-TARGETDURATION` over **every** entry, audio tails included.
    ///   M0's `audiotail-2397` fixture is why that is spelled out: its honest
    ///   target duration is 15 s where a video-only plan emits 8, and an
    ///   understated target duration is a spec violation players act on.
    ///
    /// Durations are the plan's own, in the plan's timescale — *nominal*, per
    /// ledger D6. M0 measured hls.js ignoring declared `EXTINF` values in
    /// favour of measured PTS, so a playlist that tried to be exact would be
    /// spending precision nothing reads, on numbers the segmenter is not
    /// obliged to reproduce to the microsecond.
    ///
    /// No `EXT-X-INDEPENDENT-SEGMENTS`, for [`crate::fmp4::playlist_header`]'s
    /// reason: a ceiling cut makes the claim a lie, and a lie in a spec tag is
    /// what this path exists to stop shipping.
    ///
    /// One known residual, carried deliberately: a planned audio-tail entry's
    /// `EXTINF` and the segment actually emitted for it can differ by up to
    /// one audio frame — roughly 21 to 32 ms — because the segmenter splits on
    /// whole frames and the plan splits on ticks. That is well inside the
    /// rounding the format allows, and nominal durations are what players use
    /// anyway.
    pub fn playlist(&self) -> String {
        use std::fmt::Write;
        let mut out = String::with_capacity(64 + self.entries.len() * 32);
        out.push_str("#EXTM3U\n#EXT-X-VERSION:7\n");
        let _ = writeln!(out, "#EXT-X-TARGETDURATION:{}", self.target_duration.max(1));
        out.push_str("#EXT-X-MEDIA-SEQUENCE:0\n");
        out.push_str("#EXT-X-PLAYLIST-TYPE:VOD\n");
        out.push_str("#EXT-X-MAP:URI=\"init.mp4\"\n");
        for entry in &self.entries {
            let _ = writeln!(
                out,
                "#EXTINF:{:.6},\n{}",
                entry.seconds(self.timescale),
                crate::fmp4::segment_name(u64::from(entry.index))
            );
        }
        out.push_str("#EXT-X-ENDLIST\n");
        out
    }
}

/// Per-track facts the probe already knows, needed for the audio tail and the
/// byte headroom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackDurations {
    pub video_ms: i64,
    pub audio_ms: i64,
    /// The bitrate the production pipe *asks* for on this file's audio branch
    /// (`-b:a`), or the source's declared rate when audio is copied.
    pub audio_bits_per_second: u32,
}

impl TrackDurations {
    /// Bytes of audio to reserve for `ticks` of film, with headroom.
    fn headroom(&self, ticks: u64, timescale: u32) -> u64 {
        let per_second = u64::from(self.audio_bits_per_second) / 8;
        let bytes = per_second.saturating_mul(ticks) / u64::from(timescale.max(1));
        bytes.saturating_mul(AUDIO_HEADROOM_NUM) / AUDIO_HEADROOM_DEN
    }
}

/// Build the copy path's plan: [`CutPolicy`] over the index's clean
/// boundaries, then the audio tail.
///
/// The policy is asked with **video bytes plus the audio headroom for the
/// pending duration**, not with video bytes alone, because the segment the
/// ceiling is meant to bound is the one that really ships — and that one
/// carries audio.
pub fn plan_copy(
    index: &FragmentIndex,
    policy: &CutPolicy,
    tracks: &TrackDurations,
) -> SegmentPlan {
    let timescale = index.timescale.max(1);
    let mut entries: Vec<PlanEntry> = Vec::new();
    let mut pending_ticks: u64 = 0;
    let mut pending_bytes: u64 = 0;
    let mut pending_fragments: u32 = 0;
    let mut pending_start: Option<u64> = None;

    for row in &index.rows {
        // Decide in front of the arriving fragment, exactly as
        // `Segmenter::push` does: the fragment being examined becomes the
        // FIRST of the next segment, never the last of this one.
        let with_headroom = pending_bytes.saturating_add(tracks.headroom(pending_ticks, timescale));
        let reason = policy.cut_before(
            pending_ticks,
            usize::try_from(with_headroom).unwrap_or(usize::MAX),
            row.class,
            entries.is_empty(),
        );
        if let Some(reason) = reason {
            entries.push(PlanEntry {
                index: entries.len() as u32,
                kind: PlanEntryKind::Video,
                start_ticks: pending_start.unwrap_or(0),
                duration_ticks: pending_ticks,
                est_bytes: with_headroom,
                cut: reason.into(),
                fragments: pending_fragments,
            });
            pending_ticks = 0;
            pending_bytes = 0;
            pending_fragments = 0;
            pending_start = None;
        }
        if pending_start.is_none() {
            pending_start = Some(row.dts);
        }
        pending_ticks += row.duration;
        pending_bytes += u64::from(row.bytes);
        pending_fragments += 1;
    }

    if pending_fragments > 0 {
        let with_headroom = pending_bytes.saturating_add(tracks.headroom(pending_ticks, timescale));
        entries.push(PlanEntry {
            index: entries.len() as u32,
            kind: PlanEntryKind::Video,
            start_ticks: pending_start.unwrap_or(0),
            duration_ticks: pending_ticks,
            est_bytes: with_headroom,
            cut: PlanCut::EndOfStream,
            fragments: pending_fragments,
        });
    }

    append_audio_tail(&mut entries, tracks, policy, timescale);
    finish(entries, timescale)
}

/// The transcode path's plan: a fixed 2 s grid, last entry the remainder.
///
/// No index is involved — the encoder is instructed to cut exactly here, so
/// the boundaries are known from the duration alone.
pub fn plan_transcode(duration_ms: i64, timescale: u32, tracks: &TrackDurations) -> SegmentPlan {
    let timescale = timescale.max(1);
    let duration_ms = duration_ms.max(0);
    let mut entries = Vec::new();
    let mut start_ms: i64 = 0;
    while start_ms < duration_ms {
        let span = TRANSCODE_GRID_MS.min(duration_ms - start_ms);
        let start_ticks = ms_to_ticks(start_ms, timescale);
        let end_ticks = ms_to_ticks(start_ms + span, timescale);
        entries.push(PlanEntry {
            index: entries.len() as u32,
            kind: PlanEntryKind::Video,
            start_ticks,
            duration_ticks: end_ticks - start_ticks,
            est_bytes: tracks.headroom(end_ticks - start_ticks, timescale),
            cut: if start_ms + span >= duration_ms {
                PlanCut::EndOfStream
            } else {
                PlanCut::Clean
            },
            fragments: 0,
        });
        start_ms += span;
    }
    finish(entries, timescale)
}

fn ms_to_ticks(ms: i64, timescale: u32) -> u64 {
    let ms = ms.max(0) as u64;
    ms.saturating_mul(u64::from(timescale)) / 1000
}

fn append_audio_tail(
    entries: &mut Vec<PlanEntry>,
    tracks: &TrackDurations,
    policy: &CutPolicy,
    timescale: u32,
) {
    let tail_ms = tracks.audio_ms - tracks.video_ms;
    // A tail under one grid tick is rounding, not a tail.
    if tail_ms <= 50 || entries.is_empty() {
        return;
    }
    let ceiling = policy.max_ticks.max(1);
    let mut remaining = ms_to_ticks(tail_ms, timescale);
    let mut start = entries.last().map(PlanEntry::end_ticks).unwrap_or(0);
    while remaining > 0 {
        let span = remaining.min(ceiling);
        entries.push(PlanEntry {
            index: entries.len() as u32,
            kind: PlanEntryKind::AudioTail,
            start_ticks: start,
            duration_ticks: span,
            est_bytes: tracks.headroom(span, timescale),
            cut: PlanCut::EndOfStream,
            fragments: 0,
        });
        start += span;
        remaining -= span;
    }
}

fn finish(entries: Vec<PlanEntry>, timescale: u32) -> SegmentPlan {
    let target_duration = entries
        .iter()
        .map(|entry| {
            let ts = u64::from(timescale);
            u32::try_from(entry.duration_ticks.div_ceil(ts)).unwrap_or(u32::MAX)
        })
        .max()
        .unwrap_or(0);
    SegmentPlan {
        version: SEGPLAN_VERSION,
        timescale,
        entries,
        target_duration,
    }
}

// ---------------------------------------------------------------------------
// the landing matcher — plan §2.2, ruling A1
// ---------------------------------------------------------------------------

/// Why a repositioned producer could not be placed in the index.
///
/// Both variants are a typed producer failure **and** an index invalidation,
/// never a silent drift (plan §2.2). `NoMatch` is what an ffmpeg upgrade that
/// re-fragments differently looks like: different fragmentation means
/// different output byte counts, so the sequence misses rather than
/// misaligning — which is how D4's "detected, not silently misaligned"
/// promise survives the move from timestamps to byte counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LandingError {
    /// Fewer observed fragments than a match can be made from.
    TooShort { observed: usize, needed: usize },
    /// The observed sequence appears nowhere in the index.
    NoMatch,
    /// It appears only after the row the generation was positioned at, where
    /// `-noaccurate_seek` never lands — the plan and the index disagree.
    PastTarget { row: usize, target: usize },
    /// It appears more than once at or before the target row, over every
    /// fragment observed so far, so the landing is not determined.
    Ambiguous {
        first: usize,
        second: usize,
        compared: usize,
    },
    /// The demuxer's measured landing time is not the start of any index
    /// row, so the index does not describe where this seek lands.
    TimeOffIndex { ticks: u64 },
    /// The measured landing time names an index row whose byte sequence is
    /// not what the generation emitted: the time and the bytes disagree.
    TimeContradicts { row: usize },
}

impl fmt::Display for LandingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LandingError::TooShort { observed, needed } => write!(
                f,
                "a repositioned generation emitted {observed} fragments, \
                 fewer than the {needed} a landing match needs"
            ),
            LandingError::NoMatch => write!(
                f,
                "the repositioned generation's fragment byte sequence appears \
                 nowhere in the index; the index no longer describes this pipe"
            ),
            LandingError::PastTarget { row, target } => write!(
                f,
                "the repositioned generation's fragment byte sequence appears \
                 only at index row {row}, past its target row {target}"
            ),
            LandingError::Ambiguous {
                first,
                second,
                compared,
            } => write!(
                f,
                "the repositioned generation's fragment byte sequence appears \
                 at index rows {first} and {second} over {compared} fragments; \
                 the landing is undetermined"
            ),
            LandingError::TimeOffIndex { ticks } => write!(
                f,
                "the demuxer landed at {ticks} index ticks, which begins no \
                 index row; the index does not describe this seek"
            ),
            LandingError::TimeContradicts { row } => write!(
                f,
                "the demuxer landed at index row {row}, but the repositioned \
                 generation's fragment byte sequence does not appear there"
            ),
        }
    }
}

impl std::error::Error for LandingError {}

/// Where in the index a repositioned producer landed.
///
/// `observed` is the summed VIDEO SAMPLE bytes of each fragment the
/// repositioned generation emitted, in order, starting with the first — not
/// its wire length, which carries audio and container overhead the index has
/// never seen. The producer then
/// discards forward from the returned row to the row that begins its target
/// entry — always forward, because `-noaccurate_seek -ss` lands at the RAP
/// at-or-before the boundary by design.
///
/// `target` is that row, and it bounds the search: a landing after it is
/// physically impossible, so a match there is neither an answer nor a reason
/// to call a real landing ambiguous. Every candidate must reproduce the
/// whole observation, not only its first [`LANDING_WINDOW`] fragments, so a
/// caller that gets [`LandingError::Ambiguous`] can read further fragments and
/// ask again; the copy pipe reproduces the index fragment for fragment, so
/// the true landing keeps matching while a coincidence stops.
///
/// A short observation is accepted only when the match sits within
/// [`LANDING_WINDOW`] of the end of the index — i.e. the stream genuinely ran
/// out of fragments. Anywhere else, a short window is a weaker claim than this
/// function is willing to make.
pub fn match_landing(
    index: &FragmentIndex,
    observed: &[u32],
    target: usize,
) -> Result<usize, LandingError> {
    let last_start = last_landing_start(index, observed)?;
    let probe = observed;
    let rows = &index.rows;
    let matches_at = |start: usize| reproduces(index, start, probe);

    let mut found: Option<usize> = None;
    for start in 0..=last_start.min(target) {
        if !matches_at(start) {
            continue;
        }
        // Every possible landing counts toward ambiguity, including ones the
        // tail rule below will reject. Skipping them silently was how a byte
        // count that appeared twice could still return a confident answer.
        match found {
            None => found = Some(start),
            Some(first) => {
                return Err(LandingError::Ambiguous {
                    first,
                    second: start,
                    compared: probe.len(),
                })
            }
        }
    }
    let Some(start) = found else {
        let past = (target.saturating_add(1)..=last_start).find(|&start| matches_at(start));
        return Err(match past {
            Some(row) => LandingError::PastTarget { row, target },
            None => LandingError::NoMatch,
        });
    };
    // A probe shorter than the window is only believable when the generation
    // genuinely ran out of fragments — that is, when the match lands exactly
    // at the end of the index. Anywhere else it should have produced more.
    if probe.len() < LANDING_WINDOW && start + probe.len() != rows.len() {
        return Err(LandingError::NoMatch);
    }
    Ok(start)
}

/// Where a repositioned producer landed, placed by the time its demuxer
/// landed at, for when the byte sequence alone cannot say.
///
/// Byte counts cannot separate two landings inside a run whose fragments all
/// weigh the same — a static picture, a slow pan over a flat field, a test
/// card — however far the producer reads, because every row of the run
/// reproduces the whole observation. The generation's own timestamps cannot
/// help either: `-avoid_negative_ts make_zero` rebases every generation to
/// zero (module header). What does answer is the source time the demuxer
/// actually landed at, which the caller measures with the generation's own
/// seek and hands in as `landed_ticks`, in index ticks — counted, like the
/// index, from the film's first video keyframe.
///
/// The time names the row and the bytes must still agree. The row is the one
/// whose start is nearest `landed_ticks`, and the time has to sit closer to
/// that start than `tolerance_ticks` (the caller passes one frame) and than
/// half the row's own duration: a landing is always a fragment start, so a
/// time that is off by more than a frame is a measurement the index does not
/// share — a typed failure, never the neighbouring row. Then the whole
/// observation must reproduce from that row, exactly as [`match_landing`]
/// demands of every candidate, and the row is bounded by `target` as there.
/// A time that names no row, or names one the bytes contradict, is a typed
/// failure: the index and the stream disagree about this seek.
pub fn match_landing_at(
    index: &FragmentIndex,
    observed: &[u32],
    target: usize,
    landed_ticks: u64,
    tolerance_ticks: u64,
) -> Result<usize, LandingError> {
    let last_start = last_landing_start(index, observed)?;
    let rows = &index.rows;
    let after = rows.partition_point(|row| row.dts <= landed_ticks);
    let Some(row) = [after.checked_sub(1), (after < rows.len()).then_some(after)]
        .into_iter()
        .flatten()
        .min_by_key(|&row| rows[row].dts.abs_diff(landed_ticks))
    else {
        return Err(LandingError::TimeOffIndex {
            ticks: landed_ticks,
        });
    };
    let off = rows[row].dts.abs_diff(landed_ticks);
    if off >= tolerance_ticks.max(1) || off.saturating_mul(2) >= rows[row].duration.max(1) {
        return Err(LandingError::TimeOffIndex {
            ticks: landed_ticks,
        });
    }
    if row > target {
        return Err(LandingError::PastTarget { row, target });
    }
    if row > last_start || !reproduces(index, row, observed) {
        return Err(LandingError::TimeContradicts { row });
    }
    // The same tail rule as `match_landing`: a short probe is evidence only
    // where the stream genuinely ran out.
    if observed.len() < LANDING_WINDOW && row + observed.len() != rows.len() {
        return Err(LandingError::NoMatch);
    }
    Ok(row)
}

/// What every landing question checks first: a non-empty probe and an index
/// long enough to hold it. Returns the last row a probe this long can start
/// at.
fn last_landing_start(index: &FragmentIndex, observed: &[u32]) -> Result<usize, LandingError> {
    if observed.is_empty() {
        return Err(LandingError::TooShort {
            observed: 0,
            needed: 1,
        });
    }
    let rows = &index.rows;
    if rows.len() < observed.len().min(LANDING_WINDOW) {
        return Err(LandingError::TooShort {
            observed: rows.len(),
            needed: observed.len().min(LANDING_WINDOW),
        });
    }
    // More fragments than the index has rows: no landing reproduces it.
    rows.len()
        .checked_sub(observed.len())
        .ok_or(LandingError::NoMatch)
}

/// Whether the rows from `start` weigh exactly what was observed, fragment
/// for fragment. The caller has bounded `start` by [`last_landing_start`].
fn reproduces(index: &FragmentIndex, start: usize, observed: &[u32]) -> bool {
    index.rows[start..start + observed.len()]
        .iter()
        .zip(observed)
        .all(|(row, bytes)| row.video_bytes == *bytes)
}

/// The index row that begins `entry`, or `None` when the plan and the index
/// disagree about where it starts.
pub fn entry_row(index: &FragmentIndex, entry: &PlanEntry) -> Option<usize> {
    index
        .rows
        .iter()
        .position(|row| row.dts == entry.start_ticks)
}

/// How many fragments to discard to reach `entry` from a landing at `row`.
///
/// `None` when the entry does not begin at an index row at all, or when the
/// landing is already past it — both of which mean the caller is holding a
/// plan and an index that disagree.
pub fn discards_to(index: &FragmentIndex, landed_row: usize, entry: &PlanEntry) -> Option<usize> {
    entry_row(index, entry)?.checked_sub(landed_row)
}

#[cfg(test)]
mod tests {
    use super::{FragmentIndexOutcome, IndexOutcomeState, IndexRefusal, SourceIdentity};
    use crate::content_analysis::{IndexFailureCode, INDEX_FAILURE_POLICY_REVISION};

    fn outcome(
        typed_retryable: Option<bool>,
        next_attempt_at_ms: i64,
        retry_deadline_ms: i64,
    ) -> FragmentIndexOutcome {
        FragmentIndexOutcome {
            source: SourceIdentity::new(1, 1, "argv"),
            refusal: IndexRefusal::Truncated { rows: 10 },
            reason: "budget".to_owned(),
            attempts: 1,
            next_attempt_at_ms,
            updated_at_ms: 0,
            typed_code: typed_retryable.map(|_| IndexFailureCode::IndexSourceIo),
            typed_retryable,
            retry_deadline_ms,
            policy_revision: INDEX_FAILURE_POLICY_REVISION,
            terminal_reason: None,
            diagnostic: None,
        }
    }

    /// Every recorded refusal is due, waiting for a known time, or terminal,
    /// at every instant. The state that was none of these -- retryable, its
    /// retry window past -- kept 83 lab5 titles out of VOD for good.
    #[test]
    fn no_outcome_is_neither_due_nor_waiting_nor_terminal() {
        let cases = [
            outcome(Some(true), 100, 1_000),
            outcome(Some(true), 100, 0),
            outcome(Some(false), i64::MAX, 1_000),
            outcome(None, 100, 0),
            {
                let mut unsupported = outcome(None, i64::MAX, 0);
                unsupported.refusal = IndexRefusal::Unsupported;
                unsupported
            },
        ];
        for case in &cases {
            for now in [0, 99, 100, 500, 999, 1_000, 5_000, i64::MAX - 1] {
                let state = case.state(now);
                let due = case.is_due(now);
                let terminal = case.is_terminal(now);
                let waiting = matches!(state, IndexOutcomeState::WaitUntil(at) if at > now);
                assert_eq!(
                    u8::from(due) + u8::from(terminal) + u8::from(waiting),
                    1,
                    "{case:?} at {now}: {state:?}"
                );
            }
        }
        assert_eq!(cases[0].state(50), IndexOutcomeState::WaitUntil(100));
        assert_eq!(cases[0].state(500), IndexOutcomeState::Due);
        // The former limbo row: retryable, attempt time reached, window over.
        assert_eq!(cases[0].state(1_000), IndexOutcomeState::Due);
        assert_eq!(cases[2].state(1_000), IndexOutcomeState::Terminal);
    }

    /// Review of #1299: a retryable row whose retry deadline passed while the
    /// indexer never reached it (a starved pass) read as terminal -- a
    /// permanent `vod_index_unavailable` for a title nobody tried again. It is
    /// due; the attempt it gets is recorded past the deadline, and that record
    /// is what turns it terminal.
    #[test]
    fn a_retryable_row_the_indexer_never_reached_before_its_deadline_is_due() {
        for code in [
            IndexFailureCode::IndexProbeTimeout,
            IndexFailureCode::IndexSourceIo,
            IndexFailureCode::IndexProcessFailed,
        ] {
            let mut starved = outcome(Some(true), 100, 1_000);
            starved.typed_code = Some(code);
            assert_eq!(starved.state(10_000), IndexOutcomeState::Due, "{code:?}");
        }
        // What the post-deadline attempt records: retry window expired.
        let closed = outcome(Some(false), i64::MAX, 1_000);
        assert_eq!(closed.state(10_000), IndexOutcomeState::Terminal);
    }

    /// A budget failure recorded before measured budgets existed gets one more
    /// attempt, whatever its disposition says.
    #[test]
    fn a_budget_failure_under_the_old_policy_is_due_once() {
        let mut old = outcome(Some(false), i64::MAX, 1_000);
        old.typed_code = Some(IndexFailureCode::IndexBudgetExceeded);
        old.policy_revision = INDEX_FAILURE_POLICY_REVISION - 1;
        assert_eq!(old.state(i64::MAX - 1), IndexOutcomeState::Due);
        old.policy_revision = INDEX_FAILURE_POLICY_REVISION;
        assert_eq!(old.state(i64::MAX - 1), IndexOutcomeState::Terminal);
    }

    /// Review of #1299: a retried old-policy budget row that ended
    /// `Unsupported` kept its stale typed code and revision and read as due on
    /// every pass -- a whole-file read each time. A refusal that is no longer
    /// a truncation is not the budget failure the revision bump re-tries.
    #[test]
    fn an_old_budget_row_whose_refusal_is_now_unsupported_is_not_due() {
        let mut row = outcome(None, i64::MAX, 0);
        row.typed_code = Some(IndexFailureCode::IndexBudgetExceeded);
        row.policy_revision = INDEX_FAILURE_POLICY_REVISION - 1;
        row.refusal = IndexRefusal::Unsupported;
        assert_eq!(row.state(i64::MAX - 1), IndexOutcomeState::Terminal);
    }
    use super::*;

    fn row(dts: u64, duration: u64, bytes: u32, class: CutClass) -> IndexRow {
        IndexRow {
            dts,
            duration,
            bytes,
            // The fixtures make the two differ so nothing can pass by
            // comparing the wrong one: a real production generation's wire
            // length never equals the video-only pipe's.
            video_bytes: bytes.saturating_sub(600),
            class,
        }
    }

    fn identity() -> SourceIdentity {
        SourceIdentity::new(1_024, 1_700_000_000_000, "deadbeefdeadbeef")
    }

    /// A 16000-tick timescale with 1.751750 s GOPs, the shape M0's corpus
    /// used precisely because it is not a whole number of milliseconds.
    fn gop_index(count: usize, class: CutClass) -> FragmentIndex {
        let mut rows = Vec::new();
        let mut dts = 0;
        for i in 0..count {
            let duration = if i % 2 == 0 { 28_016 } else { 28_032 };
            rows.push(row(dts, duration, 100_000 + i as u32, class));
            dts += duration;
        }
        FragmentIndex::new(16_000, rows, "sha", identity())
    }

    fn policy() -> CutPolicy {
        CutPolicy::new(6, 2, 64 * 1024 * 1024, 15, 16_000)
    }

    fn tracks(video_ms: i64, audio_ms: i64) -> TrackDurations {
        TrackDurations {
            video_ms,
            audio_ms,
            audio_bits_per_second: 256_000,
        }
    }

    #[test]
    fn a_clean_stream_cuts_at_the_first_clean_fragment_past_the_floor() {
        let index = gop_index(40, CutClass::CleanIdr);
        let plan = plan_copy(&index, &policy(), &tracks(70_000, 70_000));
        assert!(plan.len() > 2);
        // The first segment answers to the 2 s first-segment floor (32000
        // ticks), which two GOPs clear; every later one answers to the 6 s
        // floor (96000), which takes four.
        assert_eq!(plan.entries[0].fragments, 2);
        assert!(plan.entries[0].duration_ticks >= 32_000);
        assert_eq!(plan.entries[1].fragments, 4);
        assert!(plan.entries[1].duration_ticks >= 96_000);
        assert!(plan
            .entries
            .iter()
            .all(|entry| entry.kind == PlanEntryKind::Video));
    }

    #[test]
    fn the_first_segment_uses_the_lower_first_segment_floor() {
        let index = gop_index(40, CutClass::CleanIdr);
        let plan = plan_copy(&index, &policy(), &tracks(70_000, 70_000));
        let first = plan.entries[0].duration_ticks;
        let second = plan.entries[1].duration_ticks;
        assert!(
            first < second,
            "the 2 s first-segment floor must produce a shorter opening segment \
             than the 6 s floor that follows: {first} vs {second}"
        );
    }

    #[test]
    fn entries_are_contiguous_and_start_at_zero() {
        let index = gop_index(40, CutClass::CleanIdr);
        let plan = plan_copy(&index, &policy(), &tracks(70_000, 95_000));
        assert_eq!(plan.entries[0].start_ticks, 0);
        for pair in plan.entries.windows(2) {
            assert_eq!(
                pair[0].end_ticks(),
                pair[1].start_ticks,
                "entry {} ends at {} but entry {} starts at {}",
                pair[0].index,
                pair[0].end_ticks(),
                pair[1].index,
                pair[1].start_ticks
            );
        }
    }

    #[test]
    fn a_dirty_stream_cuts_at_the_time_ceiling_not_before() {
        let index = gop_index(40, CutClass::Dirty);
        let plan = plan_copy(&index, &policy(), &tracks(70_000, 70_000));
        let ceiling = 15 * 16_000;
        for entry in plan
            .entries
            .iter()
            .filter(|e| e.cut == PlanCut::TimeCeiling)
        {
            assert!(
                entry.duration_ticks >= ceiling,
                "a time-ceiling cut at {} ticks is under the {ceiling}-tick ceiling",
                entry.duration_ticks
            );
        }
        assert!(plan
            .entries
            .iter()
            .any(|entry| entry.cut == PlanCut::TimeCeiling));
    }

    #[test]
    fn the_byte_ceiling_binds_when_bytes_pile_up_faster_than_the_clock() {
        // 40 MB per fragment, dirty, so no clean cut can pre-empt the ceiling.
        let mut index = gop_index(20, CutClass::Dirty);
        for row in &mut index.rows {
            row.bytes = 40_000_000;
        }
        let plan = plan_copy(&index, &policy(), &tracks(35_000, 35_000));
        assert!(
            plan.entries
                .iter()
                .any(|entry| entry.cut == PlanCut::ByteCeiling),
            "expected a byte-ceiling cut, got {:?}",
            plan.entries.iter().map(|e| e.cut).collect::<Vec<_>>()
        );
    }

    #[test]
    fn est_bytes_carries_audio_headroom_above_the_nominal_rate() {
        let index = gop_index(8, CutClass::CleanIdr);
        let plan = plan_copy(&index, &policy(), &tracks(14_000, 14_000));
        let entry = &plan.entries[0];
        let video: u64 = index.rows[..entry.fragments as usize]
            .iter()
            .map(|row| u64::from(row.bytes))
            .sum();
        let seconds = entry.duration_ticks as f64 / 16_000.0;
        let nominal = (256_000.0 / 8.0 * seconds) as u64;
        assert!(
            entry.est_bytes > video + nominal,
            "est_bytes {} must exceed video {video} plus nominal audio {nominal}",
            entry.est_bytes
        );
    }

    #[test]
    fn a_trailing_audio_title_plans_the_tail_and_raises_target_duration() {
        // 30 s of video, 55 s of audio: the c58a4307 case M0 measured.
        let index = gop_index(17, CutClass::CleanIdr);
        let plan = plan_copy(&index, &policy(), &tracks(30_000, 55_000));
        let tails: Vec<_> = plan
            .entries
            .iter()
            .filter(|entry| entry.kind == PlanEntryKind::AudioTail)
            .collect();
        assert!(!tails.is_empty(), "a 25 s audio tail must be planned");
        let tail_ticks: u64 = tails.iter().map(|entry| entry.duration_ticks).sum();
        assert!(tail_ticks > 20 * 16_000);
        // Every tail entry respects the policy ceiling it was split at.
        for entry in &tails {
            assert!(entry.duration_ticks <= 15 * 16_000);
        }
        assert!(
            plan.target_duration >= 15,
            "TARGETDURATION must cover the tail, got {}",
            plan.target_duration
        );
    }

    #[test]
    fn target_duration_counts_audio_tail_entries() {
        let index = gop_index(17, CutClass::CleanIdr);
        let video_only = plan_copy(&index, &policy(), &tracks(30_000, 30_000));
        let with_tail = plan_copy(&index, &policy(), &tracks(30_000, 55_000));
        assert!(
            with_tail.target_duration > video_only.target_duration,
            "a tail longer than every video segment must raise TARGETDURATION: \
             {} vs {}",
            with_tail.target_duration,
            video_only.target_duration
        );
    }

    #[test]
    fn the_transcode_plan_is_a_two_second_grid_with_a_remainder() {
        let plan = plan_transcode(9_500, 90_000, &tracks(9_500, 9_500));
        assert_eq!(plan.len(), 5);
        for entry in &plan.entries[..4] {
            assert_eq!(entry.duration_ticks, 180_000);
        }
        assert_eq!(plan.entries[4].duration_ticks, 135_000);
        assert_eq!(plan.target_duration, 2);
    }

    #[test]
    fn the_plan_survives_a_round_trip_through_json() {
        let index = gop_index(20, CutClass::CleanCra);
        let plan = plan_copy(&index, &policy(), &tracks(35_000, 40_000));
        let text = serde_json::to_string(&plan).expect("serialize");
        let back: SegmentPlan = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(plan, back);

        let text = serde_json::to_string(&index).expect("serialize index");
        let back: FragmentIndex = serde_json::from_str(&text).expect("deserialize index");
        assert_eq!(index, back);
    }

    // ---- the playlist ---------------------------------------------------

    #[test]
    fn the_playlist_is_the_whole_film_before_a_byte_of_it_exists() {
        let index = gop_index(20, CutClass::CleanIdr);
        let plan = plan_copy(&index, &policy(), &tracks(35_000, 35_000));
        let playlist = plan.playlist();

        // VOD, not EVENT. EVENT promises only that segments are appended;
        // VOD promises the playlist is final, and that is what makes the
        // whole duration seekable on the first fetch.
        assert!(
            playlist.contains("#EXT-X-PLAYLIST-TYPE:VOD\n"),
            "{playlist}"
        );
        assert!(playlist.ends_with("#EXT-X-ENDLIST\n"), "{playlist}");
        assert!(playlist.contains("#EXT-X-MAP:URI=\"init.mp4\"\n"));
        assert!(
            !playlist.contains("EXT-X-INDEPENDENT-SEGMENTS"),
            "a ceiling cut makes that claim a lie"
        );

        // Every entry, in order, named the way the segment on disk is.
        let names: Vec<&str> = playlist
            .lines()
            .filter(|line| line.ends_with(".m4s"))
            .collect();
        assert_eq!(names.len(), plan.len(), "{playlist}");
        for (position, name) in names.iter().enumerate() {
            assert_eq!(*name, crate::fmp4::segment_name(position as u64));
        }
    }

    #[test]
    fn the_target_duration_counts_the_audio_tail_too() {
        // M0's audiotail-2397 case: the honest target duration is 15 s where a
        // video-only plan emits 8, and an understated one is a spec violation
        // players act on.
        let index = gop_index(17, CutClass::CleanIdr);
        let plan = plan_copy(&index, &policy(), &tracks(30_000, 55_000));
        let longest = plan
            .entries
            .iter()
            .map(|entry| entry.seconds(plan.timescale).ceil() as u32)
            .max()
            .expect("entries");
        let declared: u32 = plan
            .playlist()
            .lines()
            .find_map(|line| line.strip_prefix("#EXT-X-TARGETDURATION:"))
            .expect("a target duration")
            .parse()
            .expect("a number");
        assert!(
            declared >= longest,
            "declared {declared} is under the longest entry {longest}"
        );
    }

    #[test]
    fn every_extinf_is_the_entry_s_own_duration() {
        let index = gop_index(20, CutClass::CleanIdr);
        let plan = plan_copy(&index, &policy(), &tracks(35_000, 35_000));
        let declared: Vec<f64> = plan
            .playlist()
            .lines()
            .filter_map(|line| line.strip_prefix("#EXTINF:"))
            .map(|line| {
                line.trim_end_matches(',')
                    .parse::<f64>()
                    .expect("a duration")
            })
            .collect();
        assert_eq!(declared.len(), plan.len());
        for (entry, declared) in plan.entries.iter().zip(declared) {
            assert!(
                (entry.seconds(plan.timescale) - declared).abs() < 1e-6,
                "entry {} declared {declared}",
                entry.index
            );
        }
    }

    #[test]
    fn the_playlist_is_the_same_bytes_every_time_it_is_rendered() {
        // It is stored once and served for the life of the rendition. A
        // renderer that varied would hand two clients different films.
        let index = gop_index(20, CutClass::CleanIdr);
        let plan = plan_copy(&index, &policy(), &tracks(35_000, 35_000));
        assert_eq!(plan.playlist(), plan.playlist());
    }

    // ---- the landing matcher ------------------------------------------

    #[test]
    fn a_landing_is_found_by_its_byte_sequence() {
        let index = gop_index(30, CutClass::CleanIdr);
        let observed: Vec<u32> = index.rows[11..14]
            .iter()
            .map(|row| row.video_bytes)
            .collect();
        assert_eq!(match_landing(&index, &observed, 12), Ok(11));
    }

    #[test]
    fn a_landing_absent_from_the_index_is_a_typed_failure() {
        // This is the ffmpeg-upgrade case: the pipe re-fragments, so the byte
        // counts change and the sequence misses instead of misaligning.
        let index = gop_index(30, CutClass::CleanIdr);
        assert_eq!(
            match_landing(&index, &[7, 8, 9], 29),
            Err(LandingError::NoMatch)
        );
    }

    #[test]
    fn a_byte_sequence_that_repeats_is_refused_rather_than_guessed() {
        // A duplicated run near the landing: rows 11..14 and 20..23 weigh the
        // same, and both sit at or before the target row, so either is a
        // landing `-noaccurate_seek` could have made. The M0 corpus never
        // produced this, which is exactly why it is constructed rather than
        // sampled.
        let mut index = gop_index(30, CutClass::CleanIdr);
        for offset in 0..3 {
            index.rows[20 + offset].video_bytes = index.rows[11 + offset].video_bytes;
        }
        let observed: Vec<u32> = index.rows[11..14]
            .iter()
            .map(|row| row.video_bytes)
            .collect();
        assert_eq!(
            match_landing(&index, &observed, 21),
            Err(LandingError::Ambiguous {
                first: 11,
                second: 20,
                compared: 3,
            })
        );
    }

    /// Lab F1 (2026-10-11): a frontier generation at index row 715 failed
    /// because rows 505..508 weighed the same as its first three fragments.
    /// Three equal counts are unremarkable in low-entropy film, and one of
    /// them sitting anywhere in the index vetoed the real landing.
    #[test]
    fn a_repeat_after_the_target_cannot_veto_the_landing() {
        let mut index = gop_index(40, CutClass::CleanIdr);
        for offset in 0..3 {
            index.rows[30 + offset].video_bytes = index.rows[11 + offset].video_bytes;
        }
        let observed: Vec<u32> = index.rows[11..14]
            .iter()
            .map(|row| row.video_bytes)
            .collect();
        // Row 30 is past target row 12: `-noaccurate_seek` never lands there.
        assert_eq!(match_landing(&index, &observed, 12), Ok(11));
    }

    #[test]
    fn a_repeat_far_before_the_target_is_settled_by_reading_further() {
        // Rows 5..8 weigh the same as rows 25..28, both at or before target
        // row 26, so three fragments cannot tell them apart — the lab's
        // shape, with the copy at 715 and the coincidence at 505.
        let mut index = gop_index(40, CutClass::CleanIdr);
        for offset in 0..3 {
            index.rows[5 + offset].video_bytes = index.rows[25 + offset].video_bytes;
        }
        let observed: Vec<u32> = index.rows[25..29]
            .iter()
            .map(|row| row.video_bytes)
            .collect();
        assert_eq!(
            match_landing(&index, &observed[..3], 26),
            Err(LandingError::Ambiguous {
                first: 5,
                second: 25,
                compared: 3,
            })
        );
        // The fourth fragment is where the coincidence stops.
        assert_eq!(match_landing(&index, &observed, 26), Ok(25));
    }

    #[test]
    fn a_run_that_stays_identical_inside_the_window_is_still_refused() {
        // Static film: rows 10..20 all weigh the same, so a landing inside the
        // run is genuinely undetermined however far the producer reads into
        // it, and the matcher must keep refusing rather than pick one.
        let mut index = gop_index(40, CutClass::CleanIdr);
        for row in &mut index.rows[10..20] {
            row.video_bytes = 4_242;
        }
        let observed = vec![4_242u32; 5];
        assert_eq!(
            match_landing(&index, &observed, 15),
            Err(LandingError::Ambiguous {
                first: 10,
                second: 11,
                compared: 5,
            })
        );
    }

    /// Lab F1 (2026-10-11), constant-size content: every fragment of a
    /// static picture weighs the same, so no amount of reading separates the
    /// rows; the demuxer's landing time does.
    /// One frame at 24 fps on the fixtures' 16 kHz timescale.
    const FRAME: u64 = 667;

    #[test]
    fn a_constant_size_run_is_placed_by_its_landing_time() {
        let mut index = gop_index(200, CutClass::CleanIdr);
        for row in &mut index.rows[1..] {
            row.video_bytes = 244;
        }
        let observed = vec![244u32; 40];
        assert!(matches!(
            match_landing(&index, &observed, 150),
            Err(LandingError::Ambiguous {
                first: 1,
                second: 2,
                ..
            })
        ));
        // The demuxer's clock is the source's, not the index's, so its answer
        // can sit a few ticks off the row start.
        let landed = index.rows[149].dts + 7;
        assert_eq!(
            match_landing_at(&index, &observed, 150, landed, FRAME),
            Ok(149)
        );
        let landed = index.rows[150].dts - 7;
        assert_eq!(
            match_landing_at(&index, &observed, 150, landed, FRAME),
            Ok(150)
        );
    }

    #[test]
    fn a_landing_time_the_bytes_contradict_fails_typed() {
        let mut index = gop_index(200, CutClass::CleanIdr);
        for row in &mut index.rows[1..] {
            row.video_bytes = 244;
        }
        let observed = vec![244u32; 40];
        // Row 0 is the opening fragment, which weighs something else.
        assert_eq!(
            match_landing_at(&index, &observed, 150, index.rows[0].dts, FRAME),
            Err(LandingError::TimeContradicts { row: 0 })
        );
        // A time halfway between two fragment starts names neither.
        let between = index.rows[100].dts + index.rows[100].duration / 2;
        assert_eq!(
            match_landing_at(&index, &observed, 150, between, FRAME),
            Err(LandingError::TimeOffIndex { ticks: between })
        );
        // And a time past the target is still past the target.
        assert_eq!(
            match_landing_at(&index, &observed, 150, index.rows[151].dts, FRAME),
            Err(LandingError::PastTarget {
                row: 151,
                target: 150
            })
        );
        // Review of F1: a measurement off by more than a frame — a clock with
        // a different zero than the index's — is refused, not rounded to
        // whichever row start is nearest. Here it is 0.42 s late, well inside
        // half a fragment, which is the shape that published row 29's media
        // as row 30.
        let late = index.rows[149].dts + 6_720;
        assert_eq!(
            match_landing_at(&index, &observed, 150, late, FRAME),
            Err(LandingError::TimeOffIndex { ticks: late })
        );
    }

    #[test]
    fn a_landing_only_past_the_target_is_reported_as_such() {
        let index = gop_index(30, CutClass::CleanIdr);
        let observed: Vec<u32> = index.rows[20..23]
            .iter()
            .map(|row| row.video_bytes)
            .collect();
        assert_eq!(
            match_landing(&index, &observed, 12),
            Err(LandingError::PastTarget {
                row: 20,
                target: 12
            })
        );
    }

    #[test]
    fn a_short_observation_is_accepted_only_at_the_tail() {
        let index = gop_index(30, CutClass::CleanIdr);
        // Two fragments from the very end: acceptable, the stream ran out.
        let tail: Vec<u32> = index.rows[28..30]
            .iter()
            .map(|row| row.video_bytes)
            .collect();
        assert_eq!(match_landing(&index, &tail, 29), Ok(28));

        // Two fragments from the middle: the generation should have produced a
        // third, so refuse rather than accept a weaker claim.
        let middle: Vec<u32> = index.rows[10..12]
            .iter()
            .map(|row| row.video_bytes)
            .collect();
        assert_eq!(
            match_landing(&index, &middle, 29),
            Err(LandingError::NoMatch)
        );
    }

    #[test]
    fn an_empty_observation_is_refused() {
        let index = gop_index(30, CutClass::CleanIdr);
        assert_eq!(
            match_landing(&index, &[], 29),
            Err(LandingError::TooShort {
                observed: 0,
                needed: 1
            })
        );
    }

    #[test]
    fn discards_count_forward_from_the_landing_to_the_entry() {
        let index = gop_index(40, CutClass::CleanIdr);
        let plan = plan_copy(&index, &policy(), &tracks(70_000, 70_000));
        let entry = &plan.entries[2];
        let target = index
            .rows
            .iter()
            .position(|row| row.dts == entry.start_ticks)
            .expect("the entry begins at an index row");
        // A producer that landed one GOP early discards exactly one fragment —
        // the shape M0 measured on 43 of 43 repositions.
        assert_eq!(discards_to(&index, target - 1, entry), Some(1));
        assert_eq!(discards_to(&index, target, entry), Some(0));
        assert_eq!(discards_to(&index, target + 1, entry), None);
    }

    // ---- identity and invalidation -------------------------------------

    #[test]
    fn a_changed_video_branch_invalidates_the_index() {
        let index = gop_index(10, CutClass::CleanIdr);
        let same = identity();
        assert!(index.usable_for(&same));

        let dolby = SourceIdentity::new(
            1_024,
            1_700_000_000_000,
            argv_fingerprint(&[
                "-bsf:v".to_owned(),
                "filter_units=remove_types=32-34".to_owned(),
            ]),
        );
        assert!(
            !index.usable_for(&dolby),
            "a different video branch must never reuse an index: every byte \
             count in it, including the ones the landing matcher compares, \
             describes a pipeline that is no longer running"
        );
    }

    #[test]
    fn a_changed_file_invalidates_the_index() {
        let index = gop_index(10, CutClass::CleanIdr);
        assert!(!index.usable_for(&SourceIdentity::new(
            2_048,
            1_700_000_000_000,
            "deadbeefdeadbeef"
        )));
        assert!(!index.usable_for(&SourceIdentity::new(
            1_024,
            1_700_000_000_001,
            "deadbeefdeadbeef"
        )));
    }

    #[test]
    fn the_fingerprint_separates_pipelines_and_ignores_argument_order_never() {
        let a = argv_fingerprint(&["-c:v".into(), "copy".into()]);
        let b = argv_fingerprint(&["-c:v".into(), "copy".into()]);
        let c = argv_fingerprint(&["copy".into(), "-c:v".into()]);
        assert_eq!(a, b);
        assert_ne!(
            a, c,
            "argument order changes the pipeline, so it must \
                          change the fingerprint"
        );
    }

    #[test]
    fn an_empty_index_plans_nothing_rather_than_a_zero_length_segment() {
        let index = FragmentIndex::new(16_000, Vec::new(), "sha", identity());
        let plan = plan_copy(&index, &policy(), &tracks(0, 0));
        assert!(plan.is_empty());
        assert_eq!(plan.target_duration, 0);
        assert!(!index.usable_for(&identity()));
    }

    fn annotation(
        provenance: AnnotationProvenance,
        start_ms: i64,
        end_ms: i64,
    ) -> TimelineAnnotation {
        TimelineAnnotation {
            kind: AnnotationKind::Credits,
            start_ticks: start_ms,
            end_ticks: end_ms,
            timescale: 1_000,
            start_ms,
            end_ms,
            provenance,
            confidence_millis: 900,
            detector_version: "chapters-v1".to_owned(),
            manual_override_revision: (provenance == AnnotationProvenance::Manual).then_some(1),
        }
    }

    #[test]
    fn annotation_sets_reject_inconsistent_ticks_and_out_of_range_confidence() {
        let mut invalid_ticks = annotation(AnnotationProvenance::Authored, 10, 20);
        invalid_ticks.start_ticks = 9;
        let set = TimelineAnnotationSet {
            source_identity: identity(),
            generation_id: "generation".to_owned(),
            version: 1,
            annotations: vec![invalid_ticks],
        };
        assert!(set.clone().validate_and_normalize(100).is_err());

        let mut invalid_confidence = annotation(AnnotationProvenance::Authored, 10, 20);
        invalid_confidence.confidence_millis = 1_001;
        let mut set = set;
        set.annotations = vec![invalid_confidence];
        assert!(set.validate_and_normalize(100).is_err());
    }

    #[test]
    fn annotation_sets_normalize_same_kind_overlaps_by_evidence_rank() {
        let set = TimelineAnnotationSet {
            source_identity: identity(),
            generation_id: "generation".to_owned(),
            version: 1,
            annotations: vec![
                annotation(AnnotationProvenance::Estimated, 80, 100),
                annotation(AnnotationProvenance::Authored, 70, 90),
            ],
        }
        .validate_and_normalize(100)
        .expect("valid annotation set");
        assert_eq!(set.annotations.len(), 1);
        assert_eq!(
            set.annotations[0].provenance,
            AnnotationProvenance::Authored
        );
        assert_eq!(
            (set.annotations[0].start_ms, set.annotations[0].end_ms),
            (70, 90)
        );
        assert_eq!(
            (set.annotations[0].start_ticks, set.annotations[0].end_ticks),
            (70, 90)
        );
    }

    #[test]
    fn annotation_sets_do_not_merge_adjacent_or_promote_lower_evidence_extensions() {
        let set = TimelineAnnotationSet {
            source_identity: identity(),
            generation_id: "generation".to_owned(),
            version: 1,
            annotations: vec![
                annotation(AnnotationProvenance::Authored, 0, 20),
                annotation(AnnotationProvenance::Estimated, 15, 60),
                annotation(AnnotationProvenance::Authored, 20, 30),
            ],
        }
        .validate_and_normalize(100)
        .expect("valid annotation set");
        assert_eq!(set.annotations.len(), 2);
        assert_eq!(
            (set.annotations[0].start_ms, set.annotations[0].end_ms),
            (0, 20)
        );
        assert_eq!(
            (set.annotations[1].start_ms, set.annotations[1].end_ms),
            (20, 30)
        );
        assert!(set
            .annotations
            .iter()
            .all(|annotation| annotation.provenance == AnnotationProvenance::Authored));
    }

    #[test]
    fn annotation_overlap_merge_preserves_a_common_authoritative_tick_scale() {
        let mut first = annotation(AnnotationProvenance::Authored, 41, 125);
        first.start_ticks = 1;
        first.end_ticks = 3;
        first.timescale = 24;
        let mut second = annotation(AnnotationProvenance::Authored, 83, 166);
        second.start_ticks = 4;
        second.end_ticks = 8;
        second.timescale = 48;
        let set = TimelineAnnotationSet {
            source_identity: identity(),
            generation_id: "generation".to_owned(),
            version: 1,
            annotations: vec![second, first],
        }
        .validate_and_normalize(200)
        .expect("valid mixed-timescale annotation set");
        assert_eq!(set.annotations.len(), 1);
        assert_eq!(set.annotations[0].timescale, 48);
        assert_eq!(set.annotations[0].start_ticks, 2);
        assert_eq!(set.annotations[0].end_ticks, 8);
        assert_eq!(set.annotations[0].start_ms, 41);
        assert_eq!(set.annotations[0].end_ms, 166);
    }

    #[test]
    fn annotation_sets_bound_replicated_counts_and_identifiers() {
        let mut too_many = TimelineAnnotationSet {
            source_identity: identity(),
            generation_id: "generation".to_owned(),
            version: 1,
            annotations: vec![
                annotation(AnnotationProvenance::Authored, 0, 1);
                MAX_TIMELINE_ANNOTATIONS + 1
            ],
        };
        assert!(too_many.clone().validate_and_normalize(100).is_err());

        too_many.annotations.truncate(1);
        too_many.generation_id = "g".repeat(MAX_TIMELINE_IDENTIFIER_BYTES + 1);
        assert!(too_many.clone().validate_and_normalize(100).is_err());

        too_many.generation_id = "generation".to_owned();
        too_many.annotations[0].detector_version = "d".repeat(MAX_TIMELINE_IDENTIFIER_BYTES + 1);
        assert!(too_many.validate_and_normalize(100).is_err());
    }
}
