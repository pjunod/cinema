//! Bounded route identity for negotiated automatic-quality requests.
//!
//! An identity is a lookup key, never authorization or proof of recipe equality.
//! The owner must validate the full source/recipe and current capabilities after
//! lookup, including when two recipes produce the same shortened identity.

use sha2::{Digest, Sha256};

use serde::{de::Error, Deserialize, Deserializer, Serialize, Serializer};

/// Active render-container backing pixels, before fitting/letterboxing.
/// Invalid axes are unknown geometry, never a capability grant or refusal.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PresentationTarget {
    pub width_px: i64,
    pub height_px: i64,
    #[serde(deserialize_with = "deserialize_wire_revision")]
    pub revision: u64,
}

/// Revisions must survive native JSON and JavaScript without rounding.
pub fn deserialize_wire_revision<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<u64, D::Error> {
    let value = u64::deserialize(deserializer)?;
    if !(1..=9_007_199_254_740_991).contains(&value) {
        return Err(D::Error::custom(
            "revision must be a positive JSON-safe integer",
        ));
    }
    Ok(value)
}

impl PresentationTarget {
    pub fn rectangle(self) -> Option<(u32, u32)> {
        if !(1..=16_384).contains(&self.width_px) || !(1..=16_384).contains(&self.height_px) {
            return None;
        }
        Some((self.width_px as u32, self.height_px as u32))
    }
}

/// Fixed-width candidate identity. Keeping this value `Copy` preserves the
/// by-value quality vocabulary used by all playback owners.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CandidateId(pub [u8; 16]);

impl CandidateId {
    /// Derive a lookup key from the full canonical route/recipe digest.
    /// The canonical route must include source revision, delivery method,
    /// selected tracks, actual geometry/grade and recipe version. Its full
    /// digest must remain alongside this key for collision-safe lookup.
    /// Presentation revision is excluded: it does not change produced bytes.
    pub fn for_recipe_digest(recipe_digest: [u8; 32]) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"plurx:auto-quality-candidate:v1\0");
        digest.update(recipe_digest);
        let full = digest.finalize();
        let mut key = [0; 16];
        key.copy_from_slice(&full[..16]);
        Self(key)
    }

    /// The exact lowercase wire representation, also used in desired digests.
    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut result = String::with_capacity(32);
        for byte in self.0 {
            result.push(char::from(HEX[usize::from(byte >> 4)]));
            result.push(char::from(HEX[usize::from(byte & 15)]));
        }
        result
    }

    /// Accept only canonical 32-character lowercase hexadecimal identities.
    pub fn from_hex(value: &str) -> Option<Self> {
        fn nibble(byte: u8) -> Option<u8> {
            match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                _ => None,
            }
        }
        if value.len() != 32 {
            return None;
        }
        let mut bytes = [0; 16];
        for (output, pair) in bytes.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
            *output = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Some(Self(bytes))
    }
}

