//! Password hashing and login tokens.
//!
//! Passwords are hashed with Argon2id (REQ-USER-1). Login tokens are random
//! 256-bit values; only their SHA-256 hash is persisted, so a database leak
//! never exposes a usable token (defense in depth alongside the plurx "no
//! cloud, no shared secret" posture).

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use sha2::{Digest, Sha256};

use crate::error::AuthError;

/// Hash a password for storage. Returns a PHC string (algorithm + params +
/// salt + hash) suitable for [`verify_password`].
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    let mut salt_bytes = [0u8; 16];
    getrandom::getrandom(&mut salt_bytes).map_err(|e| AuthError::Rng(e.to_string()))?;
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|e| AuthError::Hash(e.to_string()))?;
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| AuthError::Hash(e.to_string()))?;
    Ok(hash.to_string())
}

/// Verify a password against a stored PHC hash. Returns false on any parse or
/// mismatch — callers get a plain yes/no and cannot distinguish the reason.
pub fn verify_password(password: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Generate a fresh opaque login token (hex-encoded 256-bit random value).
/// Hand this to the client; store only [`hash_token`] of it.
pub fn generate_token() -> Result<String, AuthError> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| AuthError::Rng(e.to_string()))?;
    Ok(hex::encode(bytes))
}

/// Prefix that marks a secret as an API key rather than a login token. The
/// extractor routes on it, so a key and a token can share one header without
/// either being tried against the other's table.
pub const API_KEY_PREFIX: &str = "plx_";

/// Minimum age of an authentication activity timestamp before it is refreshed.
///
/// Authentication still performs an authority-consistent credential lookup on
/// every request. This window only coalesces the best-effort activity write so
/// repeated requests do not each append a Raft log entry.
pub const ACTIVITY_REFRESH_SECS: i64 = 60;

/// Whether an authentication activity timestamp is old enough to refresh.
pub fn activity_refresh_due(last_activity_at: Option<i64>, now: i64) -> bool {
    last_activity_at.is_none_or(|last| last < now.saturating_sub(ACTIVITY_REFRESH_SECS))
}

/// Default idle lifetime of a login token while "Sign-ins expire" is on.
pub const TOKEN_IDLE_DAYS_DEFAULT: i64 = 90;
/// The shortest window an administrator may choose. One day keeps the
/// sliding window far wider than both the 60-second activity coalescing
/// ([`ACTIVITY_REFRESH_SECS`]) and the five-minute cache-only admin proof, so
/// a token that just authenticated can never expire while a proof it
/// published is still live.
pub const TOKEN_IDLE_DAYS_MIN: i64 = 1;
/// Ten years: long enough to mean "practically never" without overflowing.
pub const TOKEN_IDLE_DAYS_MAX: i64 = 3_650;
const SECONDS_PER_DAY: i64 = 86_400;

/// Parse a stored idle-window setting. Absent, unparseable or out-of-range
/// values fall back to the default rather than to a surprise.
pub fn token_idle_days(stored: Option<&str>) -> i64 {
    stored
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|days| (TOKEN_IDLE_DAYS_MIN..=TOKEN_IDLE_DAYS_MAX).contains(days))
        .unwrap_or(TOKEN_IDLE_DAYS_DEFAULT)
}

/// The server-wide sign-in expiry policy while it is in force.
///
/// Expiry is a sliding window measured from the token's `last_seen_at` — the
/// same coalesced activity timestamp authentication already maintains, so the
/// policy adds no replicated write of its own. `since` is when expiry last
/// took effect (first start of a binary with the default on, or the moment an
/// administrator switched it back on); every token's clock starts no earlier
/// than that, so enabling expiry never signs a device out on the spot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenIdlePolicy {
    pub idle_days: i64,
    pub since: i64,
}

impl TokenIdlePolicy {
    /// Resolve the policy from its three stored settings. `None` means no
    /// token can expire: either an administrator turned expiry off (the
    /// switch defaults to on), or the start of its clock has not been
    /// recorded yet — and a clock that never started cannot run out.
    pub fn from_settings(
        enabled: Option<&str>,
        idle_days: Option<&str>,
        since: Option<&str>,
    ) -> Option<Self> {
        if !crate::store::stored_switch(enabled, true) {
            return None;
        }
        let since = since?.trim().parse::<i64>().ok()?;
        Some(Self {
            idle_days: token_idle_days(idle_days),
            since,
        })
    }

    /// When a token last seen at `last_seen_at` stops authenticating.
    pub fn expires_at(&self, last_seen_at: i64) -> i64 {
        last_seen_at
            .max(self.since)
            .saturating_add(self.idle_days.saturating_mul(SECONDS_PER_DAY))
    }

    /// Whether the token has been idle for the whole window at `now`.
    pub fn is_expired(&self, last_seen_at: i64, now: i64) -> bool {
        now >= self.expires_at(last_seen_at)
    }
}

