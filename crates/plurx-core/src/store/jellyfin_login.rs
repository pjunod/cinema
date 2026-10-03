//! Compatibility login scope is separate from native human device labels.
use super::{CacheAdminMutationClaim, MAX_DEVICE_LABEL_BYTES};
use crate::error::StoreError;
use async_trait::async_trait;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JellyfinClientFamily {
    Infuse,
    AndroidTv,
}
impl JellyfinClientFamily {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Infuse => "infuse",
            Self::AndroidTv => "android_tv",
        }
    }
}
/// Digests and native password CAS material only; never a plaintext token.
/// Intentionally has no Debug/Serialize implementation.
#[derive(Clone)]
pub struct JellyfinLoginWrite {
    pub token_hash: String,
    pub user_id: i64,
    pub device_digest: String,
    pub client_family: JellyfinClientFamily,
    pub device_label: Option<String>,
    pub expected_password_hash: String,
    pub created_at: i64,
}
#[async_trait]
pub trait JellyfinLoginStore: Send + Sync {
    /// Atomically mint the new native user token, retire the previous token in
    /// this compatibility scope, and publish the scope mapping. A stale
    /// password or absent exact exclusion claim changes no login authority.
    async fn replace_jellyfin_login(
        &self,
        write: JellyfinLoginWrite,
        claim: Option<&CacheAdminMutationClaim>,
    ) -> Result<bool, StoreError>;
}
pub(crate) fn validate_write(w: &JellyfinLoginWrite) -> Result<(), StoreError> {
    let digest = |s: &str| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    if w.user_id <= 0
        || w.created_at < 0
        || !digest(&w.token_hash)
        || !digest(&w.device_digest)
        || w.expected_password_hash.is_empty()
        || w.expected_password_hash.len() > 1024
        || w.device_label
            .as_ref()
            .is_some_and(|s| s.len() > MAX_DEVICE_LABEL_BYTES)
    {
        return Err(StoreError::Credential(
            "invalid compatibility login write".into(),
        ));
    }
    Ok(())
}
pub(crate) const JELLYFIN_LOGIN_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS jellyfin_login_tokens (
 token_hash TEXT PRIMARY KEY REFERENCES tokens(token_hash) ON DELETE CASCADE,
 user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 device_digest TEXT NOT NULL CHECK(length(device_digest)=64),
 client_family TEXT NOT NULL CHECK(client_family IN ('infuse','android_tv')),
 UNIQUE(user_id,device_digest,client_family)
) STRICT;
"#;
