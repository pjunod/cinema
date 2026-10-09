use crate::{Error, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const VERSION: &str = "cinema.invitation.v1";
pub const MAX_BODY: usize = 64 * 1024;
pub const MAX_SAFE: u64 = 9_007_199_254_740_991;
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Apple,
    Android,
}
impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Apple => "apple",
            Self::Android => "android",
        }
    }
}

pub fn uuid(value: &str) -> Result<Uuid> {
    let parsed = Uuid::parse_str(value).map_err(|_| Error::invalid())?;
    if parsed.hyphenated().to_string() != value {
        return Err(Error::invalid());
    }
    Ok(parsed)
}
pub fn label(value: &str, max: usize) -> Result<()> {
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(Error::invalid());
    }
    Ok(())
}
pub fn safe(value: u64) -> Result<()> {
    if value == 0 || value > MAX_SAFE {
        return Err(Error::invalid());
    }
    Ok(())
}
pub fn expires(value: i64, now: i64) -> Result<()> {
    if value <= now || value <= 0 || value as u64 > MAX_SAFE || value - now > 120 {
        return Err(Error::invalid());
    }
    Ok(())
}
pub fn proof_hash(secret: &str) -> Result<[u8; 32]> {
    if secret.len() != 43 {
        return Err(Error::new(401, "unauthorized"));
    }
    let raw = URL_SAFE_NO_PAD
        .decode(secret)
        .map_err(|_| Error::new(401, "unauthorized"))?;
    if raw.len() != 32 || URL_SAFE_NO_PAD.encode(&raw) != secret {
        return Err(Error::new(401, "unauthorized"));
    }
    // Matches home hash_token: SHA-256 of canonical proof TEXT, not decoded bytes.
    Ok(Sha256::digest(secret.as_bytes()).into())
}
pub fn digest(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(Error::invalid());
    }
    let raw = hex::decode(value).map_err(|_| Error::invalid())?;
    raw.try_into().map_err(|_| Error::invalid())
}
pub fn device_token(platform: Platform, value: &str) -> Result<()> {
    let valid = match platform {
        Platform::Apple => {
            !value.is_empty()
                && value.len() <= 512
                && value.len().is_multiple_of(2)
                && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        }
        Platform::Android => {
            !value.is_empty()
                && value.len() <= 4096
                && !value.bytes().any(|byte| byte <= b' ' || byte == 127)
        }
    };
    if !valid {
        return Err(Error::invalid());
    }
    Ok(())
}
pub fn invitation(value: &str, installation: &str) -> Result<()> {
    if value.len() != 43 {
        return Err(Error::invalid());
    }
    let raw = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| Error::invalid())?;
    if raw.len() != 32
        || URL_SAFE_NO_PAD.encode(&raw) != value
        || &raw[..16] != uuid(installation)?.as_bytes()
    {
        return Err(Error::invalid());
    }
    Ok(())
}
pub fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
    if body.len() > MAX_BODY {
        return Err(Error::new(413, "invalid"));
    }
    // All v1 numeric DTO fields are unsigned/positive whole numbers. Preserve
    // lexical distinctions (1e0, 1.0, -0) before serde can normalize them.
    let mut at = 0;
    let mut string = false;
    let mut escape = false;
    while at < body.len() {
        let byte = body[at];
        if string {
            if escape {
                escape = false;
            } else if byte == b'\\' {
                escape = true;
            } else if byte == b'"' {
                string = false;
            }
            at += 1;
            continue;
        }
        if byte == b'"' {
            string = true;
            at += 1;
            continue;
        }
        if byte == b'-' || byte.is_ascii_digit() {
            let start = at;
            while at < body.len()
                && (body[at].is_ascii_digit()
                    || matches!(body[at], b'-' | b'+' | b'.' | b'e' | b'E'))
            {
                at += 1;
            }
            let number = &body[start..at];
            if number.iter().any(|value| !value.is_ascii_digit())
                || number.len() > 16
                || std::str::from_utf8(number)
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .is_none_or(|value| value > MAX_SAFE)
            {
                return Err(Error::invalid());
            }
            continue;
        }
        at += 1;
    }
    serde_json::from_slice(body).map_err(|_| Error::invalid())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Ticket {
    pub version: String,
    pub ticket_id: String,
    pub ticket_secret_hash: String,
    pub server_instance_id: String,
    pub installation_id: String,
    pub receiver_id: String,
    pub consent_id: String,
    pub phone_generation: u64,
    pub consent_generation: u64,
    pub transport_generation: u64,
    pub platform: Platform,
}
impl Ticket {
    pub fn validate(&self) -> Result<()> {
        if self.version != VERSION {
            return Err(Error::invalid());
        }
        for id in [
            &self.ticket_id,
            &self.installation_id,
            &self.receiver_id,
            &self.consent_id,
        ] {
            uuid(id)?;
        }
        digest(&self.ticket_secret_hash)?;
        label(&self.server_instance_id, 128)?;
        for value in [
            self.phone_generation,
            self.consent_generation,
            self.transport_generation,
        ] {
            safe(value)?;
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub version: String,
    pub ticket_id: String,
    pub platform: Platform,
    pub device_token: String,
}
impl Claim {
    pub fn validate(&self) -> Result<()> {
        if self.version != VERSION {
            return Err(Error::invalid());
        }
        uuid(&self.ticket_id)?;
        device_token(self.platform, &self.device_token)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub version: String,
    pub ticket_id: String,
}
impl Status {
    pub fn validate(&self) -> Result<()> {
        if self.version != VERSION {
            return Err(Error::invalid());
        }
        uuid(&self.ticket_id)?;
        Ok(())
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub version: String,
    pub enrollment_id: String,
    pub installation_id: String,
    pub phone_generation: u64,
    pub consent_generation: u64,
    pub transport_generation: u64,
    pub invitation_id: String,
    pub expires_at: i64,
}
impl Delivery {
    pub fn validate(&self, now: i64) -> Result<()> {
        if self.version != VERSION {
            return Err(Error::invalid());
        }
        uuid(&self.enrollment_id)?;
        uuid(&self.installation_id)?;
        invitation(&self.invitation_id, &self.installation_id)?;
        expires(self.expires_at, now)?;
        for value in [
            self.phone_generation,
            self.consent_generation,
            self.transport_generation,
        ] {
            safe(value)?;
        }
        Ok(())
    }
    pub fn matches(&self, ticket: &Ticket) -> bool {
        self.enrollment_id == ticket.ticket_id
            && self.installation_id == ticket.installation_id
            && self.phone_generation == ticket.phone_generation
            && self.consent_generation == ticket.consent_generation
            && self.transport_generation == ticket.transport_generation
    }
}
#[derive(Serialize)]
pub struct TicketIssued {
    pub version: &'static str,
    pub ticket_id: String,
    pub expires_at: i64,
    pub status: &'static str,
}
#[derive(Serialize)]
pub struct TicketStatus {
    pub version: &'static str,
    pub ticket_id: String,
    pub status: &'static str,
    pub expires_at: i64,
    pub enrollment_id: Option<String>,
    pub server_instance_id: String,
    pub installation_id: String,
    pub receiver_id: String,
    pub consent_id: String,
    pub phone_generation: u64,
    pub consent_generation: u64,
    pub transport_generation: u64,
    pub platform: Platform,
}
#[derive(Serialize)]
pub struct Outcome {
    pub version: &'static str,
    pub status: &'static str,
}
impl Outcome {
    pub fn new(status: &'static str) -> Self {
        Self {
            version: VERSION,
            status,
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.device_token.zeroize();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_whole_body_and_proof_lexemes() {
        for number in ["1.0", "1e0", "-0", "9007199254740992"] {
            assert!(parse::<serde_json::Value>(
                format!("{{\"nested\":{{\"value\":{number}}}}}").as_bytes()
            )
            .is_err());
        }
        assert!(parse::<Status>(
            br#"{"version":"cinema.invitation.v1","ticket_id":"x","ticket_id":"y"}"#
        )
        .is_err());
        assert!(parse::<Status>(
            br#"{"version":"cinema.invitation.v1","ticket_id":"x","extra":true}"#
        )
        .is_err());
        assert!(parse::<serde_json::Value>(br#"{"text":"1e0 -0 1.0"}"#).is_ok());
        let proof = URL_SAFE_NO_PAD.encode([7; 32]);
        assert_eq!(
            proof_hash(&proof).ok(),
            Some(Sha256::digest(proof.as_bytes()).into())
        );
        for bad in [
            format!(" {proof}"),
            format!("{proof}="),
            format!("{proof}\n"),
            "AA".into(),
        ] {
            assert!(proof_hash(&bad).is_err());
        }
    }
}