/// Generate a fresh API key secret: `plx_` + 32 hex (16 random bytes).
///
/// Shorter than a login token by design — it is pasted between machines by a
/// human, and 128 bits is far past any brute-force concern for a credential
/// that only exists on a home network. Store only [`hash_token`] of it.
pub fn generate_api_key() -> Result<String, AuthError> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| AuthError::Rng(e.to_string()))?;
    Ok(format!("{API_KEY_PREFIX}{}", hex::encode(bytes)))
}

/// Whether a presented secret looks like an API key rather than a login
/// token. Prefix only — it says which table to look in, never whether the
/// credential is valid.
pub fn is_api_key(secret: &str) -> bool {
    secret.starts_with(API_KEY_PREFIX)
}

/// SHA-256 of a token, hex-encoded — the form stored in the database and
/// looked up on each request.
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let hash = hash_password("hunter2").expect("hash");
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("hunter2", &hash));
        assert!(!verify_password("hunter3", &hash));
        assert!(!verify_password("hunter2", "not a real hash"));
    }

    #[test]
    fn salts_differ_between_hashes() {
        let a = hash_password("same").expect("a");
        let b = hash_password("same").expect("b");
        assert_ne!(a, b, "each hash must use a fresh salt");
    }

    #[test]
    fn tokens_are_unique_and_hash_stably() {
        let t1 = generate_token().expect("t1");
        let t2 = generate_token().expect("t2");
        assert_ne!(t1, t2);
        assert_eq!(t1.len(), 64); // 32 bytes hex
        assert_eq!(hash_token(&t1), hash_token(&t1));
        assert_ne!(hash_token(&t1), hash_token(&t2));
        // Known-answer: SHA-256("") — guards against accidental algo change.
        assert_eq!(
            hash_token(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn activity_refresh_is_due_only_after_the_window() {
        assert!(activity_refresh_due(None, 1_000));
        assert!(!activity_refresh_due(Some(941), 1_000));
        assert!(!activity_refresh_due(Some(940), 1_000));
        assert!(activity_refresh_due(Some(939), 1_000));
        assert!(!activity_refresh_due(Some(1_001), 1_000));
    }

    const DAY: i64 = 86_400;

    #[test]
    fn expiry_switch_defaults_on_and_off_means_no_policy() {
        let since = Some("1000");
        assert_eq!(
            TokenIdlePolicy::from_settings(None, None, since),
            Some(TokenIdlePolicy {
                idle_days: TOKEN_IDLE_DAYS_DEFAULT,
                since: 1_000
            }),
            "an absent switch is the default: on, 90 days"
        );
        for off in ["0", "false", "off", " OFF "] {
            assert_eq!(
                TokenIdlePolicy::from_settings(Some(off), Some("5"), since),
                None
            );
        }
        assert_eq!(
            TokenIdlePolicy::from_settings(Some("1"), Some("30"), since).map(|p| p.idle_days),
            Some(30)
        );
    }

    #[test]
    fn a_clock_that_never_started_cannot_run_out() {
        assert_eq!(TokenIdlePolicy::from_settings(Some("1"), None, None), None);
        assert_eq!(
            TokenIdlePolicy::from_settings(Some("1"), None, Some("x")),
            None
        );
    }

    #[test]
    fn idle_days_outside_the_bounds_fall_back_to_the_default() {
        assert_eq!(token_idle_days(Some("0")), TOKEN_IDLE_DAYS_DEFAULT);
        assert_eq!(token_idle_days(Some("-3")), TOKEN_IDLE_DAYS_DEFAULT);
        assert_eq!(token_idle_days(Some("3651")), TOKEN_IDLE_DAYS_DEFAULT);
        assert_eq!(token_idle_days(Some("nope")), TOKEN_IDLE_DAYS_DEFAULT);
        assert_eq!(token_idle_days(Some(" 1 ")), 1);
        assert_eq!(token_idle_days(Some("3650")), 3_650);
    }

    #[test]
    fn the_window_slides_from_last_use_and_starts_no_earlier_than_since() {
        let policy = TokenIdlePolicy {
            idle_days: 90,
            since: 1_000 * DAY,
        };
        // Seen long before expiry took effect: the clock starts at `since`,
        // so enabling expiry does not sign this device out on the spot.
        assert_eq!(policy.expires_at(3), 1_090 * DAY);
        assert!(!policy.is_expired(3, 1_000 * DAY + 1));
        assert!(!policy.is_expired(3, 1_090 * DAY - 1));
        assert!(policy.is_expired(3, 1_090 * DAY));
        // Seen after `since`: the window slides from the last use.
        assert_eq!(policy.expires_at(1_050 * DAY), 1_140 * DAY);
        assert!(!policy.is_expired(1_050 * DAY, 1_139 * DAY));
        assert!(policy.is_expired(1_050 * DAY, 1_140 * DAY));
    }
}
