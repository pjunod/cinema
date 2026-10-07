//! B-session delivery grant metadata; never a Source or physical-body proof.
#[cfg(feature = "hiqlite-store")]
pub use crate::cluster::membership::IngressCustodyMembers;
#[cfg(not(feature = "hiqlite-store"))]
#[derive(Clone)]
pub enum IngressCustodyMembers {}
#[cfg(not(feature = "hiqlite-store"))]
impl IngressCustodyMembers {
    pub fn write_guard(
        &self,
        _now_ms: i64,
        _members: usize,
        _cutoff: usize,
        _observed: usize,
    ) -> Result<(String, String, i64, i64), crate::error::StoreError> {
        match *self {}
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiverDeliveryGrant {
    /// SHA-256 verifier only; the raw bearer stays in the daemon.
    pub token_hash: String,
    pub deadline_ms: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverDeliveryWrite {
    Applied,
    Replay,
    Refused,
}

/// Fresh metadata authority for one ingress relay. It cannot attach, renew,
/// publish, adopt, or settle a producer; the receiver owner supplies all of
/// those physical obligations. The private binding fingerprint prevents a
/// parked response from acquiring authority over a replaced upstream.
#[derive(Clone)]
pub struct ReceiverRelayReadAuthority {
    pub(crate) owner: crate::sharing_receiver_sessions::ReceiverSourceOwner,
    pub(crate) deadline_ms: i64,
    pub(crate) binding_fingerprint: [u8; 32],
    pub(crate) authority: crate::sharing_receiver_sessions::ReceiverSessionWriteAuthority,
    pub(crate) attachment: crate::sharing_receiver_sessions::ReceiverSourceAttachment,
}
impl ReceiverRelayReadAuthority {
    pub fn owner(&self) -> &crate::sharing_receiver_sessions::ReceiverSourceOwner {
        &self.owner
    }
    pub fn deadline_ms(&self) -> i64 {
        self.deadline_ms
    }
    /// Immutable metadata identity only. Encryption rewrap and lease renewal
    /// cannot change the principal's durable outer-writer obligation identity.
    pub fn owner_identity(&self) -> String {
        hash_owner_identity(&self.owner, self.binding_fingerprint)
    }
    pub fn viewer_id(&self) -> i64 {
        self.authority.intent.user_id
    }
    pub fn binds_file(
        &self,
        reference: &crate::sharing_file_locators::FileLocatorReference,
    ) -> bool {
        let recipe = &self.authority.intent.recipe;
        recipe.reference == reference.item
            && recipe.lifecycle_generation == reference.lifecycle_generation
            && recipe.file_id == reference.file_id
            && recipe.file_revision == reference.revision
    }
    pub fn same_lineage(&self, other: &Self) -> bool {
        self.owner.incarnation_id == other.owner.incarnation_id
            && self.owner.session_id == other.owner.session_id
            && self.owner.owner_node_id == other.owner.owner_node_id
            && self.owner.owner_epoch == other.owner.owner_epoch
            && self.owner.request_id == other.owner.request_id
            && self.binding_fingerprint == other.binding_fingerprint
    }
}

/// Cleanup identity of an actual retained binding; this hash grants no media
/// admission, actor adoption or closure. Live ingress uses the opaque reader.
pub fn receiver_retained_owner_identity(
    recipe_json: &str,
    attachment: &crate::sharing_receiver_sessions::ReceiverSourceAttachment,
) -> Result<String, crate::error::StoreError> {
    let b = &attachment.binding;
    let fingerprint = serde_json::to_vec(&serde_json::json!({
        "recipe":recipe_json, "reference":b.reference, "file":b.file_id,
        "revision":b.file_revision,"request":b.source_request_id,"session":b.source_session_id,"incarnation":b.source_incarnation_id,
    })).map_err(|_| crate::sharing::invalid())?;
    use sha2::Digest;
    Ok(hash_owner_identity(
        &attachment.owner,
        sha2::Sha256::digest(fingerprint).into(),
    ))
}
fn hash_owner_identity(
    owner: &crate::sharing_receiver_sessions::ReceiverSourceOwner,
    binding_fingerprint: [u8; 32],
) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"plurx.receiver.ingress.owner.v1\0");
    digest.update(owner.incarnation_id.as_bytes());
    digest.update(owner.session_id.as_bytes());
    digest.update((owner.owner_node_id.len() as u64).to_be_bytes());
    digest.update(owner.owner_node_id.as_bytes());
    digest.update(owner.owner_epoch.to_be_bytes());
    digest.update((owner.request_id.len() as u64).to_be_bytes());
    digest.update(owner.request_id.as_bytes());
    digest.update(binding_fingerprint);
    format!("{:x}", digest.finalize())
}

/// Metadata-only contract exerciser over an actual guarded published B fixture.
#[cfg(any(test, feature = "hiqlite-contract-tests"))]
pub async fn assert_receiver_delivery_contract<
    T: crate::store::SharingReceiverDeliveryStore + ?Sized,
>(
    store: &T,
    authority: &crate::sharing_receiver_sessions::ReceiverSessionWriteAuthority,
    attachment: &crate::sharing_receiver_sessions::ReceiverSourceAttachment,
) -> ReceiverDeliveryGrant {
    use ReceiverDeliveryWrite::{Applied, Refused, Replay};
    let grant = ReceiverDeliveryGrant {
        token_hash: "e".repeat(64),
        deadline_ms: attachment
            .owner
            .now_ms
            .saturating_add(5000)
            .min(attachment.owner.lease_expires_at_ms),
    };
    assert!(store
        .receiver_delivery(authority, attachment, "bad")
        .await
        .is_err());
    assert!(store
        .issue_receiver_delivery(
            authority,
            attachment,
            &ReceiverDeliveryGrant {
                token_hash: "A".repeat(64),
                deadline_ms: grant.deadline_ms
            }
        )
        .await
        .is_err());
    let expired = ReceiverDeliveryGrant {
        token_hash: "b".repeat(64),
        deadline_ms: attachment.owner.now_ms - 1,
    };
    assert_eq!(
        store
            .issue_receiver_delivery(authority, attachment, &expired)
            .await
            .expect("no expired issuance"),
        Refused
    );
    assert_eq!(
        store
            .revoke_receiver_delivery(authority, attachment, &expired.token_hash)
            .await
            .expect("absent grant is not replay"),
        Refused
    );
    let mut wrong = attachment.clone();
    wrong.binding.source_session_id = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .issue_receiver_delivery(authority, &wrong, &grant)
            .await
            .expect("wrong binding"),
        Refused
    );
    let mut over = grant.clone();
    over.deadline_ms = attachment.owner.lease_expires_at_ms.saturating_add(1);
    assert_eq!(
        store
            .issue_receiver_delivery(authority, attachment, &over)
            .await
            .expect("lease cap"),
        Refused
    );
    assert_eq!(
        store
            .issue_receiver_delivery(authority, attachment, &grant)
            .await
            .expect("issue"),
        Applied
    );
    assert_eq!(
        store
            .issue_receiver_delivery(authority, attachment, &grant)
            .await
            .expect("exact issue retry"),
        Replay
    );
    assert_eq!(
        store
            .receiver_delivery(authority, attachment, &grant.token_hash)
            .await
            .expect("current reader"),
        Some(grant.clone())
    );
    assert!(store
        .receiver_delivery(authority, &wrong, &grant.token_hash)
        .await
        .expect("wrong tuple read")
        .is_none());
    assert_eq!(
        store
            .revoke_receiver_delivery(authority, &wrong, &grant.token_hash)
            .await
            .expect("foreign revoke"),
        Refused
    );
    let mut replacement = grant.clone();
    replacement.deadline_ms -= 1;
    assert_eq!(
        store
            .issue_receiver_delivery(authority, attachment, &replacement)
            .await
            .expect("no replacement"),
        Refused
    );
    assert_eq!(
        store
            .revoke_receiver_delivery(authority, attachment, &grant.token_hash)
            .await
            .expect("revoke"),
        Applied
    );
    assert_eq!(
        store
            .revoke_receiver_delivery(authority, attachment, &grant.token_hash)
            .await
            .expect("revoke retry"),
        Replay
    );
    assert!(store
        .receiver_delivery(authority, attachment, &grant.token_hash)
        .await
        .expect("revoked reader")
        .is_none());
    assert_eq!(
        store
            .issue_receiver_delivery(authority, attachment, &grant)
            .await
            .expect("no resurrection"),
        Refused
    );
    let active = ReceiverDeliveryGrant {
        token_hash: "f".repeat(64),
        deadline_ms: attachment.owner.lease_expires_at_ms,
    };
    assert_eq!(
        store
            .issue_receiver_delivery(authority, attachment, &active)
            .await
            .expect("retained active grant"),
        Applied
    );
    active
}
