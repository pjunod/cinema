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
    pub revision: u64,
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
