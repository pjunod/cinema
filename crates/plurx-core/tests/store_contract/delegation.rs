//! The same scoped-login contract runs against standalone and replicated stores.
use super::*;
use plurx_core::store::{
    delegation::{DelegationClient, DelegationCode, DelegationRedemption},
    JellyfinEntityKind, TokenAuthentication,
};
fn hash(s: &str) -> String {
    plurx_core::auth::hash_token(s)
}
fn client() -> DelegationClient {
    DelegationClient {
        id: "curator-client".into(),
        name: "Curator".into(),
        redirect: "https://curator.example/api/v1/auth/cinema/callback".into(),
        generation: 1,
    }
}
fn code(user: i64, subject: &str, label: &str) -> DelegationCode {
    DelegationCode {
        code_hash: hash(label),
        client_id: client().id,
        user_id: user,
        subject: subject.into(),
        source_hash: hash("native-source"),
        challenge: "verified-s256-challenge".into(),
    }
}
fn redeem(label: &str, grant: &str, claim: &str) -> DelegationRedemption {
    DelegationRedemption {
        code_hash: hash(label),
        client_id: client().id,
        secret_hash: hash("client-secret"),
        redirect: client().redirect,
        challenge: "verified-s256-challenge".into(),
        grant_hash: hash(grant),
        claim: claim.into(),
    }
}
#[tokio::test]
async fn delegation_codes_are_single_use_scoped_and_source_revocable() {
    for_each_backend(|store, backend| async move {
        let user = store
            .create_user("delegation-person", "local-password-hash", true)
            .await
            .expect("local user");
        store
            .create_token(&hash("native-source"), user.id, Some("web"))
            .await
            .expect("native source");
        let subject = store
            .jellyfin_entity_ids(JellyfinEntityKind::User, &[user.id])
            .await
            .expect("stable subject")[0]
            .wire_id
            .clone();
        store
            .delegation_register(client(), hash("client-secret"))
            .await
            .expect("register");
        assert!(store
            .delegation_mint(code(user.id, &subject, "code-one"))
            .await
            .expect("mint"));
        let mut wrong = redeem("code-one", "wrong", "wrong-claim");
        wrong.challenge = "wrong-proof".into();
        assert!(store
            .delegation_redeem(wrong)
            .await
            .expect("wrong PKCE")
            .is_none());
        let a = redeem("code-one", "grant-a", "unique-claim-a");
        let b = redeem("code-one", "grant-b", "unique-claim-b");
        let (left, right) = tokio::join!(store.delegation_redeem(a), store.delegation_redeem(b));
        let left = left.expect("claim a");
        let right = right.expect("claim b");
        assert_ne!(
            left.is_some(),
            right.is_some(),
            "exactly one claim must win: {backend}"
        );
        let grant = if left.is_some() { "grant-a" } else { "grant-b" };
        let before = store
            .list_tokens_for_user(user.id)
            .await
            .expect("source activity");
        for _ in 0..3 {
            let identity = store
                .delegation_introspect(&client().id, &hash("client-secret"), &hash(grant))
                .await
                .expect("introspect")
                .expect("active");
            assert_eq!(identity.subject, subject);
        }
        assert_eq!(
            before,
            store
                .list_tokens_for_user(user.id)
                .await
                .expect("source activity"),
            "introspection refreshed Cinema login activity"
        );
        assert!(
            matches!(
                store
                    .authenticate_token(&hash(grant))
                    .await
                    .expect("native lookup"),
                TokenAuthentication::Unknown
            ),
            "delegation grant became a Cinema login"
        );
        assert!(store
            .delegation_introspect(&client().id, &hash("wrong-secret"), &hash(grant))
            .await
            .expect("client isolation")
            .is_none());
        store
            .delete_token(&hash("native-source"))
            .await
            .expect("source logout");
        assert!(store
            .delegation_introspect(&client().id, &hash("client-secret"), &hash(grant))
            .await
            .expect("source revocation")
            .is_none());
        assert!(!store
            .delegation_mint(code(user.id, &subject, "after-logout"))
            .await
            .expect("mint after logout"));
    })
    .await;
}
#[tokio::test]
async fn delegation_disconnect_and_recreated_names_never_restore_identity() {
    for_each_backend(|store, _| async move {
        let user = store
            .create_user("same-name", "password", false)
            .await
            .expect("user");
        store
            .create_token(&hash("native-source"), user.id, None)
            .await
            .expect("source");
        let subject = store
            .jellyfin_entity_ids(JellyfinEntityKind::User, &[user.id])
            .await
            .expect("subject")[0]
            .wire_id
            .clone();
        store
            .delegation_register(client(), hash("client-secret"))
            .await
            .expect("client");
        assert!(store
            .delegation_mint(code(user.id, &subject, "code"))
            .await
            .expect("code"));
        assert!(store
            .delegation_redeem(redeem("code", "grant", "claim"))
            .await
            .expect("redeem")
            .is_some());
        store.delete_user(user.id).await.expect("delete user");
        let replacement = store
            .create_user("same-name", "another-password", true)
            .await
            .expect("recreated name");
        let new_subject = store
            .jellyfin_entity_ids(JellyfinEntityKind::User, &[replacement.id])
            .await
            .expect("new subject")[0]
            .wire_id
            .clone();
        assert_ne!(subject, new_subject);
        assert!(store
            .delegation_introspect(&client().id, &hash("client-secret"), &hash("grant"))
            .await
            .expect("deleted subject")
            .is_none());
        store
            .create_token(&hash("native-source"), replacement.id, None)
            .await
            .expect("new source");
        assert!(store
            .delegation_mint(code(replacement.id, &new_subject, "new-code"))
            .await
            .expect("new code"));
        assert!(store
            .delegation_redeem(redeem("new-code", "new-grant", "new-claim"))
            .await
            .expect("new grant")
            .is_some());
        store
            .delegation_disconnect(&client().id)
            .await
            .expect("disconnect");
        assert!(store
            .delegation_introspect(&client().id, &hash("client-secret"), &hash("new-grant"))
            .await
            .expect("revoked client")
            .is_none());
        assert!(!store
            .delegation_mint(code(replacement.id, &new_subject, "disconnected-code"))
            .await
            .expect("disconnected mint"));
    })
    .await;
}
