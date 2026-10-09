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
    /// Descriptive server-resolved output codec; the exact recipe remains authoritative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_codec: Option<String>,
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
            planned_codec: None,
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
    fn compatible_original_keeps_priority_with_unknown_peak() {
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
    }
    #[test]
    fn unproved_encode_is_not_selected() {
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
    }
}
