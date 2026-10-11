//! Parse only the observed user-token carriers. Device metadata is not authority.
use std::fmt;

pub const MAX_CARRIERS: usize = 32;
pub const MAX_CREDENTIAL_BYTES: usize = 8 * 1024;
pub const MAX_HEADER_BYTES: usize = 4096;
pub const MAX_TOKEN_BYTES: usize = 512;

/// Deliberately redacted in diagnostics; expose only at the authentication call.
#[derive(Clone, PartialEq, Eq)]
pub struct UserToken(String);
impl UserToken {
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for UserToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UserToken([REDACTED])")
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    #[error("credential input exceeds its bound")]
    TooLarge,
    #[error("malformed credential carrier")]
    Malformed,
    #[error("conflicting credentials")]
    Conflict,
    #[error("scoped API keys are not user credentials")]
    ScopedKey,
}

/// Carrier names must identify their transport: headers use their ordinary
/// names; decoded query pairs use `query:api_key` or `query:apikey`. This avoids
/// treating an unrelated header or DeviceId as a credential. Preserve duplicate
/// headers/query pairs when constructing this input; collapsing a map would
/// hide conflicting credentials before this validator can reject them.
pub fn parse_user_token(carriers: &[(&str, &str)]) -> Result<Option<UserToken>, CredentialError> {
    if carriers.len() > MAX_CARRIERS {
        return Err(CredentialError::TooLarge);
    }
    let mut token = None;
    let mut bytes = 0usize;
    for &(name, value) in carriers {
        let authorization = name.eq_ignore_ascii_case("authorization")
            || name.eq_ignore_ascii_case("x-emby-authorization");
        let direct = name.eq_ignore_ascii_case("x-emby-token")
            || name.eq_ignore_ascii_case("query:api_key")
            || name.eq_ignore_ascii_case("query:apikey");
        if !authorization && !direct {
            continue;
        }
        bytes = bytes
            .checked_add(value.len())
            .ok_or(CredentialError::TooLarge)?;
        if bytes > MAX_CREDENTIAL_BYTES || value.len() > MAX_HEADER_BYTES {
            return Err(CredentialError::TooLarge);
        }
        if authorization {
            parse_authorization(value, &mut token)?;
        } else {
            merge(value, &mut token)?;
        }
    }
    Ok(token)
}
fn merge(value: &str, token: &mut Option<UserToken>) -> Result<(), CredentialError> {
    if value.len() > MAX_TOKEN_BYTES {
        return Err(CredentialError::TooLarge);
    }
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(CredentialError::Malformed);
    }
    if value
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("plx_"))
    {
        return Err(CredentialError::ScopedKey);
    }
    if token
        .as_ref()
        .is_some_and(|existing| existing.expose() != value)
    {
        return Err(CredentialError::Conflict);
    }
    if token.is_none() {
        *token = Some(UserToken(value.to_owned()));
    }
    Ok(())
}
fn parse_authorization(value: &str, token: &mut Option<UserToken>) -> Result<(), CredentialError> {
    visit_authorization(value, |name, field| {
        if name.eq_ignore_ascii_case("token") {
            merge(field, token)?;
        }
        Ok(())
    })
}
fn visit_authorization(
    value: &str,
    mut visit: impl FnMut(&str, &str) -> Result<(), CredentialError>,
) -> Result<(), CredentialError> {
    let (scheme, rest) = value.split_once(' ').ok_or(CredentialError::Malformed)?;
    if scheme.eq_ignore_ascii_case("bearer") {
        return visit("Token", rest);
    }
    if !scheme.eq_ignore_ascii_case("mediabrowser") && !scheme.eq_ignore_ascii_case("emby") {
        return Err(CredentialError::Malformed);
    }
    let mut rest = rest.trim();
    let mut fields = 0usize;
    while !rest.is_empty() {
        fields += 1;
        if fields > 16 {
            return Err(CredentialError::TooLarge);
        }
        let (name, tail) = rest.split_once('=').ok_or(CredentialError::Malformed)?;
        let name = name.trim();
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(CredentialError::Malformed);
        }
        let tail = tail.trim_start();
        let (field, remaining) = if let Some(quoted) = tail.strip_prefix('"') {
            let mut escaped = false;
            let mut end = None;
            for (index, byte) in quoted.bytes().enumerate() {
                if escaped {
                    escaped = false;
                    continue;
                }
                if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    end = Some(index);
                    break;
                }
            }
            let end = end.ok_or(CredentialError::Malformed)?;
            (&quoted[..end], quoted[end + 1..].trim_start())
        } else {
            let (field, remaining) = tail
                .split_once(',')
                .map_or((tail, ""), |(field, remaining)| (field, remaining));
            visit(name, field.trim())?;
            rest = remaining.trim_start();
            if rest.is_empty() && tail.contains(',') {
                return Err(CredentialError::Malformed);
            }
            continue;
        };
        visit(name, field)?;
        if remaining.is_empty() {
            return Ok(());
        }
        rest = remaining
            .strip_prefix(',')
            .ok_or(CredentialError::Malformed)?
            .trim_start();
        if rest.is_empty() {
            return Err(CredentialError::Malformed);
        }
    }
    Ok(())
}
/// Optional client metadata never grants authentication or administrative rights.
/// Device identifiers stay out of Debug output and are hashed before storage.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ClientIdentity {
    pub client: Option<String>,
    pub version: Option<String>,
    pub device: Option<String>,
    pub device_id: Option<String>,
}
pub fn parse_client_identity(
    carriers: &[(&str, &str)],
) -> Result<Option<ClientIdentity>, CredentialError> {
    // Validate the same carrier limits, grammar and credential conflicts first.
    parse_user_token(carriers)?;
    let mut metadata = ClientIdentity::default();
    for &(name, value) in carriers {
        if !name.eq_ignore_ascii_case("authorization")
            && !name.eq_ignore_ascii_case("x-emby-authorization")
        {
            continue;
        }
        visit_authorization(value, |name, raw| {
            let (slot, bound) = match name.to_ascii_lowercase().as_str() {
                "client" => (&mut metadata.client, 64),
                "version" => (&mut metadata.version, 64),
                "device" => (&mut metadata.device, 256),
                "deviceid" => (&mut metadata.device_id, 256),
                _ => return Ok(()),
            };
            if raw.len() > bound {
                return Err(CredentialError::TooLarge);
            }
            let mut value = String::new();
            let mut chars = raw.chars();
            while let Some(ch) = chars.next() {
                if ch == '\\' {
                    match chars.next() {
                        Some('"') => value.push('"'),
                        Some('\\') => value.push('\\'),
                        _ => return Err(CredentialError::Malformed),
                    }
                } else {
                    value.push(ch);
                }
            }
            if value.is_empty() || value.chars().any(char::is_control) {
                return Err(CredentialError::Malformed);
            }
            if slot.as_ref().is_some_and(|prior| prior != &value) {
                return Err(CredentialError::Conflict);
            }
            *slot = Some(value);
            Ok(())
        })?;
    }
    if metadata == ClientIdentity::default() {
        Ok(None)
    } else {
        Ok(Some(metadata))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credential_carriers_accept_same_secret_and_reject_conflicts_without_debug_leak() {
        let parsed = parse_user_token(&[
            (
                "Authorization",
                "MediaBrowser Client=\"Infuse\", DeviceId=\"tv\", Token=\"secret_123\"",
            ),
            ("X-Emby-Token", "secret_123"),
            ("query:ApiKey", "secret_123"),
        ])
        .expect("same carriers")
        .expect("token");
        assert_eq!(parsed.expose(), "secret_123");
        assert!(!format!("{parsed:?}").contains("secret_123"));
        assert_eq!(
            parse_user_token(&[
                ("X-Emby-Token", "secret_123"),
                ("query:api_key", "different")
            ]),
            Err(CredentialError::Conflict)
        );
        assert_eq!(
            parse_user_token(&[("Authorization", "MediaBrowser Token=\"one\", token=\"two\"")]),
            Err(CredentialError::Conflict)
        );
        assert_eq!(
            parse_user_token(&[("Authorization", "MediaBrowser DeviceId=\"secret_123\"")]),
            Ok(None)
        );
        assert_eq!(parse_user_token(&[("DeviceId", "secret_123")]), Ok(None));
    }
    #[test]
    fn credential_parser_bounds_input_and_rejects_scoped_keys_and_malformed_quotes() {
        for value in [
            "MediaBrowser Token=\"unterminated",
            "MediaBrowser Token=\"secret\" trailing",
            "MediaBrowser Token=\"secret\",",
            "MediaBrowser Device=unquoted, ",
            "Basic c2VjcmV0",
            "Bearer ",
            "MediaBrowser Token=\"bad token\"",
        ] {
            assert!(parse_user_token(&[("Authorization", value)]).is_err());
        }
        assert_eq!(
            parse_user_token(&[("query:api_key", "plx_scoped")]),
            Err(CredentialError::ScopedKey)
        );
        let huge = "a".repeat(MAX_TOKEN_BYTES + 1);
        assert_eq!(
            parse_user_token(&[("X-Emby-Token", &huge)]),
            Err(CredentialError::TooLarge)
        );
        assert_eq!(
            parse_user_token(&vec![("X-Emby-Token", "same"); MAX_CARRIERS + 1]),
            Err(CredentialError::TooLarge)
        );
        assert_eq!(
            parse_user_token(&[(
                "x-emby-authorization",
                "Emby Client=\"quoted\\\"client\", Token=\"secret\""
            )])
            .expect("escaped metadata")
            .expect("token")
            .expose(),
            "secret"
        );
    }
}

#[cfg(test)]
mod client_tests {
    use super::*;
    #[test]
    fn client_metadata_is_bounded_conflict_checked_and_never_a_credential() {
        let header =
            "MediaBrowser Client=\"Infuse-Direct\", DeviceId=\"tv-device\", Version=\"8.5.6\"";
        let metadata = parse_client_identity(&[("Authorization", header)])
            .expect("metadata")
            .expect("present");
        assert_eq!(metadata.client.as_deref(), Some("Infuse-Direct"));
        assert_eq!(metadata.device_id.as_deref(), Some("tv-device"));
        assert_eq!(
            parse_user_token(&[("Authorization", header)]).expect("credential grammar"),
            None
        );
        assert!(parse_client_identity(&[
            ("Authorization", header),
            (
                "X-Emby-Authorization",
                "MediaBrowser DeviceId=\"other-device\""
            )
        ])
        .is_err());
        assert!(parse_client_identity(&[
            (
                "Authorization",
                "MediaBrowser Client=\"Infuse\", Token=\"one\""
            ),
            ("X-Emby-Token", "other")
        ])
        .is_err());
        assert!(parse_client_identity(&[(
            "Authorization",
            &format!("MediaBrowser DeviceId=\"{}\"", "x".repeat(257))
        )])
        .is_err());
        assert!(parse_client_identity(&[(
            "Authorization",
            "MediaBrowser DeviceId=\"line\nfeed\""
        )])
        .is_err());
        assert!(
            parse_client_identity(&[("Authorization", "Bearer secret_123")])
                .expect("token only")
                .is_none()
        );
    }
}
