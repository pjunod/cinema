//! The segment plan — the single source of truth for a VOD rendition.
//!
//! [`VOD-PRESENTATION-PLAN.md`][plan] §2.2 makes this plan **normative, not
//! predictive** (ledger D10): a materializing producer cuts *at* the
//! boundaries computed here and never re-decides them. So nothing in this
//! module tries to reproduce what today's producer-driven policy would have
//! chosen. It computes boundaries that are provably clean, floor- and
//! ceiling-respecting, and stable for the life of the file.
//!
//! [plan]: ../../../docs/VOD-PRESENTATION-PLAN.md
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
//! plan §12.3 and §12.9 ruling A1.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::fmp4::{CutClass, CutPolicy, CutReason};

/// Bumped when a stored index or plan stops being readable by this binary.
/// A persisted row from a newer version is discarded and rebuilt rather than
/// misinterpreted — an index is always cheaper to rebuild than to get wrong.
pub const SEGPLAN_VERSION: u32 = 1;

/// How many consecutive fragments a landing match compares.
///
/// One fragment's byte count is not enough: two fragments of a 90-minute film
/// can plausibly weigh the same, and a confidently wrong landing puts the
/// viewer in the wrong part of the film with no error anywhere. Three is the
/// smallest window that makes a collision require three consecutive
/// coincidences, and it costs the producer nothing — it reads the fragments it
/// was going to read anyway.
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
    /// ceiling accumulates. Also what the landing matcher compares.
    pub bytes: u32,
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
    pub init_sha256: String,
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
    /// It appears more than once, so the landing is not determined.
    Ambiguous { first: usize, second: usize },
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
            LandingError::Ambiguous { first, second } => write!(
                f,
                "the repositioned generation's fragment byte sequence appears \
                 at index rows {first} and {second}; the landing is undetermined"
            ),
        }
    }
}

impl std::error::Error for LandingError {}

/// Where in the index a repositioned producer landed.
///
/// `observed` is the output byte count of each fragment the repositioned
/// generation emitted, in order, starting with the first. The producer then
/// discards forward from the returned row to the row that begins its target
/// entry — always forward, because `-noaccurate_seek -ss` lands at the RAP
/// at-or-before the boundary by design.
///
/// A short observation is accepted only when the match sits within
/// [`LANDING_WINDOW`] of the end of the index — i.e. the stream genuinely ran
/// out of fragments. Anywhere else, a short window is a weaker claim than this
/// function is willing to make.
pub fn match_landing(index: &FragmentIndex, observed: &[u32]) -> Result<usize, LandingError> {
    if observed.is_empty() {
        return Err(LandingError::TooShort {
            observed: 0,
            needed: 1,
        });
    }
    let window = observed.len().min(LANDING_WINDOW);
    let probe = &observed[..window];
    let rows = &index.rows;
    if rows.len() < window {
        return Err(LandingError::TooShort {
            observed: rows.len(),
            needed: window,
        });
    }

    let mut found: Option<usize> = None;
    for start in 0..=rows.len() - window {
        let matches = rows[start..start + window]
            .iter()
            .zip(probe)
            .all(|(row, bytes)| row.bytes == *bytes);
        if !matches {
            continue;
        }
        if window < LANDING_WINDOW && rows.len() - start > LANDING_WINDOW {
            // A short probe matched somewhere with plenty of index left after
            // it. The generation should have produced more fragments, so this
            // is not the tail — refuse rather than guess.
            continue;
        }
        match found {
            None => found = Some(start),
            Some(first) => {
                return Err(LandingError::Ambiguous {
                    first,
                    second: start,
                })
            }
        }
    }
    found.ok_or(LandingError::NoMatch)
}

/// How many fragments to discard to reach `entry` from a landing at `row`.
///
/// `None` when the entry does not begin at an index row at all, or when the
/// landing is already past it — both of which mean the caller is holding a
/// plan and an index that disagree.
pub fn discards_to(index: &FragmentIndex, landed_row: usize, entry: &PlanEntry) -> Option<usize> {
    let target = index
        .rows
        .iter()
        .position(|row| row.dts == entry.start_ticks)?;
    target.checked_sub(landed_row)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(dts: u64, duration: u64, bytes: u32, class: CutClass) -> IndexRow {
        IndexRow {
            dts,
            duration,
            bytes,
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

    // ---- the landing matcher ------------------------------------------

    #[test]
    fn a_landing_is_found_by_its_byte_sequence() {
        let index = gop_index(30, CutClass::CleanIdr);
        let observed: Vec<u32> = index.rows[11..14].iter().map(|row| row.bytes).collect();
        assert_eq!(match_landing(&index, &observed), Ok(11));
    }

    #[test]
    fn a_landing_absent_from_the_index_is_a_typed_failure() {
        // This is the ffmpeg-upgrade case: the pipe re-fragments, so the byte
        // counts change and the sequence misses instead of misaligning.
        let index = gop_index(30, CutClass::CleanIdr);
        assert_eq!(
            match_landing(&index, &[7, 8, 9]),
            Err(LandingError::NoMatch)
        );
    }

    #[test]
    fn a_byte_sequence_that_repeats_is_refused_rather_than_guessed() {
        // A duplicated run near the landing: rows 11..14 and 20..23 weigh the
        // same. The M0 corpus never produced this, which is exactly why it is
        // constructed rather than sampled.
        let mut index = gop_index(30, CutClass::CleanIdr);
        for offset in 0..3 {
            index.rows[20 + offset].bytes = index.rows[11 + offset].bytes;
        }
        let observed: Vec<u32> = index.rows[11..14].iter().map(|row| row.bytes).collect();
        assert_eq!(
            match_landing(&index, &observed),
            Err(LandingError::Ambiguous {
                first: 11,
                second: 20
            })
        );
    }

    #[test]
    fn a_short_observation_is_accepted_only_at_the_tail() {
        let index = gop_index(30, CutClass::CleanIdr);
        // Two fragments from the very end: acceptable, the stream ran out.
        let tail: Vec<u32> = index.rows[28..30].iter().map(|row| row.bytes).collect();
        assert_eq!(match_landing(&index, &tail), Ok(28));

        // Two fragments from the middle: the generation should have produced a
        // third, so refuse rather than accept a weaker claim.
        let middle: Vec<u32> = index.rows[10..12].iter().map(|row| row.bytes).collect();
        assert_eq!(match_landing(&index, &middle), Err(LandingError::NoMatch));
    }

    #[test]
    fn an_empty_observation_is_refused() {
        let index = gop_index(30, CutClass::CleanIdr);
        assert_eq!(
            match_landing(&index, &[]),
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
}
