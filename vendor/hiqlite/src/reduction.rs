//! A durable internal reduction reference, never transmitted clock authority.

use serde::{Deserialize, Serialize};

/// Untrusted transport data. A receiver must compare this complete immutable
/// tuple to the actual durable fence before excluding any target from clocks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReductionFenceReference {
    pub version: u8,
    pub target_node_id: String,
    pub target_raft_id: u64,
    pub attempt_id: String,
    pub barrier_index: u64,
}

impl ReductionFenceReference {
    /// Bounds are checked before Store work; passing them is not fence proof.
    #[must_use]
    pub fn has_valid_shape(&self) -> bool {
        self.version == 1
            && self.barrier_index > 0
            && canonical_uuid(&self.target_node_id)
            && canonical_uuid(&self.attempt_id)
    }
}

fn canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}