impl Serialize for CandidateId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for CandidateId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::from_hex(&value)
            .ok_or_else(|| D::Error::custom("candidate_id must be 32 lowercase hex characters"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_digest_domain_is_fixed_and_recipe_scoped() {
        let recipe = std::array::from_fn(|index| index as u8);
        assert_eq!(
            CandidateId::for_recipe_digest(recipe).to_hex(),
            "661b7d5634c18ab35cc653562b70ac92"
        );
        let mut changed = recipe;
        changed[31] ^= 1;
        assert_ne!(
            CandidateId::for_recipe_digest(recipe),
            CandidateId::for_recipe_digest(changed)
        );
    }

    #[test]
    fn presentation_target_bounds_leave_invalid_rectangles_unknown() {
        let target = PresentationTarget {
            width_px: 2400,
            height_px: 1600,
            revision: 3,
        };
        assert_eq!(target.rectangle(), Some((2400, 1600)));
        for revision in [0, 9_007_199_254_740_992u64] {
            assert!(
                serde_json::from_value::<PresentationTarget>(serde_json::json!({
                    "width_px": 2400, "height_px": 1600, "revision": revision
                }))
                .is_err()
            );
        }
        assert!(
            serde_json::from_value::<PresentationTarget>(serde_json::json!({
                "width_px": 2400, "height_px": 1600, "revision": 9_007_199_254_740_991u64
            }))
            .is_ok()
        );
        for width_px in [-1, 0, 16_385, i64::MAX] {
            assert_eq!(PresentationTarget { width_px, ..target }.rectangle(), None);
        }
        for height_px in [-1, 0, 16_385, i64::MAX] {
            assert_eq!(
                PresentationTarget {
                    height_px,
                    ..target
                }
                .rectangle(),
                None
            );
        }
        assert_eq!(
            PresentationTarget {
                width_px: 16_384,
                height_px: 16_384,
                ..target
            }
            .rectangle(),
            Some((16_384, 16_384))
        );
    }

    #[test]
    fn candidate_id_is_copy_and_round_trips_canonical_wire() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<CandidateId>();
        assert_copy::<Option<CandidateId>>();
        let expected = "0123456789abcdef0123456789abcdef";
        let id = CandidateId::from_hex(expected).expect("valid parser-floor test fixture");
        let json = serde_json::to_string(&id).expect("valid parser-floor test fixture");
        assert_eq!(json, format!("\"{expected}\""));
        assert_eq!(
            serde_json::from_str::<CandidateId>(&json).expect("valid parser-floor test fixture"),
            id
        );
        assert_eq!(id.to_hex(), expected);
    }

    #[test]
    fn candidate_id_refuses_noncanonical_or_unbounded_wire() {
        for value in [
            "",
            "0123456789abcdef",
            "0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef0123456789abcdeg",
            "0123456789abcdef0123456789abcdef00",
            "é123456789abcdef0123456789abcdef",
        ] {
            assert!(CandidateId::from_hex(value).is_none(), "{value}");
            assert!(serde_json::from_str::<CandidateId>(
                &serde_json::to_string(value).expect("valid parser-floor test fixture")
            )
            .is_err());
        }
        for json in ["null", "0", "[]", "{}"] {
            assert!(serde_json::from_str::<CandidateId>(json).is_err());
        }
    }
}

/// A complete route identity retained beside the shortened wire lookup key.
/// Worker proof is specific to the recipe, not to a height or encoder family.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QualityCandidate {
    pub id: CandidateId,
    pub recipe_digest: [u8; 32],
    pub route: CandidateRoute,
    /// False only for exact compatible legacy cache recipes; identity remains distinct.
    #[serde(
        default = "normalized_geometry_default",
        skip_serializing_if = "normalized_geometry_default_value"
    )]
    pub normalized_geometry: bool,
    pub width: u32,
    pub height: u32,
    /// Requested encoder rung; distinct from a cropped/source-ceiling raster.
    pub target_height: u32,
    pub average_bps: Option<u64>,
    pub peak_bps: Option<u64>,
    pub grade: crate::transcode::OutputGrade,
    pub decoder_compatible: bool,
    pub complete_cache: bool,
    pub sustainable: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CandidateRoute {
    Original,
    Remux,
    Encode,
}

fn normalized_geometry_default() -> bool {
    true
}
fn normalized_geometry_default_value(value: &bool) -> bool {
    *value
}

impl QualityCandidate {
    pub fn identity_matches(&self) -> bool {
        self.id == CandidateId::for_recipe_digest(self.recipe_digest)
    }

    /// Cached bytes avoid encode admission, but never avoid decoder admission.
    pub fn playable(&self) -> bool {
        self.identity_matches()
            && self.width > 0
            && self.height > 0
            && self.decoder_compatible
            && (self.complete_cache || self.sustainable)
    }
}

/// A transfer is useful link evidence only when it completed over the network
/// independently of producer pacing and has not aged out. Legacy aggregate
/// bandwidth values deliberately cannot construct this proof implicitly.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NetworkTransferEvidence {
    pub bytes: u64,
    pub elapsed_ms: u32,
    pub age_ms: u32,
    pub completed: bool,
    pub from_cache: bool,
    pub producer_paced: bool,
}

