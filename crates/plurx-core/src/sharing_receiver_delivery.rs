//! B-session delivery grant metadata; never a Source or physical-body proof.
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
