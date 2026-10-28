//! Playback handoff codes and tokens on both backends (Linux player plan
//! §3.1, M1): single use, the token's audience and scope, revocation with the
//! minting token, expiry, and pruning.
use super::*;
use plurx_core::store::{
    NewPlaybackHandoff, PlaybackHandoffRedeemOutcome, PlaybackHandoffRedemption, TokenAudience,
    TokenAuthentication,
};

const DEVICE: &str = "plurx Player (Linux)";

fn digest(label: &str) -> String {
    plurx_core::auth::hash_token(label)
}

fn handoff(id: &str, user_id: i64, file_id: i64, minter: &str, now: i64) -> NewPlaybackHandoff {
    NewPlaybackHandoff {
        id: id.to_owned(),
        code_hash: digest(&format!("code-{id}")),
        user_id,
        file_id,
        source_token_hash: minter.to_owned(),
        start_ms: Some(42_000),
        audio: Some(1),
        subtitle: None,
        origin: Some("http://media1:8096".into()),
        created_at: now,
        expires_at: now + 120,
    }
}

fn redemption(id: &str, token: &str, now: i64, token_expires_at: i64) -> PlaybackHandoffRedemption {
    PlaybackHandoffRedemption {
        code_hash: digest(&format!("code-{id}")),
        token_hash: token.to_owned(),
        device: Some(DEVICE.into()),
        now,
        token_expires_at,
    }
}

/// The audiences a token digest authenticates under.
async fn audiences(store: &Arc<dyn Store>, token: &str) -> Vec<TokenAudience> {
    let mut found = Vec::new();
    for audience in [
        TokenAudience::Native,
        TokenAudience::JellyfinCompatibility,
        TokenAudience::PlaybackHandoff,
    ] {
        if let TokenAuthentication::Authenticated(_) = store
            .authenticate_token_for(token, audience)
            .await
            .expect("authenticate")
        {
            found.push(audience);
        }
    }
    found
}

async fn lists_device(store: &Arc<dyn Store>, user_id: i64) -> bool {
    store
        .list_tokens_for_user(user_id)
        .await
        .expect("inventory")
        .iter()
        .any(|row| row.device.as_deref() == Some(DEVICE))
}