impl NetworkTransferEvidence {
    pub fn usable_bps(self) -> Option<u64> {
        if !self.completed
            || self.from_cache
            || self.producer_paced
            || self.bytes == 0
            || self.elapsed_ms == 0
            || self.age_ms > 15_000
        {
            return None;
        }
        self.bytes
            .checked_mul(8_000)?
            .checked_div(u64::from(self.elapsed_ms))
    }
}

/// One policy shared by initial Auto, menus and runtime transitions. Original
/// routes retain priority when they can decode sustainably; display fit applies
/// to produced alternatives and never compels a compatible original downgrade.
pub fn select_quality_candidate(
    candidates: &[QualityCandidate],
    aspect: crate::playback::geometry::DisplayAspect,
    target: Option<PresentationTarget>,
    network: Option<NetworkTransferEvidence>,
) -> Option<&QualityCandidate> {
    let link_bps = network.and_then(NetworkTransferEvidence::usable_bps);
    let fits_link = |candidate: &&QualityCandidate| {
        candidate.playable()
            && match (candidate.peak_bps, link_bps) {
                (Some(peak), Some(link)) => peak <= link,
                // Unknown wire cost or link cannot force a downgrade. It is
                // not sufficient evidence for a voluntary upgrade either.
                _ => true,
            }
    };
    if let Some(original) = candidates
        .iter()
        .filter(fits_link)
        .filter(|candidate| candidate.route != CandidateRoute::Encode)
        .max_by_key(|candidate| u64::from(candidate.width) * u64::from(candidate.height))
    {
        return Some(original);
    }
    let mut eligible: Vec<_> = candidates
        .iter()
        .filter(fits_link)
        .filter(|candidate| candidate.route == CandidateRoute::Encode)
        .collect();
    eligible.sort_by_key(|candidate| {
        (
            u64::from(candidate.width) * u64::from(candidate.height),
            candidate.peak_bps,
        )
    });
    if let Some(target) = target.filter(|target| target.rectangle().is_some()) {
        if let Some(candidate) = eligible.iter().find(|candidate| {
            aspect.covered_by(target, (candidate.width, candidate.height)) == Some(true)
        }) {
            let minimum_area = u64::from(candidate.width) * u64::from(candidate.height);
            if let Some(cached) = eligible
                .iter()
                .filter(|cached| {
                    cached.complete_cache
                        && cached.grade == candidate.grade
                        && u64::from(cached.width) * u64::from(cached.height) >= minimum_area
                        && aspect.covered_by(target, (cached.width, cached.height)) == Some(true)
                })
                .max_by_key(|cached| u64::from(cached.width) * u64::from(cached.height))
            {
                return Some(cached);
            }
            return Some(candidate);
        }
    }
    eligible.last().copied()
}

/// Lookup requires full recipe equality, so a shortened-key collision cannot
/// authorize another source, grade, track or production profile.
pub fn find_quality_candidate(
    candidates: &[QualityCandidate],
    id: CandidateId,
    recipe_digest: [u8; 32],
) -> Option<&QualityCandidate> {
    candidates.iter().find(|candidate| {
        candidate.id == id && candidate.recipe_digest == recipe_digest && candidate.playable()
    })
}

/// Cause belongs to the observed failure domain. Runway urgency alone never
/// establishes a link, encode or decoder cause.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoCause {
    Link,
    Encode,
    Decode,
    Hold,
    Authority,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoTransition {
    Hold,
    Prepare,
    Recover,
    NaturalBoundary,
}

#[derive(Clone, Debug, Default)]
pub struct AutoRuntimeState {
    pub last_switch_ms: Option<u64>,
    pub last_stall_ms: Option<u64>,
    pub last_cliff_ms: Option<u64>,
    pub target_revision: Option<u64>,
    pub target_changed_ms: Option<u64>,
    pub upgrade_since_ms: Option<u64>,
    pub mild_samples: u8,
    pub switch_times_ms: Vec<u64>,
    pub blocked_candidates: Vec<CandidateId>,
}

