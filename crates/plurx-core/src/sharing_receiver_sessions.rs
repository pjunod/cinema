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

impl ReceiverSessionWriteAuthority {
    /// Finite delivery deadline from this actual Store observation. The issuing
    /// transaction still repeats the current login and exact owner guards.
    pub fn receiver_delivery_deadline(&self, owner_lease_ms: i64) -> Option<i64> {
        delivery_deadline(self.observed_at_ms, owner_lease_ms, self.login_expires_at_s)
    }
}

fn delivery_deadline(
    observed_ms: i64,
    owner_lease_ms: i64,
    login_expiry_s: Option<i64>,
) -> Option<i64> {
    if observed_ms <= 0 {
        return None;
    }
    let mut deadline = observed_ms.checked_add(30_000)?.min(owner_lease_ms);
    if let Some(expiry) = login_expiry_s {
        deadline = deadline.min(expiry.checked_mul(1_000)?);
    }
    (deadline > observed_ms).then_some(deadline)
}

#[cfg(test)]
mod deadline_tests {
    use super::delivery_deadline;

    #[test]
    fn receiver_delivery_deadline_respects_original_login_owner_and_checked_clock() {
        assert_eq!(delivery_deadline(10_000, 50_000, Some(12)), Some(12_000));
        assert_eq!(delivery_deadline(10_000, 50_000, None), Some(40_000));
        assert_eq!(delivery_deadline(10_000, 20_000, None), Some(20_000));
        assert_eq!(delivery_deadline(10_000, 10_000, None), None);
        assert_eq!(delivery_deadline(10_000, 50_000, Some(10)), None);
        assert_eq!(delivery_deadline(i64::MAX - 29_999, i64::MAX, None), None);
        assert_eq!(delivery_deadline(10_000, 50_000, Some(i64::MAX)), None);
        assert_eq!(delivery_deadline(0, 50_000, None), None);
    }
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

/// Current-authorized durable B metadata for actor recovery. An attached
/// blocked owner has no response; a published owner retains its resolved
/// canonical B response. The actor must still open the envelope and obtain
/// actual current Source evidence before physical or delivery work.
#[derive(Clone)]
pub struct ReceiverSourceSnapshot {
    pub binding: ReceiverSourceBinding,
    pub response_json: Option<String>,
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
