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
    pub width: u32,
    pub height: u32,
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
            || self.age_ms > 10_000
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
        || state.switch_times_ms.len() >= 6
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
            candidate.playable()
                && candidate.grade == current.grade
                && !state.blocked_candidates.contains(&candidate.id)
        })
        .cloned()
        .collect();
    let picked = select_quality_candidate(&eligible, aspect, target, sample.transfer);
    let link = sample
        .transfer
        .and_then(NetworkTransferEvidence::usable_bps);
    let severe = sample.cause == AutoCause::Link
        && match (link, current.peak_bps) {
            (Some(link), Some(peak)) => u128::from(link) * 10 < u128::from(peak) * 7,
            _ => false,
        };
    let pressure = severe || matches!(sample.cause, AutoCause::Encode | AutoCause::Decode);
    let chosen = if pressure {
        let mut lower: Vec<_> = eligible
            .iter()
            .filter(|candidate| {
                u64::from(candidate.width) * u64::from(candidate.height) < current_area
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
        let link_margin = match (link, chosen.peak_bps) {
            (Some(link), Some(peak)) => u128::from(link) * 10 >= u128::from(peak) * 18,
            _ => false,
        };
        let encode_margin = chosen.complete_cache
            || chosen.route != CandidateRoute::Encode
            || sample
                .active_encode_milli_realtime
                .is_some_and(|speed| speed >= 1150);
        if !link_margin
            || !encode_margin
            || !sample.speculative_admission
            || !sample.parallel_decoder_safe
            || sample.stalled
            || sample.runway_ms.is_none_or(|runway| runway < 10_000)
        {
            state.upgrade_since_ms = None;
            return hold;
        }
        let since = *state.upgrade_since_ms.get_or_insert(sample.now_ms);
        if sample.now_ms.saturating_sub(since) < 45_000 {
            return hold;
        }
    } else if !pressure {
        // A smaller container is advisory; resize recovery waits for a safe
        // boundary unless the viewer explicitly chose a different quality.
        if !sample.natural_boundary {
            return hold;
        }
    }
    if !severe
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
    pub fn committed(&mut self, now_ms: u64) {
        self.last_switch_ms = Some(now_ms);
        self.upgrade_since_ms = None;
        self.mild_samples = 0;
        self.switch_times_ms.push(now_ms);
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