#[derive(Clone, Copy, Debug)]
pub struct AutoRuntimeSample {
    pub now_ms: u64,
    pub automatic: bool,
    pub presenting: bool,
    pub paused: bool,
    pub seeking: bool,
    pub move_in_flight: bool,
    pub cause: AutoCause,
    pub cause_age_ms: u32,
    pub runway_ms: Option<u32>,
    pub stalled: bool,
    pub natural_boundary: bool,
    pub speculative_admission: bool,
    pub parallel_decoder_safe: bool,
    /// Active production measurement with input pacing removed. None is
    /// unknown and cannot prove a speculative encode upgrade.
    pub active_encode_milli_realtime: Option<u32>,
    pub transfer: Option<NetworkTransferEvidence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoRuntimeDecision {
    pub candidate_id: Option<CandidateId>,
    pub transition: AutoTransition,
    pub cause: AutoCause,
}

/// Pure route-based reducer. The adapter owns clocks, measurement, presentation
/// and the prepared-switch exchange; this function owns no timer or resources.
pub fn decide_auto_transition(
    state: &mut AutoRuntimeState,
    sample: AutoRuntimeSample,
    candidates: &[QualityCandidate],
    current_id: CandidateId,
    aspect: crate::playback::geometry::DisplayAspect,
    target: Option<PresentationTarget>,
) -> AutoRuntimeDecision {
    let hold = AutoRuntimeDecision {
        candidate_id: None,
        transition: AutoTransition::Hold,
        cause: sample.cause,
    };
    state
        .switch_times_ms
        .retain(|at| sample.now_ms.saturating_sub(*at) < 3_600_000);
    if !sample.automatic
        || !sample.presenting
        || sample.paused
        || sample.seeking
        || sample.move_in_flight
    {
        state.upgrade_since_ms = None;
        state.mild_samples = 0;
        return hold;
    }
    let Some(current) = candidates
        .iter()
        .find(|candidate| candidate.id == current_id)
    else {
        return hold;
    };
    if sample.cause_age_ms > 15_000
        || matches!(sample.cause, AutoCause::Hold | AutoCause::Authority)
    {
        state.upgrade_since_ms = None;
        return hold;
    }
    if sample.stalled {
        state.last_stall_ms = Some(sample.now_ms);
    }
    if state.target_revision != target.map(|target| target.revision) {
        state.target_revision = target.map(|target| target.revision);
        state.target_changed_ms = Some(sample.now_ms);
        state.upgrade_since_ms = None;
    }
    let current_area = u64::from(current.width) * u64::from(current.height);
    if sample.cause == AutoCause::Decode {
        if !state.blocked_candidates.contains(&current_id) {
            state.blocked_candidates.push(current_id);
        } else {
            // A repeated decoder failure belongs to compatibility recovery.
            return hold;
        }
    }
    let eligible: Vec<_> = candidates
        .iter()
        .filter(|candidate| {
            (candidate.playable()
                || (!sample.natural_boundary
                    && candidate.identity_matches()
                    && candidate.decoder_compatible
                    && candidate.width > 0
                    && candidate.height > 0))
                && (candidate.grade == current.grade || candidate.route != CandidateRoute::Encode)
                && !state.blocked_candidates.contains(&candidate.id)
        })
        .cloned()
        .collect();
    let trials: Vec<_> = eligible
        .iter()
        .cloned()
        .map(|mut candidate| {
            candidate.sustainable = true;
            candidate
        })
        .collect();
    let picked = select_quality_candidate(&trials, aspect, target, sample.transfer);
    let link = sample
        .transfer
        .and_then(NetworkTransferEvidence::usable_bps);
    let severe = sample.cause == AutoCause::Link
        && match (link, current.peak_bps) {
            (Some(link), Some(peak)) => u128::from(link) * 10 < u128::from(peak) * 7,
            _ => false,
        };
    if severe {
        state.last_cliff_ms = Some(sample.now_ms);
    }
    let mild = sample.cause == AutoCause::Link
        && match (link, current.peak_bps) {
            (Some(link), Some(peak)) => u128::from(link) * 10 < u128::from(peak) * 13,
            _ => false,
        };
    state.mild_samples = if mild {
        state.mild_samples.saturating_add(1)
    } else {
        0
    };
    let pressure = severe
        || state.mild_samples >= 2
        || matches!(sample.cause, AutoCause::Encode | AutoCause::Decode);
    let chosen = if pressure {
        let mut lower: Vec<_> = eligible
            .iter()
            .filter(|candidate| {
                candidate.playable()
                    && u64::from(candidate.width) * u64::from(candidate.height) < current_area
            })
            .collect();
        lower.sort_by_key(|candidate| u64::from(candidate.width) * u64::from(candidate.height));
        if severe {
            lower
                .iter()
                .rev()
                .find(|candidate| match (candidate.peak_bps, link) {
                    (Some(peak), Some(link)) => peak <= link,
                    _ => false,
                })
                .copied()
        } else {
            lower.last().copied()
        }
    } else {
        picked
    };
    let Some(chosen) = chosen.filter(|chosen| chosen.id != current_id) else {
        state.upgrade_since_ms = None;
        return hold;
    };
    let chosen_area = u64::from(chosen.width) * u64::from(chosen.height);
    let upgrade = chosen_area > current_area
        || chosen.route != CandidateRoute::Encode && current.route == CandidateRoute::Encode;
    if upgrade {
        if !sample.natural_boundary
            && (state
                .last_stall_ms
                .is_some_and(|at| sample.now_ms.saturating_sub(at) < 60_000)
                || state
                    .last_cliff_ms
                    .is_some_and(|at| sample.now_ms.saturating_sub(at) < 90_000))
        {
            state.upgrade_since_ms = None;
            return hold;
        }
        let link_margin = match (link, chosen.peak_bps) {
            (Some(link), Some(peak)) => u128::from(link) * 10 >= u128::from(peak) * 18,
            (None, _) | (_, None) => true,
        };
        // Production is proved by the successor before commit, not before requesting its bounded slot.
        if (!sample.natural_boundary && !link_margin)
            || (!sample.natural_boundary
                && (sample.stalled || sample.runway_ms.is_none_or(|runway| runway < 10_000)))
        {
            state.upgrade_since_ms = None;
            return hold;
        }
        if !sample.natural_boundary {
            let since = *state.upgrade_since_ms.get_or_insert(sample.now_ms);
            if sample.now_ms.saturating_sub(since) < 45_000 {
                return hold;
            }
        }
    } else if !pressure {
        // A smaller container is advisory; resize recovery waits for a safe
        // boundary unless the viewer explicitly chose a different quality.
        if !sample.natural_boundary {
            return hold;
        }
    }
    if !pressure
        && !sample.natural_boundary
        && state
            .target_changed_ms
            .is_some_and(|at| sample.now_ms.saturating_sub(at) < 5_000)
    {
        return hold;
    }
    if !severe && !sample.natural_boundary && state.switch_times_ms.len() >= 6 {
        return hold;
    }
    if !severe
        && !sample.natural_boundary
        && state
            .last_switch_ms
            .is_some_and(|last| sample.now_ms.saturating_sub(last) < 60_000)
    {
        return hold;
    }
    AutoRuntimeDecision {
        candidate_id: Some(chosen.id),
        transition: if sample.natural_boundary {
            AutoTransition::NaturalBoundary
        } else if sample.stalled {
            AutoTransition::Recover
        } else {
            AutoTransition::Prepare
        },
        cause: sample.cause,
    }
}

impl AutoRuntimeState {
    /// Call only after the presentation owner confirms the switch landed.
    pub fn committed(&mut self, now_ms: u64, voluntary: bool) {
        self.last_switch_ms = Some(now_ms);
        self.upgrade_since_ms = None;
        self.mild_samples = 0;
        if voluntary {
            self.switch_times_ms.push(now_ms);
        }
        self.switch_times_ms
            .retain(|at| now_ms.saturating_sub(*at) < 3_600_000);
        self.switch_times_ms.truncate(6);
    }

