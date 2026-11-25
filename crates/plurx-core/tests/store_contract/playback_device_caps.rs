//! A signed-in device's capability record on both backends (Curator ADR 0031
//! §5): written once per change, listed newest first over live tokens, and
//! gone with its token on every path that removes one.
use super::*;

fn digest(label: &str) -> String {
    plurx_core::auth::hash_token(label)
}

async fn listed(store: &Arc<dyn Store>, user_id: i64) -> Vec<String> {
    store
        .device_caps_for_user(user_id)
        .await
        .expect("list devices")
        .into_iter()
        .map(|record| record.device.unwrap_or_default())
        .collect()
}

/// The record follows the token: logout, a revoked device, a password reset,
/// revoking every sign-in and deleting the account each leave no row, with
/// no step of their own.
#[tokio::test]
async fn a_device_capability_record_lives_and_dies_with_its_sign_in() {
    for_each_backend(|store, backend| async move {
        let user = store
            .create_user("device-caps", "hash", false)
            .await
            .expect("create user");
        let other = store
            .create_user("device-caps-other", "hash", false)
            .await
            .expect("create other user");
        for (token, device) in [
            ("tv", "Living room"),
            ("phone", "Phone"),
            ("tablet", "Tablet"),
        ] {
            store
                .create_token(&digest(token), user.id, Some(device))
                .await
                .expect("create token");
        }
        store
            .create_token(&digest("other"), other.id, Some("Other"))
            .await
            .expect("create other token");

        assert!(store
            .put_device_caps(&digest("tv"), "{\"v\":2}", 10)
            .await
            .expect("record tv"));
        assert!(
            !store
                .put_device_caps(&digest("tv"), "{\"v\":2}", 11)
                .await
                .expect("unchanged"),
            "{backend}: an unchanged document is not written"
        );
        assert!(store
            .put_device_caps(&digest("tv"), "{\"v\":2,\"audio\":[]}", 12)
            .await
            .expect("changed"));
        assert!(
            !store
                .put_device_caps(&digest("never-issued"), "{}", 12)
                .await
                .expect("unknown token"),
            "{backend}: no token, no record"
        );
        for token in ["phone", "tablet", "other"] {
            store
                .put_device_caps(&digest(token), "{}", 13)
                .await
                .expect("record");
        }
        let records = store
            .device_caps_for_user(user.id)
            .await
            .expect("list devices");
        assert_eq!(records.len(), 3, "{backend}");
        let tv = records
            .iter()
            .find(|record| record.device.as_deref() == Some("Living room"))
            .expect("tv record");
        assert_eq!(tv.token_prefix, digest("tv")[..8], "{backend}");
        assert_eq!(tv.caps, "{\"v\":2,\"audio\":[]}", "{backend}");
        assert_eq!(tv.updated_at, 12, "{backend}");
        assert_eq!(tv.username, "device-caps", "{backend}");
        assert_eq!(
            store
                .device_caps_for_all_users()
                .await
                .expect("list all")
                .len(),
            4,
            "{backend}"
        );

        // Logout (and a revoked device: both delete the token row).
        assert!(store.delete_token(&digest("tv")).await.expect("logout"));
        let mut left = listed(&store, user.id).await;
        left.sort();
        assert_eq!(left, ["Phone", "Tablet"], "{backend}");

        // A password reset revokes every sign-in, and every record with them.
        assert!(store
            .reset_password_and_revoke_tokens(user.id, "new-hash", None)
            .await
            .expect("reset"));
        assert!(listed(&store, user.id).await.is_empty(), "{backend}");

        // Revoking every sign-in by hand does the same.
        store
            .create_token(&digest("tv-again"), user.id, Some("Living room"))
            .await
            .expect("sign in again");
        store
            .put_device_caps(&digest("tv-again"), "{}", 20)
            .await
            .expect("record");
        store
            .delete_tokens_for_user(user.id)
            .await
            .expect("revoke all");
        assert!(listed(&store, user.id).await.is_empty(), "{backend}");

        // Deleting the account takes its records too; nobody else's.
        assert!(store.delete_user(other.id).await.expect("delete user"));
        assert!(
            store
                .device_caps_for_all_users()
                .await
                .expect("list all")
                .is_empty(),
            "{backend}"
        );
    })
    .await;
}
