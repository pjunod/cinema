//! Receiver-owned remote sessions. These handles never authorize Source work.
use crate::{
    sharing::SourceId, sharing_catalogue::SharedReference, sharing_catalogue_details::FileRevision,
    store::sharing_catalogue::ReceiverCatalogueScope,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiverProducerKind {
    RemoteSource,
}

/// Complete persisted remote identity; no fake Local file or Source address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteSourceRecipe {
    pub kind: ReceiverProducerKind,
    pub version: u8,
    pub reference: SharedReference,
    pub lifecycle_generation: i64,
    pub file_id: SourceId,
    pub file_revision: FileRevision,
    pub source_request_id: Uuid,
    /// Canonical bounded actual playback request, including capability/selection.
    pub request_json: String,
}

#[derive(Clone)]
pub struct ReceiverSessionIntent {
    pub scope: ReceiverCatalogueScope,
    pub user_id: i64,
    pub login_hash: String,
    pub recipe: RemoteSourceRecipe,
    pub source_position_ms: i64,
}

/// Constructed only from a current Store login/policy/scope snapshot. Every
/// committing writer must repeat its guard inside the lifecycle transaction.
#[derive(Clone)]
pub struct ReceiverSessionWriteAuthority {
    pub(crate) intent: ReceiverSessionIntent,
    pub(crate) policy_json: String,
    pub(crate) last_seen: i64,
    pub(crate) login_expires_at_s: Option<i64>,
    pub(crate) observed_at_ms: i64,
}

impl RemoteSourceRecipe {
    /// Request identity excludes the planned incarnation so exact request
    /// replay can recover its previously persisted Source request UUID.
    pub fn request_fingerprint(&self) -> Result<String, crate::error::StoreError> {
        use sha2::{Digest, Sha256};
        let mut value = serde_json::to_value(self).map_err(|_| crate::sharing::invalid())?;
        value
            .as_object_mut()
            .ok_or_else(crate::sharing::invalid)?
            .remove("source_request_id");
        let mut hash = Sha256::new();
        hash.update(b"plurx.receiver.remote-source-request.v1\0");
        hash.update(serde_json::to_vec(&value).map_err(|_| crate::sharing::invalid())?);
        Ok(format!("{:x}", hash.finalize()))
    }
}