    pub fn viewer_discontinuity(&mut self) {
        self.switch_times_ms.clear();
        self.upgrade_since_ms = None;
        self.mild_samples = 0;
    }
}

/// A prepared request may be issued from spare capacity before these receipts
/// exist; commit requires both the exact server reservation and successfully
/// staged decoder. The incumbent survives refusal or expiry.
pub fn auto_prepared_commit_allowed(
    requested: CandidateId,
    reserved: Option<CandidateId>,
    sample: AutoRuntimeSample,
    presentation_revision_matches: bool,
    decoder_revision_matches: bool,
) -> bool {
    reserved == Some(requested)
        && sample.speculative_admission
        && sample.parallel_decoder_safe
        && presentation_revision_matches
        && decoder_revision_matches
        && sample.automatic
        && sample.presenting
        && !sample.paused
        && !sample.seeking
}

#[cfg(test)]
mod policy_regressions {
    use super::*;
    use crate::playback::geometry::DisplayAspect;
    fn candidate(
        height: u32,
        route: CandidateRoute,
        sustainable: bool,
        cached: bool,
    ) -> QualityCandidate {
        let recipe_digest = [height as u8; 32];
        QualityCandidate {
            id: CandidateId::for_recipe_digest(recipe_digest),
            recipe_digest,
            route,
            normalized_geometry: true,
            width: height * 16 / 9,
            height,
            target_height: height,
            average_bps: Some(u64::from(height) * 8_000),
            peak_bps: Some(u64::from(height) * 12_000),
            grade: crate::transcode::OutputGrade::Sdr,
            decoder_compatible: true,
            complete_cache: cached,
            sustainable,
        }
    }
    fn aspect() -> DisplayAspect {
        DisplayAspect::from_source(3840, 2160, Some((1, 1)), Some(0))
            .expect("fixture supplies a valid policy input")
    }
    fn target(width: i64) -> PresentationTarget {
        PresentationTarget {
            width_px: width,
            height_px: 1600,
            revision: 1,
        }
    }
    fn sample(now_ms: u64) -> AutoRuntimeSample {
        AutoRuntimeSample {
            now_ms,
            automatic: true,
            presenting: true,
            paused: false,
            seeking: false,
            move_in_flight: false,
            cause: AutoCause::Unknown,
            cause_age_ms: 0,
            runway_ms: Some(12_000),
            stalled: false,
            natural_boundary: false,
            speculative_admission: false,
            parallel_decoder_safe: false,
            active_encode_milli_realtime: None,
            transfer: None,
        }
    }
    #[test]
    fn fit_boundary_and_complete_cache_use_one_catalog() {
        let candidates = [
            candidate(1080, CandidateRoute::Encode, true, false),
            candidate(1440, CandidateRoute::Encode, true, false),
        ];
        assert_eq!(
            select_quality_candidate(&candidates, aspect(), Some(target(2112)), None)
                .expect("fixture supplies a valid policy input")
                .height,
            1080
        );
        assert_eq!(
            select_quality_candidate(&candidates, aspect(), Some(target(2113)), None)
                .expect("fixture supplies a valid policy input")
                .height,
            1440
        );
        let mut cached = candidates.to_vec();
        cached.push(candidate(2160, CandidateRoute::Encode, false, true));
        assert_eq!(
            select_quality_candidate(&cached, aspect(), Some(target(2400)), None)
                .expect("fixture supplies a valid policy input")
                .height,
            2160
        );
    }
    #[test]
    fn compatible_original_priority_and_unknown_peak_probe_preserve_incumbent() {
        let mut original = candidate(2160, CandidateRoute::Remux, true, false);
        original.peak_bps = None;
        let candidates = [
            candidate(1080, CandidateRoute::Encode, true, false),
            original,
        ];
        assert_eq!(
            select_quality_candidate(&candidates, aspect(), Some(target(2400)), None)
                .expect("fixture supplies a valid policy input")
                .route,
            CandidateRoute::Remux
        );
        let mut state = AutoRuntimeState {
            target_revision: Some(1),
            upgrade_since_ms: Some(0),
            ..Default::default()
        };
        let result = decide_auto_transition(
            &mut state,
            sample(60_000),
            &candidates,
            candidates[0].id,
            aspect(),
            Some(target(2400)),
        );
        assert_eq!(result.transition, AutoTransition::Prepare);
        assert!(!auto_prepared_commit_allowed(
            candidates[1].id,
            None,
            sample(60_000),
            true,
            true
        ));
    }
    #[test]
    fn unproved_encode_is_trial_only_and_stall_cliff_windows_are_enforced() {
        let candidates = [
            candidate(1080, CandidateRoute::Encode, true, false),
            candidate(1440, CandidateRoute::Encode, false, false),
        ];
        assert_eq!(
            select_quality_candidate(&candidates, aspect(), Some(target(2400)), None)
                .expect("fixture supplies a valid policy input")
                .height,
            1080
        );
        let mut state = AutoRuntimeState {
            target_revision: Some(1),
            ..Default::default()
        };
        assert_eq!(
            decide_auto_transition(
                &mut state,
                sample(0),
                &candidates,
                candidates[0].id,
                aspect(),
                Some(target(2400))
            )
            .transition,
            AutoTransition::Hold
        );
        assert_eq!(
            decide_auto_transition(
                &mut state,
                sample(45_000),
                &candidates,
                candidates[0].id,
                aspect(),
                Some(target(2400))
            )
            .transition,
            AutoTransition::Prepare
        );
        state.last_stall_ms = Some(45_000);
        assert_eq!(
            decide_auto_transition(
                &mut state,
                sample(104_999),
                &candidates,
                candidates[0].id,
                aspect(),
                Some(target(2400))
            )
            .transition,
            AutoTransition::Hold
        );
        state.last_stall_ms = None;
        state.last_cliff_ms = Some(45_000);
        assert_eq!(
            decide_auto_transition(
                &mut state,
                sample(134_999),
                &candidates,
                candidates[0].id,
                aspect(),
                Some(target(2400))
            )
            .transition,
            AutoTransition::Hold
        );
        let mut boundary = sample(134_999);
        boundary.natural_boundary = true;
        assert_eq!(
            decide_auto_transition(
                &mut state,
                boundary,
                &candidates,
                candidates[0].id,
                aspect(),
                Some(target(2400))
            )
            .transition,
            AutoTransition::Hold
        );
    }
    #[test]
    fn severe_link_pressure_skips_voluntary_budget_without_using_stale_samples() {
        let candidates = [
            candidate(480, CandidateRoute::Encode, true, false),
            candidate(720, CandidateRoute::Encode, true, false),
            candidate(1080, CandidateRoute::Encode, true, false),
            candidate(1440, CandidateRoute::Encode, true, false),
        ];
        let mut state = AutoRuntimeState {
            switch_times_ms: vec![0; 6],
            last_switch_ms: Some(99_999),
            target_revision: Some(1),
            ..Default::default()
        };
        let mut pressure = sample(100_000);
        pressure.cause = AutoCause::Link;
        pressure.transfer = Some(NetworkTransferEvidence {
            bytes: 1_000_000,
            elapsed_ms: 1000,
            age_ms: 0,
            completed: true,
            from_cache: false,
            producer_paced: false,
        });
        let result = decide_auto_transition(
            &mut state,
            pressure,
            &candidates,
            candidates[3].id,
            aspect(),
            Some(target(2400)),
        );
        assert_eq!(result.candidate_id, Some(candidates[0].id));
        pressure
            .transfer
            .as_mut()
            .expect("fixture supplies a valid policy input")
            .age_ms = 15_001;
        assert_eq!(
            decide_auto_transition(
                &mut state,
                pressure,
                &candidates,
                candidates[3].id,
                aspect(),
                Some(target(2400))
            )
            .transition,
            AutoTransition::Hold
        );
    }
}
