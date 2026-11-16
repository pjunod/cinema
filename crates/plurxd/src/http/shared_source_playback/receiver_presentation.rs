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
/// A receiver that relays a Dolby Vision Start (its Start decoder admits the
/// `dolby_vision` range and profile) says so with this header, exactly as it
/// affirms bounded ranges. A receiver that does not is planned for without
/// Dolby Vision, so an older receiver gets the compatible base instead of a
/// Start it would refuse.
pub(crate) const DOLBY_VISION_RECEIVER_HEADER: &str = "x-plurx-receiver-dynamic-range";
pub(crate) const DOLBY_VISION_RECEIVER_HEADER_VALUE: &str = "dolby-vision-v1";
/// The plan note a Dolby Vision title's shared Start carries when the
/// receiver did not affirm Dolby Vision relay.
pub(crate) const DOLBY_VISION_WITHHELD_NOTE: &str = "receiver does not relay Dolby Vision";

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

/// The receiver's Dolby Vision relay affirmation, read from the raw request
/// headers exactly as the bounded-range header is. Only the public ingress
/// and the decision endpoint read it: the ingress applies the answer to the
/// effective recipe ([`normalize_wire`]), which every forwarded owner then
/// plans from, so the forwarding wire carries nothing new.
pub(crate) fn authenticated_receiver_dolby_vision(headers: &axum::http::HeaderMap) -> bool {
    let mut values = headers.get_all(DOLBY_VISION_RECEIVER_HEADER).iter();
    values
        .next()
        .is_some_and(|value| value.as_bytes() == DOLBY_VISION_RECEIVER_HEADER_VALUE.as_bytes())
        && values.next().is_none()
}
/// Plan for a receiver that did not affirm Dolby Vision relay: remove every
/// Dolby Vision claim from the caps, so the shared planner itself chooses the
/// compatible base, HDR10-E or a tone-map. Returns whether anything was
/// removed. Used where a typed caps document is planned directly (the shared
/// decision); a Start is withheld in its recipe by [`normalize_wire`].
pub(crate) fn withhold_dolby_vision(caps: &mut plurx_core::playback::DeviceCaps) -> bool {
    let mut withheld = false;
    for video in &mut caps.video {
        withheld |= !video.dv_profiles.is_empty();
        video.dv_profiles.clear();
    }
    withheld |= caps.dv_transport.take().is_some();
    if let Some(display) = caps.display.as_mut() {
        withheld |= std::mem::take(&mut display.dolby_vision);
    }
    withheld
}
/// [`withhold_dolby_vision`] on a recipe's JSON caps: the claims are removed
/// (their absence is the default), so a caps document that makes none is
/// left exactly as it was.
fn withhold_dolby_vision_json(caps: &mut Value) -> bool {
    let mut withheld = false;
    if let Some(videos) = caps.get_mut("video").and_then(Value::as_array_mut) {
        for video in videos {
            if let Some(video) = video.as_object_mut() {
                withheld |= video.remove("dv_profiles").is_some_and(|profiles| {
                    profiles.as_array().is_none_or(|list| !list.is_empty())
                });
            }
        }
    }
    if let Some(caps) = caps.as_object_mut() {
        withheld |= caps
            .remove("dv_transport")
            .is_some_and(|value| !value.is_null());
        if let Some(display) = caps.get_mut("display").and_then(Value::as_object_mut) {
            withheld |= display
                .remove("dolby_vision")
                .is_some_and(|value| value != Value::Bool(false));
        }
    }
    withheld
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
///
/// `dolby_vision` is the receiver's Dolby Vision relay affirmation. Without
/// it every Dolby Vision claim is removed from the caps here, where both the
/// typed session and the canonical recipe are derived, so the recipe, its
/// fingerprint and the plan agree. No token is added: an owner of any version
/// reads an ordinary recipe, and a recipe without a Dolby Vision claim is left
/// byte-identical.
pub(super) fn normalize_wire(
    bytes: &[u8],
    ready: bool,
    dolby_vision: bool,
) -> Result<Vec<u8>, ApiError> {
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
    if !dolby_vision && !direct {
        if let Some(caps) = normalized
            .get_mut("session")
            .and_then(|session| session.get_mut("caps"))
        {
            withhold_dolby_vision_json(caps);
        }
    }
    if normalized == value {
        return Ok(bytes.to_vec());
    }
    serde_json::to_vec(&normalized).map_err(|_| invalid())
}
pub(super) fn public_recipe_hash(bytes: &[u8]) -> Result<[u8; 32], ApiError> {
    // Every reserved token and every Dolby Vision claim is stripped, so the
    // public identity of a recipe is the same with or without either receiver
    // affirmation; a recipe that makes no Dolby Vision claim hashes exactly as
    // it always has.
    let normalized = normalize_wire(bytes, false, false)?;
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
/// Checked before registration: the bounded identity derives from the input
/// alone, so refusing here never leaves a registered Start without its owner.
pub(super) fn require_receiver_for_start(
    input: &SourceStartInput,
    headers: &impl AuthenticationHeaders,
) -> Result<(), ApiError> {
    if range_bound(input) && !authenticated_receiver_ready(headers) {
        return Err(unavailable());
    }
    Ok(())
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
    let legacy = normalize_wire(raw, false, false)?;
    let range = normalize_wire(raw, true, false)?;
    // The same two modes with the receiver's Dolby Vision affirmation (the
    // recipes before Dolby Vision withholding existed).
    let legacy_dolby_vision = normalize_wire(raw, false, true)?;
    let range_dolby_vision = normalize_wire(raw, true, true)?;
    [
        (raw.to_vec(), false, true),
        (legacy, false, false),
        (range, true, false),
        (legacy_dolby_vision, false, false),
        (range_dolby_vision, true, false),
    ]
    .into_iter()
    .find(|(recipe, range, raw)| {
        (cleanup || !*raw || !has_reserved_tokens(recipe))
            && super::ingress::fingerprint_recipe(recipe, *range) == stored
    })
    .map(|(recipe, _, _)| recipe)
    .ok_or_else(|| super::SourceStartFailure::Conflict.response())
}

#[cfg(test)]
mod dolby_vision_relay_tests {
    use super::*;
    use plurx_core::{sharing::SourceId, sharing_catalogue_details::FileRevision};

    fn reference() -> crate::http::hls::SourcePlaybackTarget {
        crate::http::hls::SourcePlaybackTarget {
            server_id: uuid::Uuid::new_v4(),
            catalogue_epoch: uuid::Uuid::new_v4(),
            library_id: SourceId::parse("0").expect("library"),
            item_id: SourceId::parse("7").expect("item"),
            file_id: SourceId::parse("9").expect("file"),
            revision: FileRevision::parse(&"a".repeat(64)).expect("revision"),
        }
    }
    fn start(presentation: &str, caps: Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "reference": reference(),
            "session": {
                "playback_id":"dv-relay","request_id":uuid::Uuid::new_v4(),"copy":true,"height":2160,
                "quality_auto":false,"presentation":presentation,"caps":caps
            }
        }))
        .expect("canonical Start")
    }
    /// An Apple TV-shaped shared Start: HEVC Main10 enumerating Profile 8,
    /// Dolby Vision over HLS and a Dolby Vision display.
    fn dolby_vision_start(presentation: &str) -> Vec<u8> {
        start(
            presentation,
            serde_json::json!({"v":2,"video":[{"codec":"hevc","profiles":["main10"],"max_height":2160,"present":["sdr","pq"],"dv_profiles":[8]}],
                "audio":["aac"],"containers":["mp4"],"transports":["hls","hls-bounded-range-v1"],
                "hls_hevc_sample_entries":["hvc1"],"dv_transport":"hls","display":{"hdr":true,"dolby_vision":true}}),
        )
    }
    fn typed(bytes: &[u8]) -> super::super::SourceStartInput {
        super::super::parse_start_request(bytes, "7", "9").expect("closed Source Start")
    }
    fn dolby_vision_claims(input: &super::super::SourceStartInput) -> (Vec<u8>, bool, bool) {
        let caps = input.session.caps.as_ref().expect("caps");
        (
            caps.video[0].dv_profiles.clone(),
            caps.dv_transport.is_some(),
            caps.display
                .as_ref()
                .is_some_and(|display| display.dolby_vision),
        )
    }
    /// What `public_recipe_hash` returned before Dolby Vision withholding,
    /// for a recipe that carries no reserved token: the digest of its bytes.
    fn legacy_public_hash(raw: &[u8]) -> [u8; 32] {
        let value: Value = serde_json::from_slice(raw).expect("raw");
        sha2::Sha256::digest(serde_json::to_vec(&value).expect("bytes")).into()
    }
    /// The check an owner built before this change applied to every
    /// forwarded recipe: its reserved tokens are exactly the range relay, or
    /// none.
    fn an_older_owner_accepts(recipe: &[u8]) -> bool {
        let value: Value = serde_json::from_slice(recipe).expect("recipe");
        let reserved: Vec<_> = transport_values(&value)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|token| token.starts_with(RESERVED_RELAY_PREFIX))
            .collect();
        reserved.is_empty() || reserved == [RELAY_TRANSPORT]
    }

    #[test]
    fn dolby_vision_caps_survive_only_an_affirming_receiver() {
        let raw = dolby_vision_start("vod");
        // An older receiver sends no affirmation: the effective recipe and
        // the typed session both lose every Dolby Vision claim.
        for ready in [false, true] {
            let unaffirmed = normalize_wire(&raw, ready, false).expect("unaffirmed");
            let recipe: Value = serde_json::from_slice(&unaffirmed).expect("recipe");
            let caps = &recipe["session"]["caps"];
            assert!(caps["video"][0].get("dv_profiles").is_none());
            assert!(caps.get("dv_transport").is_none());
            assert!(caps["display"].get("dolby_vision").is_none());
            assert_eq!(caps["display"]["hdr"], Value::Bool(true));
            assert_eq!(
                dolby_vision_claims(&typed(&unaffirmed)),
                (vec![], false, false)
            );
            assert!(an_older_owner_accepts(&unaffirmed));
            assert_eq!(
                normalize_wire(&unaffirmed, ready, false).expect("idempotent"),
                unaffirmed
            );
        }
        // An affirming receiver keeps its caps, and its recipe carries
        // nothing an older owner would refuse.
        for ready in [false, true] {
            let affirmed = normalize_wire(&raw, ready, true).expect("affirmed");
            assert_eq!(
                dolby_vision_claims(&typed(&affirmed)),
                (vec![8], true, true)
            );
            assert!(an_older_owner_accepts(&affirmed));
            assert_eq!(internal_range(&affirmed).expect("closed tokens"), ready);
        }
        // The public identity is the same raw or effective, affirmed or not.
        let public = public_recipe_hash(&raw).expect("raw public");
        for ready in [false, true] {
            for dolby_vision in [false, true] {
                let effective = normalize_wire(&raw, ready, dolby_vision).expect("effective");
                assert_eq!(public_recipe_hash(&effective).expect("public"), public);
            }
        }
        // Direct play keeps its own caps.
        let direct = dolby_vision_start("direct");
        assert_eq!(
            normalize_wire(&direct, false, false).expect("direct"),
            direct
        );
        assert_eq!(dolby_vision_claims(&typed(&direct)), (vec![8], true, true));
    }

    #[test]
    fn a_start_without_dolby_vision_caps_is_byte_identical_either_way() {
        // An SDR H264 player, the common case: no affirmation can change a
        // byte of its recipe or its public identity.
        let sdr = start(
            "vod",
            serde_json::json!({"v":2,"video":[{"codec":"h264","max_height":1080,"present":["sdr"]}],
                "audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"],"display":{"hdr":false}}),
        );
        for dolby_vision in [false, true] {
            assert_eq!(normalize_wire(&sdr, false, dolby_vision).expect("sdr"), sdr);
        }
        assert_eq!(
            public_recipe_hash(&sdr).expect("public"),
            legacy_public_hash(&sdr)
        );
    }

    #[test]
    fn an_unaffirmed_profile8_plan_is_the_hdr10_base() {
        use plurx_core::playback::{decide, DeviceProfile, PlaybackMethod, RenderCaps};
        let raw = dolby_vision_start("vod");
        let withheld = typed(&normalize_wire(&raw, false, false).expect("unaffirmed"));
        let affirmed = typed(&normalize_wire(&raw, false, true).expect("affirmed"));
        let mut file = plurx_core::domain::MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 9,
            item_id: 7,
            path: "/media/p81.mkv".into(),
            size: 1,
            mtime: 1,
            duration_ms: Some(60_000),
            container: Some("mkv".into()),
            video_codec: Some("hevc".into()),
            video_codec_tag: Some("hvc1".into()),
            field_order: Some("progressive".into()),
            video_profile: Some("Main 10".into()),
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            hdr: Some("dolby_vision".into()),
            hdr_format: Some("Dolby Vision · Profile 8.1 (HDR10-compatible)".into()),
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: Some(20_000_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        };
        file.dolby_vision.profile = Some(8);
        file.dolby_vision.level = Some(6);
        file.dolby_vision.bl_compat_id = Some(1);
        file.dolby_vision.rpu_present = Some(true);
        let node = RenderCaps {
            dv_strippable: true,
            ..Default::default()
        };
        let plan = |input: &super::super::SourceStartInput| {
            decide(
                &file,
                &DeviceProfile::from_caps_v2(input.session.caps.as_ref().expect("caps")),
                &node,
            )
        };
        assert!(plan(&affirmed).preserve_dolby_vision);
        let base = plan(&withheld);
        assert_eq!(base.method, PlaybackMethod::Remux);
        assert!(!base.preserve_dolby_vision && !base.convert_dolby_vision);
        assert_eq!(base.delivered_dynamic_range, "hdr10");
    }
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
        let legacy = normalize_wire(injected, false, false).expect("strip every reserved token");
        assert!(!has_reserved_tokens(&legacy));
        let range = normalize_wire(injected, true, false).expect("authenticated normalization");
        assert!(internal_range(&range).expect("exact sole internal token"));
        assert_eq!(
            public_recipe_hash(injected).expect("raw public"),
            public_recipe_hash(&range).expect("effective public")
        );
        assert_eq!(
            normalize_wire(&range, true, false).expect("idempotent"),
            range
        );
    }
    #[test]
    fn legacy_lost_reply_keeps_mode_after_receiver_upgrade() {
        let raw = recipe();
        let legacy = super::super::ingress::fingerprint_recipe(&raw, false);
        let selected = select_persisted_recipe(&raw, &legacy, false).expect("legacy replay");
        assert_eq!(selected, raw);
        assert!(!internal_range(&selected).expect("legacy mode"));
        let range = normalize_wire(&raw, true, false).expect("range");
        let stored = super::super::ingress::fingerprint_recipe(&range, true);
        assert_ne!(legacy, stored);
        let selected = select_persisted_recipe(&raw, &stored, false).expect("range reconstruction");
        assert!(internal_range(&selected).expect("range remains range after header loss"));
        // The caller must gate this selected mode, never normalize it back.
        assert!(!authenticated_receiver_ready(&axum::http::HeaderMap::new()));
    }
    #[test]
    fn historical_injected_relay_is_never_range_provenance_but_raw_cleanup_survives() {
        let injected = normalize_wire(&recipe(), true, false).expect("historical raw injection");
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
                &normalize_wire(raw, true, false).expect("legacy")
            ));
        }
    }
}
