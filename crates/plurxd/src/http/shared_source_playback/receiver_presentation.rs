//! Server-owned receiver presentation negotiation. The end-client transport
//! token alone never proves that the authenticated receiver implements ranges.
use super::{invalid, ApiError, AuthenticationHeaders, SourceStartEntry, SourceStartInput};
use axum::http::StatusCode;
use serde_json::Value;
use sha2::Digest;

pub(crate) const RELAY_TRANSPORT: &str = "plurx-internal-relay-bounded-range-v1";
pub(crate) const RESERVED_RELAY_PREFIX: &str = "plurx-internal-relay-";
pub(crate) const CLIENT_TRANSPORT: &str = "hls-bounded-range-v1";
pub(crate) const RECEIVER_HEADER: &str = "x-plurx-receiver-presentation";
pub(crate) const RECEIVER_HEADER_VALUE: &str = "bounded-range-v1";

pub(crate) fn client_supports_range(recipe: &str) -> bool {
    serde_json::from_str::<Value>(recipe)
        .ok()
        .is_some_and(|value| {
            let session = value.get("session").unwrap_or(&value);
            session
                .get("caps")
                .and_then(|caps| caps.get("transports"))
                .and_then(Value::as_array)
                .is_some_and(|values| {
                    values
                        .iter()
                        .any(|value| value.as_str() == Some(CLIENT_TRANSPORT))
                        && values.iter().any(|value| value.as_str() == Some("hls"))
                })
        })
}

/// This response-shape admission follows authenticated Source file/session
/// decoding. A bounded view needs an explicit, checked signed timeline origin.
pub(crate) fn response_presentable(
    response: &crate::http::hls::StartResponse,
    client_range: bool,
    receiver_affirmed: bool,
) -> bool {
    (response.vod && response.media_origin_ms == Some(0))
        || (client_range
            && receiver_affirmed
            && response.media_origin_ms.is_some_and(|origin| {
                (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&origin)
            }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_range_requires_hls_and_exact_public_token() {
        assert!(client_supports_range(
            r#"{"caps":{"transports":["hls","hls-bounded-range-v1"]}}"#
        ));
        assert!(client_supports_range(
            r#"{"session":{"caps":{"transports":["hls","hls-bounded-range-v1"]}}}"#
        ));
        assert!(!client_supports_range(
            r#"{"caps":{"transports":["hls","plurx-internal-relay-bounded-range-v1"]}}"#
        ));
        assert!(!client_supports_range(
            r#"{"caps":{"transports":["hls-bounded-range-v1"]}}"#
        ));
    }
}

/// Call only after current exact Source grant/reference authentication.
pub(super) fn authenticated_receiver_ready(headers: &impl AuthenticationHeaders) -> bool {
    if let Some(ready) = headers.authenticated_receiver_presentation() {
        return ready;
    }
    let mut values = headers.raw_headers().get_all(RECEIVER_HEADER).iter();
    values
        .next()
        .is_some_and(|value| value.as_bytes() == RECEIVER_HEADER_VALUE.as_bytes())
        && values.next().is_none()
}
fn transport_values(value: &Value) -> Option<&Vec<Value>> {
    value
        .get("session")
        .unwrap_or(value)
        .get("caps")?
        .get("transports")?
        .as_array()
}
pub(super) fn has_reserved_tokens(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .is_some_and(|value| {
            transport_values(&value).into_iter().flatten().any(|value| {
                value
                    .as_str()
                    .is_some_and(|token| token.starts_with(RESERVED_RELAY_PREFIX))
            })
        })
}
pub(super) fn internal_range(bytes: &[u8]) -> Result<bool, ApiError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let reserved: Vec<_> = transport_values(&value)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|token| token.starts_with(RESERVED_RELAY_PREFIX))
        .collect();
    if reserved.is_empty() {
        return Ok(false);
    }
    if reserved != [RELAY_TRANSPORT]
        || !client_supports_range(&value.to_string())
        || value
            .get("session")
            .is_some_and(super::super::sharing_direct_wire::session_presentation_is_direct)
    {
        return Err(invalid());
    }
    Ok(true)
}
/// Strip reserved client tokens before deriving BOTH the effective typed
/// session and canonical recipe. Preserve exact legacy bytes for a no-op.
pub(super) fn normalize_wire(bytes: &[u8], ready: bool) -> Result<Vec<u8>, ApiError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let mut normalized = value.clone();
    let direct = value
        .get("session")
        .is_some_and(super::super::sharing_direct_wire::session_presentation_is_direct);
    let supports = client_supports_range(&value.to_string());
    if let Some(values) = normalized
        .get_mut("session")
        .and_then(|session| session.get_mut("caps"))
        .and_then(|caps| caps.get_mut("transports"))
        .and_then(Value::as_array_mut)
    {
        values.retain(|value| {
            !value
                .as_str()
                .is_some_and(|token| token.starts_with(RESERVED_RELAY_PREFIX))
        });
        if ready && supports && !direct {
            values.push(Value::String(RELAY_TRANSPORT.into()));
        }
    }
    if normalized == value {
        return Ok(bytes.to_vec());
    }
    serde_json::to_vec(&normalized).map_err(|_| invalid())
}
pub(super) fn public_recipe_hash(bytes: &[u8]) -> Result<[u8; 32], ApiError> {
    let normalized = normalize_wire(bytes, false)?;
    let value: Value = serde_json::from_slice(&normalized).map_err(|_| invalid())?;
    Ok(sha2::Sha256::digest(serde_json::to_vec(&value).map_err(|_| invalid())?).into())
}
pub(super) fn range_bound(input: &SourceStartInput) -> bool {
    internal_range(&input.canonical_recipe).unwrap_or(false)
}
pub(super) fn unavailable() -> ApiError {
    ApiError::typed(
        StatusCode::CONFLICT,
        "sharing_receiver_presentation_unavailable",
        "Receiver cannot recover this bounded presentation",
    )
}
pub(super) fn require_current_receiver(
    entry: &SourceStartEntry,
    headers: &impl AuthenticationHeaders,
) -> Result<(), ApiError> {
    if entry.identity.receiver_range && !authenticated_receiver_ready(headers) {
        return Err(unavailable());
    }
    Ok(())
}
/// Replacing only the effective session leaves exact operation lineage and
/// control/resource fields intact. Start preserves its selected exact bytes.
pub(super) fn operation_body(path: &str, raw: &[u8], recipe: &[u8]) -> Result<Vec<u8>, ApiError> {
    if path.ends_with("/sessions") {
        return Ok(recipe.to_vec());
    }
    let mut operation: Value = serde_json::from_slice(raw).map_err(|_| invalid())?;
    let recipe: Value = serde_json::from_slice(recipe).map_err(|_| invalid())?;
    operation.as_object_mut().ok_or_else(invalid)?.insert(
        "session".into(),
        recipe.get("session").cloned().ok_or_else(invalid)?,
    );
    serde_json::to_vec(&operation).map_err(|_| invalid())
}