#[tokio::test]
async fn playback_handoff_redeems_once_into_a_file_scoped_audience() {
    for_each_backend(|store, backend| async move {
        let (uid, file) = seed_file(&store, "playback-handoff").await;
        let minter = digest("handoff-minter");
        store
            .create_token(&minter, uid, Some("web"))
            .await
            .expect("minting token");
        let now = unix_seconds();

        // A code is recorded only while its minting token exists.
        assert!(!store
            .create_playback_handoff(handoff("orphan", uid, file, &digest("absent"), now))
            .await
            .expect("orphan mint"));
        assert!(store
            .create_playback_handoff(handoff("h1", uid, file, &minter, now))
            .await
            .expect("mint"));
        let status = store
            .playback_handoff_for_minter("h1", uid, &minter)
            .await
            .expect("status")
            .expect("minter sees it");
        assert_eq!(status.redeemed_at, None, "{backend}");
        assert!(store
            .playback_handoff_for_minter("h1", uid, &digest("other-session"))
            .await
            .expect("status")
            .is_none());

        let token = digest("handoff-token");
        let expires = now + 6 * 60 * 60;
        let redeemed = store
            .redeem_playback_handoff(&redemption("h1", &token, now, expires))
            .await
            .expect("redeem");
        let PlaybackHandoffRedeemOutcome::Redeemed(redeemed) = redeemed else {
            panic!("{backend}: first redemption must win, got {redeemed:?}");
        };
        assert_eq!(
            (
                redeemed.id.as_str(),
                redeemed.file_id,
                redeemed.start_ms,
                redeemed.audio
            ),
            ("h1", file, Some(42_000), Some(1))
        );
        assert_eq!(redeemed.origin.as_deref(), Some("http://media1:8096"));
        assert!(store
            .playback_handoff_for_minter("h1", uid, &minter)
            .await
            .expect("status")
            .expect("still visible")
            .redeemed_at
            .is_some());

        // Single use: a second redemption mints nothing at all.
        let second = digest("handoff-token-2");
        assert_eq!(
            store
                .redeem_playback_handoff(&redemption("h1", &second, now, expires))
                .await
                .expect("second redeem"),
            PlaybackHandoffRedeemOutcome::Gone,
            "{backend}"
        );
        assert!(audiences(&store, &second).await.is_empty(), "{backend}");
        assert_eq!(
            store
                .redeem_playback_handoff(&redemption("never", &second, now, expires))
                .await
                .expect("unknown redeem"),
            PlaybackHandoffRedeemOutcome::Unknown
        );

        // The token is its own audience only; the minter stays native.
        assert_eq!(
            audiences(&store, &token).await,
            [TokenAudience::PlaybackHandoff],
            "{backend}"
        );
        assert_eq!(audiences(&store, &minter).await, [TokenAudience::Native]);
        // It is not a row in `tokens` at all, so no native lookup — this
        // binary's or an older one's — can find it.
        assert!(
            !store.delete_token(&token).await.expect("tokens lookup"),
            "{backend}: a handoff token must not be a tokens row"
        );
        assert_eq!(
            audiences(&store, &token).await,
            [TokenAudience::PlaybackHandoff]
        );
        let scope = store
            .playback_handoff_token_scope(&token, now)
            .await
            .expect("scope")
            .expect("live scope");
        assert_eq!(
            (
                scope.handoff_id.as_str(),
                scope.user_id,
                scope.file_id,
                scope.expires_at
            ),
            ("h1", uid, file, expires)
        );
        assert!(lists_device(&store, uid).await, "{backend}: sessions list");

        // Revoking the minting token revokes the handoff token with it, and
        // it leaves the sessions list at once.
        assert!(store.delete_token(&minter).await.expect("logout"));
        assert!(audiences(&store, &token).await.is_empty(), "{backend}");
        assert!(store
            .playback_handoff_token_scope(&token, now)
            .await
            .expect("scope")
            .is_none());
        assert!(!lists_device(&store, uid).await, "{backend}");

        // A code whose minter is gone cannot be redeemed.
        let minter2 = digest("handoff-minter-2");
        store
            .create_token(&minter2, uid, Some("web"))
            .await
            .expect("second minting token");
        assert!(store
            .create_playback_handoff(handoff("h2", uid, file, &minter2, now))
            .await
            .expect("mint"));
        assert!(store.delete_token(&minter2).await.expect("logout"));
        assert_eq!(
            store
                .redeem_playback_handoff(&redemption("h2", &digest("t2"), now, expires))
                .await
                .expect("redeem revoked"),
            PlaybackHandoffRedeemOutcome::Gone
        );

        // An expired code is gone; an expired token authenticates nobody.
        let minter3 = digest("handoff-minter-3");
        store
            .create_token(&minter3, uid, Some("web"))
            .await
            .expect("third minting token");
        let mut stale = handoff("h3", uid, file, &minter3, now - 200);
        stale.expires_at = now - 80;
        assert!(store.create_playback_handoff(stale).await.expect("mint"));
        assert_eq!(
            store
                .redeem_playback_handoff(&redemption("h3", &digest("t3"), now, expires))
                .await
                .expect("redeem expired"),
            PlaybackHandoffRedeemOutcome::Gone
        );
        assert!(store
            .create_playback_handoff(handoff("h4", uid, file, &minter3, now))
            .await
            .expect("mint"));
        let short = digest("t4");
        assert!(matches!(
            store
                .redeem_playback_handoff(&redemption("h4", &short, now, now - 1))
                .await
                .expect("redeem"),
            PlaybackHandoffRedeemOutcome::Redeemed(_)
        ));
        assert!(audiences(&store, &short).await.is_empty(), "{backend}");
        assert!(store
            .playback_handoff_token_scope(&short, now)
            .await
            .expect("scope")
            .is_none());

        // Pruning deletes the dead tokens (revoked minter, expired) and the
        // stale codes, never the live minting token.
        let pruned = store
            .prune_playback_handoffs(now, now)
            .await
            .expect("prune");
        assert!(pruned >= 3, "{backend}: pruned {pruned}");
        assert_eq!(audiences(&store, &minter3).await, [TokenAudience::Native]);
        for dead in [&token, &short] {
            assert!(audiences(&store, dead).await.is_empty());
        }
        assert!(!lists_device(&store, uid).await);

        // Revocation from the sessions list (by digest prefix) and by the
        // player itself.
        assert!(store
            .create_playback_handoff(handoff("h5", uid, file, &minter3, now))
            .await
            .expect("mint"));
        let listed = digest("t5");
        assert!(matches!(
            store
                .redeem_playback_handoff(&redemption("h5", &listed, now, expires))
                .await
                .expect("redeem"),
            PlaybackHandoffRedeemOutcome::Redeemed(_)
        ));
        assert!(lists_device(&store, uid).await, "{backend}");
        assert!(store
            .revoke_playback_handoff_token_by_prefix(uid, &listed[..8])
            .await
            .expect("revoke by prefix"));
        assert!(audiences(&store, &listed).await.is_empty(), "{backend}");
        assert!(!store
            .revoke_playback_handoff_token(&listed)
            .await
            .expect("already revoked"));
        assert!(!store
            .revoke_playback_handoff_token_by_prefix(uid, &minter3[..8])
            .await
            .expect("a login token prefix is not a handoff token"));
        assert_eq!(audiences(&store, &minter3).await, [TokenAudience::Native]);
    })
    .await;
}

