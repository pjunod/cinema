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
    /// Original B login; another login for the same user cannot adopt this film.
    pub parent_login_hash: String,
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

/// Exact existing blocked receiver owner. No Source or delivery authority.
#[derive(Clone)]
pub struct ReceiverPendingRenewal {
    pub incarnation_id: String,
    pub owner_node_id: String,
    pub owner_epoch: i64,
    pub request_id: String,
    pub now_ms: i64,
    pub lease_expires_at_ms: i64,
}

/// Exact B owner captured by the receiver actor. This is metadata identity,
/// not evidence that Source has admitted or published a producer.
#[derive(Clone)]
pub struct ReceiverSourceOwner {
    pub incarnation_id: Uuid,
    pub session_id: Uuid,
    pub owner_node_id: String,
    pub owner_epoch: i64,
    pub request_id: String,
    pub lease_expires_at_ms: i64,
    pub now_ms: i64,
}

/// Complete Source result retained by B. The caller must verify actual Source
/// publication and seal its upstream capability with the selected B key/AAD.
/// Store checks the closed envelope and current B authority; it grants no
/// Source authority. No wire encoding or cleartext credential representation.
#[derive(Clone)]
pub struct ReceiverSourceBinding {
    pub reference: SharedReference,
    pub file_id: SourceId,
    pub file_revision: FileRevision,
    pub source_request_id: Uuid,
    pub source_session_id: Uuid,
    pub source_incarnation_id: Uuid,
    pub capability_envelope: crate::secrets::SealedSecret,
}

#[derive(Clone)]
pub struct ReceiverSourceAttachment {
    pub owner: ReceiverSourceOwner,
    pub binding: ReceiverSourceBinding,
}

/// Canonical complete B response, produced by the receiver projection after
/// verifying Source's published result. Store atomically resolves the B claim.
#[derive(Clone)]
pub struct ReceiverSourcePublication {
    pub attachment: ReceiverSourceAttachment,
    pub response_json: String,
}

#[derive(Clone)]
pub struct ReceiverSourceRenewal {
    pub attachment: ReceiverSourceAttachment,
    pub lease_expires_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverSourceWrite {
    Applied,
    Replay,
    Refused,
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
