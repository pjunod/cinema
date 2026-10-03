//! Compatibility login scope is separate from native human device labels.
use super::{CacheAdminMutationClaim, MAX_DEVICE_LABEL_BYTES};
use crate::error::StoreError;
use async_trait::async_trait;
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct JellyfinCompatibilityState {
    pub enabled: bool,
    /// Every explicit save creates a fresh generation, including off/on cycles.
    pub generation: Option<String>,
}
#[async_trait]
pub trait JellyfinLoginStore: Send + Sync {
    async fn jellyfin_compatibility_state(&self) -> Result<JellyfinCompatibilityState, StoreError>;
    /// Save the explicit choice and its generation atomically; readiness is advisory.
    async fn set_jellyfin_compatibility(&self, enabled: bool) -> Result<(), StoreError>;
    /// Public facade minting additionally checks the exact enabled generation
    /// in the same mutation as password CAS and scoped replacement.
    async fn replace_jellyfin_login_if_enabled(
        &self,
        write: JellyfinLoginWrite,
        claim: Option<&CacheAdminMutationClaim>,
        generation: &str,
    ) -> Result<bool, StoreError>;
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

pub(crate) const SWITCH_STATE: &str = "SELECT json_object('enabled',json(CASE WHEN lower(trim(COALESCE((SELECT value FROM settings WHERE key='compat.jellyfin.enabled'),''),char(9)||char(10)||char(11)||char(12)||char(13)||' ')) IN ('1','true','yes','on') THEN 'true' ELSE 'false' END),'generation',(SELECT value FROM settings WHERE key='compat.jellyfin.generation')) AS result_json";
pub(crate) const SAVE_GENERATION: &str = "INSERT INTO settings(key,value,updated_at) VALUES('compat.jellyfin.generation',$1,$2) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at";
pub(crate) const SAVE_SWITCH: &str = "INSERT INTO settings(key,value,updated_at) VALUES('compat.jellyfin.enabled',$1,$2) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at";
pub(crate) fn validate_generation(generation: &str) -> Result<(), StoreError> {
    if generation.len() != 32
        || !generation
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || generation.bytes().all(|b| b == b'0')
    {
        return Err(StoreError::Credential(
            "invalid compatibility generation".into(),
        ));
    }
    Ok(())
}

pub(crate) fn switch_save_time() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