#[tokio::test]
async fn playback_handoff_advances_only_a_live_expected_scope_without_extending_it() {
    for_each_backend(|store, backend| async move {
        let (uid, file) = seed_file(&store, "handoff-advance").await;
        let minter = digest("advance-minter");
        store
            .create_token(&minter, uid, Some("web"))
            .await
            .expect("minter");
        let now = unix_seconds();
        let expires = now + 60;
        for id in ["advance", "revoked", "expired", "logout"] {
            assert!(store
                .create_playback_handoff(handoff(id, uid, file, &minter, now))
                .await
                .expect("mint"));
            store
                .redeem_playback_handoff(&redemption(id, &digest(id), now, expires))
                .await
                .expect("redeem");
        }
        let token = digest("advance");
        assert!(!store
            .advance_playback_handoff(&digest("unknown"), file, file + 1, now)
            .await
            .expect("unknown"));
        assert!(!store
            .advance_playback_handoff(&token, file + 1, file + 2, now)
            .await
            .expect("wrong predecessor"));
        assert!(
            store
                .advance_playback_handoff(&token, file, file + 1, now)
                .await
                .expect("advance"),
            "{backend}"
        );
        assert!(!store
            .advance_playback_handoff(&token, file, file + 2, now)
            .await
            .expect("replay"));
        let scope = store
            .playback_handoff_token_scope(&token, now)
            .await
            .expect("scope")
            .expect("live");
        assert_eq!(
            (
                scope.file_id,
                scope.expires_at,
                scope.user_id,
                scope.handoff_id.as_str()
            ),
            (file + 1, expires, uid, "advance")
        );
        assert_eq!(
            audiences(&store, &token).await,
            [TokenAudience::PlaybackHandoff]
        );
        store
            .revoke_playback_handoff_token(&digest("revoked"))
            .await
            .expect("revoke");
        assert!(!store
            .advance_playback_handoff(&digest("revoked"), file, file + 1, now)
            .await
            .expect("revoked"));
        assert!(!store
            .advance_playback_handoff(&digest("expired"), file, file + 1, expires)
            .await
            .expect("expired"));
        store.delete_token(&minter).await.expect("logout");
        assert!(!store
            .advance_playback_handoff(&digest("logout"), file, file + 1, now)
            .await
            .expect("minter revoked"));
        assert!(
            store
                .playback_handoff_token_scope(&token, now)
                .await
                .expect("scope")
                .is_none(),
            "advance retained minter binding"
        );
    })
    .await;
}
