//! Bounded cancellation receipts, independent of terminal session acknowledgements.
//! These writes never end the incumbent route or release its cache pins.
use serde::{Deserialize, Serialize};

pub(crate) const QUALITY_CANCELLATION_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS quality_cancellation_receipts (
    receipt_key TEXT PRIMARY KEY,
    generation TEXT NOT NULL,
    session_id TEXT NOT NULL,
    owner_node_id TEXT NOT NULL,
    owner_epoch INTEGER NOT NULL CHECK(owner_epoch > 0),
    client_instance_id TEXT NOT NULL,
    lifetime_id TEXT NOT NULL,
    recipe_revision INTEGER NOT NULL CHECK(recipe_revision > 0),
    accepted_sequence INTEGER NOT NULL CHECK(accepted_sequence > 0),
    state TEXT NOT NULL CHECK(state IN ('requested', 'settled')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(generation, client_instance_id, lifetime_id, recipe_revision, accepted_sequence)
) STRICT;
CREATE INDEX IF NOT EXISTS quality_cancellation_generation
    ON quality_cancellation_receipts(generation, client_instance_id, lifetime_id, recipe_revision);
CREATE INDEX IF NOT EXISTS quality_cancellation_retention
    ON quality_cancellation_receipts(updated_at_ms, receipt_key);
CREATE TABLE IF NOT EXISTS quality_preparation_owners (
    staged_incarnation_id TEXT PRIMARY KEY,
    cancellation_key TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS quality_preparation_intent_identity
    ON quality_preparation_owners(cancellation_key) WHERE cancellation_key != '';";

pub(crate) const CANCELLATION_COLS: &str = "receipt_key, generation, session_id, owner_node_id, owner_epoch, client_instance_id, lifetime_id, recipe_revision, accepted_sequence, state, created_at_ms, updated_at_ms";
pub(crate) const INSERT_CANCELLATION: &str = "INSERT OR IGNORE INTO quality_cancellation_receipts
    (receipt_key, generation, session_id, owner_node_id, owner_epoch, client_instance_id,
     lifetime_id, recipe_revision, accepted_sequence, state, created_at_ms, updated_at_ms)
    SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, 'requested', $10, $10
    WHERE EXISTS (SELECT 1 FROM media_sessions WHERE incarnation_id = $2 AND session_id = $3
        AND owner_node_id = $4 AND owner_epoch = $5 AND state = 'active' AND lease_expires_at_ms > $10)
    AND (SELECT COUNT(*) FROM quality_cancellation_receipts WHERE generation = $2) < 128";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityCancellationReceipt {
    pub receipt_key: String,
    pub generation: String,
    pub session_id: String,
    pub owner_node_id: String,
    pub owner_epoch: i64,
    pub client_instance_id: String,
    pub lifetime_id: String,
    pub recipe_revision: i64,
    pub accepted_sequence: i64,
    pub state: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl QualityCancellationReceipt {
    pub fn valid_request(&self) -> bool {
        self.receipt_key.len() == 64
            && self.receipt_key.bytes().all(|b| b.is_ascii_hexdigit())
            && uuid::Uuid::parse_str(&self.generation).is_ok()
            && uuid::Uuid::parse_str(&self.session_id).is_ok()
            && uuid::Uuid::parse_str(&self.client_instance_id).is_ok()
            && !self.owner_node_id.is_empty()
            && self.owner_node_id.len() <= 256
            && self.owner_epoch > 0
            && !self.lifetime_id.is_empty()
            && self.lifetime_id.len() <= crate::playback::MAX_LIFETIME_ID
            && !self.lifetime_id.bytes().any(|b| b.is_ascii_control())
            && (1..=9_007_199_254_740_991).contains(&self.recipe_revision)
            && (1..=9_007_199_254_740_991).contains(&self.accepted_sequence)
            && self.state == "requested"
            && self.created_at_ms > 0
            && self.updated_at_ms == self.created_at_ms
    }

    pub(crate) fn same_request(&self, other: &Self) -> bool {
        self.receipt_key == other.receipt_key
            && self.generation == other.generation
            && self.session_id == other.session_id
            && self.owner_node_id == other.owner_node_id
            && self.owner_epoch == other.owner_epoch
            && self.client_instance_id == other.client_instance_id
            && self.lifetime_id == other.lifetime_id
            && self.recipe_revision == other.recipe_revision
            && self.accepted_sequence == other.accepted_sequence
    }
}
