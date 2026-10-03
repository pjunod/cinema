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
    let (scheme, rest) = value.split_once(' ').ok_or(CredentialError::Malformed)?;
    if scheme.eq_ignore_ascii_case("bearer") {
        return merge(rest, token);
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
            if name.eq_ignore_ascii_case("token") {
                merge(field.trim(), token)?;
            }
            rest = remaining.trim_start();
            if rest.is_empty() && tail.contains(',') {
                return Err(CredentialError::Malformed);
            }
            continue;
        };
        if name.eq_ignore_ascii_case("token") {
            merge(field, token)?;
        }
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