/// Explicit domains make historical injected tokens distinguishable from a
/// Range invocation. A legacy raw candidate is retained solely for cleanup.
pub(super) fn select_persisted_recipe(
    raw: &[u8],
    stored: &str,
    cleanup: bool,
) -> Result<Vec<u8>, ApiError> {
    let legacy = normalize_wire(raw, false)?;
    let range = normalize_wire(raw, true)?;
    [(raw.to_vec(), false), (legacy, false), (range, true)]
        .into_iter()
        .find(|(recipe, range)| {
            (cleanup || *range || !has_reserved_tokens(recipe))
                && super::ingress::fingerprint_recipe(recipe, *range) == stored
        })
        .map(|(recipe, _)| recipe)
        .ok_or_else(|| super::SourceStartFailure::Conflict.response())
}

#[cfg(test)]
mod normalization_tests {
    use super::*;
    fn recipe() -> Vec<u8> {
        br#"{"session":{"caps":{"transports":["hls","hls-bounded-range-v1"]}}}"#.to_vec()
    }
    #[test]
    fn exact_authenticated_header_only() {
        let mut headers = axum::http::HeaderMap::new();
        assert!(!authenticated_receiver_ready(&headers));
        headers.insert(
            RECEIVER_HEADER,
            RECEIVER_HEADER_VALUE.parse().expect("header"),
        );
        assert!(authenticated_receiver_ready(&headers));
        headers.append(
            RECEIVER_HEADER,
            RECEIVER_HEADER_VALUE.parse().expect("header"),
        );
        assert!(!authenticated_receiver_ready(&headers));
        headers.remove(RECEIVER_HEADER);
        headers.insert(RECEIVER_HEADER, "unknown".parse().expect("header"));
        assert!(!authenticated_receiver_ready(&headers));
    }
    #[test]
    fn reserved_external_tokens_are_removed_before_effective_recipe() {
        let injected = br#"{"session":{"caps":{"transports":["hls","hls-bounded-range-v1","plurx-internal-relay-bounded-range-v1","plurx-internal-relay-injected"]}}}"#;
        assert!(internal_range(injected).is_err());
        let legacy = normalize_wire(injected, false).expect("strip every reserved token");
        assert!(!has_reserved_tokens(&legacy));
        let range = normalize_wire(injected, true).expect("authenticated normalization");
        assert!(internal_range(&range).expect("exact sole internal token"));
        assert_eq!(
            public_recipe_hash(injected).expect("raw public"),
            public_recipe_hash(&range).expect("effective public")
        );
        assert_eq!(normalize_wire(&range, true).expect("idempotent"), range);
    }
    #[test]
    fn legacy_lost_reply_keeps_mode_after_receiver_upgrade() {
        let raw = recipe();
        let legacy = super::super::ingress::fingerprint_recipe(&raw, false);
        let selected = select_persisted_recipe(&raw, &legacy, false).expect("legacy replay");
        assert_eq!(selected, raw);
        assert!(!internal_range(&selected).expect("legacy mode"));
        let range = normalize_wire(&raw, true).expect("range");
        let stored = super::super::ingress::fingerprint_recipe(&range, true);
        assert_ne!(legacy, stored);
        let selected = select_persisted_recipe(&raw, &stored, false).expect("range reconstruction");
        assert!(internal_range(&selected).expect("range remains range after header loss"));
        // The caller must gate this selected mode, never normalize it back.
        assert!(!authenticated_receiver_ready(&axum::http::HeaderMap::new()));
    }
    #[test]
    fn historical_injected_relay_is_never_range_provenance_but_raw_cleanup_survives() {
        let injected = normalize_wire(&recipe(), true).expect("historical raw injection");
        let old = super::super::ingress::fingerprint_recipe(&injected, false);
        let new = super::super::ingress::fingerprint_recipe(&injected, true);
        assert_ne!(old, new);
        assert!(select_persisted_recipe(&injected, &old, false).is_err());
        assert_eq!(
            select_persisted_recipe(&injected, &old, true).expect("exact old cleanup bytes"),
            injected
        );
    }
    #[test]
    fn normalization_requires_hls_end_token_and_non_direct_presentation() {
        for raw in [
            br#"{"session":{"caps":{"transports":["hls"]}}}"#.as_slice(),
            br#"{"session":{"caps":{"transports":["hls-bounded-range-v1"]}}}"#.as_slice(),
        ] {
            assert!(!has_reserved_tokens(
                &normalize_wire(raw, true).expect("legacy")
            ));
        }
    }
}
