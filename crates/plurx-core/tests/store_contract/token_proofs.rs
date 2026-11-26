use super::*;
use plurx_core::store::{keys, TokenAudience, TokenAuthentication};

#[tokio::test]
async fn token_proof_uses_the_authoritative_audience_and_policy_read_on_each_backend() {
    for_each_backend(|store, backend| async move {
        let user = store.create_user("proof-viewer", "hash", false).await.expect("user");
        let digest = plurx_core::auth::hash_token("source-bound-token");
        store.create_token(&digest, user.id, Some("proof fixture")).await.expect("token");
        assert!(store.put_auth_token_policy(&[
            (keys::AUTH_TOKEN_EXPIRY_ENABLED, "0"),
        ], None).await.expect("initial policy"));
        let proof = store.authenticate_token_proof_for(&digest, TokenAudience::Native).await.expect("proof");
        assert!(matches!(proof.authentication, TokenAuthentication::Authenticated(ref found) if found.id == user.id));
        // SQLite has no replicated writer fence, and this contract's
        // Hiqlite client is remote (no known local image). Both authenticate
        // correctly without claiming that a reusable local proof exists.
        assert!(proof.lease.is_none(), "{backend}: unknown/local-unfenced source remains uncached");
        let wrong_audience = store.authenticate_token_proof_for(&digest, TokenAudience::JellyfinCompatibility).await.expect("wrong audience");
        assert!(matches!(wrong_audience.authentication, TokenAuthentication::Unknown));
        assert!(wrong_audience.lease.is_none());
        assert!(store.delete_token(&digest).await.expect("revoke token"));
        let revoked = store.authenticate_token_proof_for(&digest, TokenAudience::Native).await.expect("revoked proof");
        assert!(matches!(revoked.authentication, TokenAuthentication::Unknown));
        assert!(revoked.lease.is_none());
    }).await;
}
