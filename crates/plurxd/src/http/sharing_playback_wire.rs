//! Engine response projection only. These helpers grant no viewer, Source,
//! worker, session or delivery authority and are not registered as routes.
use super::{
    hls::StartResponse,
    stream::{DecisionResponse, DeliveryPlan},
};
use plurx_core::{
    sharing_file_locators::{FileLocatorKey, FileLocatorReference},
    sharing_resources::{
        SharingFileResource, SharingFileResourceKind, SharingHlsResource, SharingHlsResourceKind,
        SharingResourceUnsupported,
    },
};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

type Result<T> = std::result::Result<T, SharingResourceUnsupported>;
const MAX_ENVELOPE_BYTES: usize = 4 * 1024 * 1024;

/// Private payload, with no Deserialize/Debug or caller-mutable fields.
#[derive(Serialize)]
#[serde(transparent)]
pub(crate) struct SharedDecisionResponse(Value);

#[derive(Serialize)]
#[serde(transparent)]
pub(crate) struct SharedOverlayManifest(Value);

/// Preserve current PGS geometry and source-time intervals, with full string
/// identity. Relative object names remain beneath the captured B file base.
/// Actual generation/PNG/file-revision authorization belongs to delivery.
pub(crate) fn project_shared_overlay(
    manifest: crate::pgs_overlay::OverlayManifest,
    reference: &FileLocatorReference,
    key: &FileLocatorKey,
) -> Result<SharedOverlayManifest> {
    key.issue(reference)
        .map_err(|_| SharingResourceUnsupported)?;
    if manifest.file_id.to_string() != reference.file_id.as_str()
        || manifest.schema != crate::pgs_overlay::SCHEMA
        || manifest.kind != "pgs"
        || manifest.timebase != "source_ms"
        || !(0..=4095).contains(&manifest.track_index)
        || !(0..=9_007_199_254_740_991).contains(&manifest.duration_ms)
        || manifest.generation.len() != 64
        || !manifest
            .generation
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || manifest.cues.len() > 100_000
    {
        return Err(SharingResourceUnsupported);
    }
    let mut end = 0;
    for cue in &manifest.cues {
        if cue.start_ms < end
            || cue.start_ms >= cue.end_ms
            || cue.end_ms > manifest.duration_ms
            || cue.canvas_width == 0
            || cue.canvas_width > 4096
            || cue.canvas_height == 0
            || cue.canvas_height > 2160
            || cue.objects.len() > 64
            || cue.id.len() > 128
            || cue.id.chars().any(char::is_control)
        {
            return Err(SharingResourceUnsupported);
        }
        end = cue.end_ms;
        for object in &cue.objects {
            let suffix = format!("subs/{}/{}", manifest.track_index, object.image);
            let parsed = SharingFileResource::parse(&suffix)?;
            if !matches!(
                parsed.kind(),
                SharingFileResourceKind::SubtitleObject { .. }
            ) || !object
                .image
                .starts_with(&format!("overlay/{}/objects/", manifest.generation))
                || object.width == 0
                || object.height == 0
                || u32::from(object.x) + u32::from(object.width) > u32::from(cue.canvas_width)
                || u32::from(object.y) + u32::from(object.height) > u32::from(cue.canvas_height)
            {
                return Err(SharingResourceUnsupported);
            }
        }
    }
    let mut payload = serde_json::to_value(manifest).map_err(|_| SharingResourceUnsupported)?;
    payload["file_id"] = json!(reference.file_id);
    payload["reference"] = json!({"item":reference.item,"file_id":reference.file_id,"revision":reference.revision,"lifecycle_generation":reference.lifecycle_generation});
    if serde_json::to_vec(&payload)
        .map_err(|_| SharingResourceUnsupported)?
        .len()
        > MAX_ENVELOPE_BYTES
    {
        return Err(SharingResourceUnsupported);
    }
    Ok(SharedOverlayManifest(payload))
}

fn file_url(url: &str, file: i64, base: &str, expected: SharingFileResourceKind) -> Result<String> {
    let prefix = format!("/api/v1/files/{file}/");
    let suffix = url
        .strip_prefix(&prefix)
        .ok_or(SharingResourceUnsupported)?;
    let (path, query) = suffix
        .split_once('?')
        .map_or((suffix, None), |(path, query)| (path, Some(query)));
    let parsed = SharingFileResource::parse(path)?;
    if parsed.kind() != expected {
        return Err(SharingResourceUnsupported);
    }
    if let Some(query) = query {
        if expected != SharingFileResourceKind::Progressive {
            return Err(SharingResourceUnsupported);
        }
        let audio = query
            .strip_prefix("audio=")
            .ok_or(SharingResourceUnsupported)?;
        let index = audio
            .parse::<u16>()
            .map_err(|_| SharingResourceUnsupported)?;
        if index > 4095 || index.to_string() != audio {
            return Err(SharingResourceUnsupported);
        }
    }
    Ok(format!("{base}/{suffix}"))
}

/// Preserve the complete actual engine envelope; replace its numeric Source
/// file identity with the canonical string and full compound reference.
/// The caller still needs fresh B/Source authority before publishing it.
pub(crate) fn project_shared_decision(
    mut decision: DecisionResponse,
    reference: &FileLocatorReference,
    key: &FileLocatorKey,
) -> Result<SharedDecisionResponse> {
    let file = reference
        .file_id
        .as_str()
        .parse::<i64>()
        .map_err(|_| SharingResourceUnsupported)?;
    if file < 0 || decision.file_id != file {
        return Err(SharingResourceUnsupported);
    }
    let locator = key
        .issue(reference)
        .map_err(|_| SharingResourceUnsupported)?;
    let base = locator.file_base();
    let play_kind = match &decision.delivery {
        DeliveryPlan::Direct { .. } => SharingFileResourceKind::Direct,
        _ => SharingFileResourceKind::Progressive,
    };
    decision.play_url = file_url(&decision.play_url, file, &base, play_kind)?;
    match &mut decision.delivery {
        DeliveryPlan::Direct { url } => {
            *url = file_url(url, file, &base, SharingFileResourceKind::Direct)?
        }
        DeliveryPlan::Remux {
            url, sessions_url, ..
        } => {
            *url = file_url(url, file, &base, SharingFileResourceKind::Progressive)?;
            *sessions_url = file_url(sessions_url, file, &base, SharingFileResourceKind::Start)?;
        }
        DeliveryPlan::Transcode { sessions_url, .. } => {
            *sessions_url = file_url(sessions_url, file, &base, SharingFileResourceKind::Start)?;
        }
    }
    let mut payload = serde_json::to_value(decision).map_err(|_| SharingResourceUnsupported)?;
    payload["file_id"] = json!(reference.file_id);
    payload["reference"] = json!({"item":reference.item,"file_id":reference.file_id,"revision":reference.revision,"lifecycle_generation":reference.lifecycle_generation});
    if serde_json::to_vec(&payload)
        .map_err(|_| SharingResourceUnsupported)?
        .len()
        > MAX_ENVELOPE_BYTES
    {
        return Err(SharingResourceUnsupported);
    }
    Ok(SharedDecisionResponse(payload))
}

/// Only the closed whole-engine decoder can enter this peer-payload projection.
/// No Source path or URL is forwarded into the receiver's file namespace.
pub(super) fn project_decoded_decision(
    decoded: super::sharing_decision_decode::DecodedDecision,
    reference: &FileLocatorReference,
    key: &FileLocatorKey,
) -> Result<SharedDecisionResponse> {
    let file = reference
        .file_id
        .as_str()
        .parse::<i64>()
        .map_err(|_| SharingResourceUnsupported)?;
    let base = key
        .issue(reference)
        .map_err(|_| SharingResourceUnsupported)?
        .file_base();
    let mut payload = decoded.into_payload();
    if payload["file_id"].as_i64() != Some(file) {
        return Err(SharingResourceUnsupported);
    }
    let mode = payload["delivery"]["mode"]
        .as_str()
        .ok_or(SharingResourceUnsupported)?
        .to_owned();
    let kind = if mode == "direct" {
        SharingFileResourceKind::Direct
    } else {
        SharingFileResourceKind::Progressive
    };
    let url = payload["play_url"]
        .as_str()
        .ok_or(SharingResourceUnsupported)?;
    payload["play_url"] = json!(file_url(url, file, &base, kind)?);
    match mode.as_str() {
        "direct" => {
            let url = payload["delivery"]["url"]
                .as_str()
                .ok_or(SharingResourceUnsupported)?;
            payload["delivery"]["url"] =
                json!(file_url(url, file, &base, SharingFileResourceKind::Direct)?);
        }
        "remux" => {
            let url = payload["delivery"]["url"]
                .as_str()
                .ok_or(SharingResourceUnsupported)?;
            payload["delivery"]["url"] = json!(file_url(
                url,
                file,
                &base,
                SharingFileResourceKind::Progressive
            )?);
            let start = payload["delivery"]["sessions_url"]
                .as_str()
                .ok_or(SharingResourceUnsupported)?;
            payload["delivery"]["sessions_url"] = json!(file_url(
                start,
                file,
                &base,
                SharingFileResourceKind::Start
            )?);
        }
        "transcode" => {
            let start = payload["delivery"]["sessions_url"]
                .as_str()
                .ok_or(SharingResourceUnsupported)?;
            payload["delivery"]["sessions_url"] = json!(file_url(
                start,
                file,
                &base,
                SharingFileResourceKind::Start
            )?);
        }
        _ => return Err(SharingResourceUnsupported),
    }
    payload["file_id"] = json!(reference.file_id);
    payload["reference"] = json!({"item":reference.item,"file_id":reference.file_id,"revision":reference.revision,"lifecycle_generation":reference.lifecycle_generation});
    if serde_json::to_vec(&payload)
        .map_err(|_| SharingResourceUnsupported)?
        .len()
        > MAX_ENVELOPE_BYTES
    {
        return Err(SharingResourceUnsupported);
    }
    Ok(SharedDecisionResponse(payload))
}

fn v4(id: Uuid) -> bool {
    id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122 && !id.is_nil()
}

/// Exact ordinary B session namespace. Source IDs/URLs are never published;
/// Quality, timing, HDR/DV and VOD fields are retained. Control identity
/// names the actual receiver owner, rather than exposing Source ownership.
/// This pure projection cannot create or bind a session.
pub(crate) fn project_shared_start(
    mut response: StartResponse,
    source_session: Uuid,
    receiver_session: Uuid,
    receiver_incarnation: Uuid,
    receiver_owner_epoch: i64,
) -> Result<StartResponse> {
    if !response.start_seconds.is_finite()
        || response.start_seconds < 0.0
        || response.start_seconds > 9_007_199_254_740.0
        || response
            .duration_ms
            .is_some_and(|ms| !(0..=9_007_199_254_740_991).contains(&ms))
        || response
            .media_origin_ms
            .is_some_and(|ms| !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&ms))
        || !v4(source_session)
        || !v4(receiver_session)
        || !v4(receiver_incarnation)
        || !(1..=9_007_199_254_740_991).contains(&receiver_owner_epoch)
        || response.control.is_none()
        // A shared Start is always a VOD presentation (B accepts only `vod`
        // or `direct`, and direct play has its own reply).
        || !response.vod
        || source_session == receiver_session
        || response.session_id != source_session.to_string()
    {
        return Err(SharingResourceUnsupported);
    }
    let source_prefix = format!("/api/v1/hls/{source_session}/");
    let suffix = response
        .playlist_url
        .strip_prefix(&source_prefix)
        .ok_or(SharingResourceUnsupported)?;
    let resource = SharingHlsResource::parse(suffix)?;
    if !matches!(
        resource.kind(),
        SharingHlsResourceKind::Master
            | SharingHlsResourceKind::Index
            | SharingHlsResourceKind::Video
    ) {
        return Err(SharingResourceUnsupported);
    }
    if let Some(control) = &mut response.control {
        if control.url != format!("{source_prefix}control")
            || control.protocol != crate::playback_control::PROTOCOL_V1
            || control.control_epoch == 0
            || control.control_epoch > 9_007_199_254_740_991
            || control.next_exchange_ms != crate::playback_control::NEXT_EXCHANGE_MS
            || control.lease_timeout_ms != crate::playback_control::VOD_LEASE_TIMEOUT_MS
            || Uuid::parse_str(&control.generation)
                .ok()
                .is_none_or(|id| !v4(id) || id.to_string() != control.generation)
        {
            return Err(SharingResourceUnsupported);
        }
        control.url = format!("/api/v1/hls/{receiver_session}/control");
        control.generation = receiver_incarnation.to_string();
        control.control_epoch = receiver_owner_epoch as u64;
    }
    response.playlist_url = format!("/api/v1/hls/{receiver_session}/{suffix}");
    response.session_id = receiver_session.to_string();
    if serde_json::to_vec(&response)
        .map_err(|_| SharingResourceUnsupported)?
        .len()
        > MAX_ENVELOPE_BYTES
    {
        return Err(SharingResourceUnsupported);
    }
    Ok(response)
}

/// The closed refusal vocabulary a Source may return for an authenticated
/// current-rendition exchange. B maps each one onto the ordinary client
/// control answer under its own session identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SharedControlRefusalCode {
    StaleControl,
    OwnerChanged,
    RateLimited,
    SessionEnded,
    OwnerTransition,
    OwnerLost,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SharedControlRefusal {
    pub(crate) code: SharedControlRefusalCode,
    pub(crate) retry_after_ms: Option<u32>,
}

impl SharedControlRefusal {
    pub(crate) fn from_state(error: crate::playback_control::ControlStateError) -> Self {
        use SharedControlRefusalCode as C;
        let (code, retry_after_ms) = match error {
            crate::playback_control::ControlStateError::StaleGeneration
            | crate::playback_control::ControlStateError::StaleClient
            | crate::playback_control::ControlStateError::StaleSequence => (C::StaleControl, None),
            crate::playback_control::ControlStateError::OwnerChanged => (C::OwnerChanged, None),
            crate::playback_control::ControlStateError::RateLimited(after) => {
                (C::RateLimited, Some(after.clamp(1, 60_000)))
            }
            // Shared controls are VOD current-rendition only; a rolling or
            // paused terminal on the Source is still an ended session to B.
            crate::playback_control::ControlStateError::SessionEnded
            | crate::playback_control::ControlStateError::RollingEnded(_)
            | crate::playback_control::ControlStateError::PauseExpired => (C::SessionEnded, None),
            crate::playback_control::ControlStateError::OwnerTransition => {
                (C::OwnerTransition, Some(500))
            }
            crate::playback_control::ControlStateError::OwnerLost => (C::OwnerLost, None),
            crate::playback_control::ControlStateError::Unavailable => (C::Unavailable, Some(500)),
        };
        Self {
            code,
            retry_after_ms,
        }
    }
    /// A well-formed refusal carries a retry hint exactly when its code is
    /// one a client retries on the server's cadence.
    pub(crate) fn is_valid(&self) -> bool {
        use SharedControlRefusalCode as C;
        match self.code {
            C::RateLimited => self
                .retry_after_ms
                .is_some_and(|after| (1..=60_000).contains(&after)),
            C::OwnerTransition | C::Unavailable => self.retry_after_ms == Some(500),
            C::StaleControl | C::OwnerChanged | C::SessionEnded | C::OwnerLost => {
                self.retry_after_ms.is_none()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::stream::SourceSummary;
    use plurx_core::{
        playback::{AudioAction, AudioDelivery, Decision, PlaybackMethod},
        secrets::CredentialKey,
        sharing::{SharingIdentity, SourceId},
        sharing_catalogue::SharedReference,
        sharing_catalogue_details::FileRevision,
        transcode::OutputGrade,
    };
    fn reference() -> FileLocatorReference {
        FileLocatorReference {
            item: SharedReference {
                import_id: Uuid::new_v4(),
                server_id: Uuid::new_v4(),
                catalogue_epoch: Uuid::new_v4(),
                library_id: SourceId::parse("9007199254740993").expect("library"),
                item_id: SourceId::parse("9223372036854775807").expect("item"),
            },
            file_id: SourceId::parse("9223372036854775807").expect("file"),
            revision: FileRevision::parse(&"d".repeat(64)).expect("revision"),
            lifecycle_generation: 7,
        }
    }
    fn key() -> FileLocatorKey {
        let identity = SharingIdentity {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            created_at_ms: 1000,
        };
        let master = CredentialKey::from_bytes([77; 32]);
        let envelope = FileLocatorKey::generate_sealed(&master, &identity).expect("fixture key");
        FileLocatorKey::open(&master, &identity, &envelope).expect("fixture signer")
    }
    fn decision(method: PlaybackMethod) -> DecisionResponse {
        decision_for_file(method, i64::MAX)
    }
    fn decision_for_file(method: PlaybackMethod, file_id: i64) -> DecisionResponse {
        let direct = format!("/api/v1/files/{file_id}/direct");
        let progressive = format!("/api/v1/files/{file_id}/stream.mp4?audio=2");
        let sessions_url = format!("/api/v1/files/{file_id}/hls/sessions");
        let delivery = match method {
            PlaybackMethod::DirectPlay => DeliveryPlan::Direct {
                url: direct.clone(),
            },
            PlaybackMethod::Remux => DeliveryPlan::Remux {
                url: progressive.clone(),
                sessions_url,
                aac: true,
                requires_hls: true,
                preserve_dolby_vision: true,
                audio: Some(2),
            },
            PlaybackMethod::Transcode => DeliveryPlan::Transcode {
                sessions_url,
                audio: Some(2),
            },
        };
        DecisionResponse {
            display_aware_auto_protocol: Some("route-v1".into()),
            quality_candidate_id: Some(plurx_core::playback::candidate::CandidateId([4; 16])),
            quality_candidates: Some(vec![]),
            file_id,
            vod_indexed: true,
            decision: Decision {
                method,
                reasons: vec!["fixture reason".into()],
                transcode_audio: true,
                delivered_audio: AudioDelivery {
                    action: AudioAction::Copy {
                        codec: "aac".into(),
                        channels: 2,
                    },
                    downmix: None,
                    reason: "fixture",
                },
                preserve_dolby_vision: true,
                convert_dolby_vision: true,
                container: "mp4",
                delivered_dynamic_range: "dolby_vision",
                delivered_dolby_vision_profile: Some(8),
                transcode_grade: OutputGrade::Sdr,
            },
            play_url: if method == PlaybackMethod::DirectPlay {
                direct
            } else {
                progressive
            },
            delivery,
            source: SourceSummary {
                container: Some("matroska".into()),
                video_codec: Some("hevc".into()),
                video_profile: Some("Main 10".into()),
                width: Some(3840),
                height: Some(2160),
                bit_depth: Some(10),
                hdr: Some("dolby_vision".into()),
                hdr_format: Some("Dolby Vision P7".into()),
                dv_profile: Some(7),
                dv_el_present: Some(true),
                bitrate: Some(30_000_000),
                duration_ms: Some(100_000),
                frame_rate: Some("24000/1001".into()),
            },
            audio: vec![super::super::stream::AudioTrackDto {
                index: 2,
                codec: "aac".into(),
                channels: Some(2),
                language: Some("eng".into()),
                title: Some("actual typed audio".into()),
                default: true,
            }],
            subtitles: vec![super::super::stream::SubTrackDto {
                index: 3,
                codec: "hdmv_pgs_subtitle".into(),
                language: Some("eng".into()),
                title: Some("actual typed bitmap metadata".into()),
                default: false,
                forced: true,
                text: false,
                native: false,
                overlay: Some(crate::pgs_overlay::PROTOCOL),
            }],
            selection: Some(super::super::stream::DecisionSelection {
                audio_index: Some(2),
                subtitle_index: Some(3),
                subtitle_requires_burn_in: false,
                subtitle_burn_in_blocked_by_hdr: false,
                subtitle_route: Some("overlay"),
            }),
            markers: vec![],
            audio_offset_ms: 0,
            declared_offset_ms: Some(-50),
            ladder: crate::transcode::ladder(Some(2160)),
            prior_kbps: Some(50000),
            prefer_segmented: Some("fixture".into()),
        }
    }
    #[test]
    fn sharing_decision_projection_preserves_complete_engine_fields_and_exact_string_identity() {
        let reference = reference();
        let key = key();
        let base = key.issue(&reference).expect("locator").file_base();
        for method in [
            PlaybackMethod::DirectPlay,
            PlaybackMethod::Remux,
            PlaybackMethod::Transcode,
        ] {
            let mut expected =
                serde_json::to_value(decision(method)).expect("actual typed engine serializer");
            let projected =
                project_shared_decision(decision(method), &reference, &key).expect("projection");
            let actual = serde_json::to_value(projected).expect("wire");
            expected["file_id"] = json!(reference.file_id);
            expected["reference"] = json!({"item":reference.item,"file_id":reference.file_id,"revision":reference.revision,"lifecycle_generation":reference.lifecycle_generation});
            let prefix = format!("/api/v1/files/{}/", i64::MAX);
            for pointer in ["/play_url", "/delivery/url", "/delivery/sessions_url"] {
                if let Some(url) = expected.pointer_mut(pointer) {
                    *url = json!(format!(
                        "{base}/{}",
                        url.as_str()
                            .expect("engine URL")
                            .strip_prefix(&prefix)
                            .expect("Source file prefix")
                    ));
                }
            }
            assert_eq!(actual, expected);
            assert!(actual["file_id"].is_string());
            assert_eq!(
                actual["reference"]["item"]["library_id"],
                json!("9007199254740993")
            );
            assert!(!serde_json::to_string(&actual)
                .expect("wire text")
                .contains(&prefix));
        }
        for url in [
            "https://source.invalid/file",
            "/api/v1/files/7/stream.mp4",
            "/api/v1/files/9223372036854775807/stream.mp4?audio=02",
            "/api/v1/files/9223372036854775807/stream.mp4?audio=2&token=secret",
        ] {
            let mut wrong = decision(PlaybackMethod::Remux);
            wrong.play_url = url.into();
            assert!(project_shared_decision(wrong, &reference, &key).is_err());
        }
        let mut wrong = decision(PlaybackMethod::Remux);
        wrong.file_id = 7;
        assert!(project_shared_decision(wrong, &reference, &key).is_err());
    }
    #[test]
    fn sharing_peer_decision_decoder_retains_complete_engine_and_refuses_shape_drift() {
        use super::super::sharing_decision_decode::DecodedDecision;
        for method in [
            PlaybackMethod::DirectPlay,
            PlaybackMethod::Remux,
            PlaybackMethod::Transcode,
        ] {
            let mut engine = decision(method);
            engine.prior_kbps = None;
            let payload = serde_json::to_value(engine).expect("actual full engine payload");
            assert_eq!(
                DecodedDecision::parse(payload.clone(), i64::MAX)
                    .expect("complete Source decision")
                    .into_payload(),
                payload
            );
            for pointer in [
                "",
                "/source",
                "/delivered_audio",
                "/delivered_audio/action",
                "/delivery",
                "/ladder/0",
            ] {
                let mut wrong = payload.clone();
                wrong
                    .pointer_mut(pointer)
                    .expect("actual object")
                    .as_object_mut()
                    .expect("object")
                    .insert("unnegotiated_engine_field".into(), json!(true));
                assert!(DecodedDecision::parse(wrong, i64::MAX).is_err());
            }
            for field in [
                "file_id",
                "vod_indexed",
                "method",
                "reasons",
                "transcode_audio",
                "delivered_audio",
                "container",
                "delivered_dynamic_range",
                "play_url",
                "delivery",
                "source",
                "audio",
                "subtitles",
                "markers",
                "audio_offset_ms",
                "declared_offset_ms",
                "ladder",
            ] {
                let mut missing = payload.clone();
                missing
                    .as_object_mut()
                    .expect("engine object")
                    .remove(field);
                assert!(
                    DecodedDecision::parse(missing, i64::MAX).is_err(),
                    "required field {field}"
                );
            }
            for (pointer, value) in [
                ("/file_id", json!("9223372036854775807")),
                ("/method", json!("unsupported")),
                ("/prior_kbps", json!(100)),
                ("/vod_indexed", json!(1)),
                ("/audio_offset_ms", json!(1.5)),
                ("/delivery/mode", json!("remote_decode")),
            ] {
                let mut wrong = payload.clone();
                // prior_kbps is intentionally omitted by the Source engine.
                if pointer == "/prior_kbps" {
                    wrong["prior_kbps"] = value;
                } else {
                    *wrong.pointer_mut(pointer).expect("current field") = value;
                }
                assert!(DecodedDecision::parse(wrong, i64::MAX).is_err());
            }
            assert!(DecodedDecision::parse(payload, i64::MAX - 1).is_err());
        }
    }

    #[test]
    fn sharing_decoded_decision_zero_identity_projects_only_receiver_urls() {
        let mut reference = reference();
        reference.item.library_id =
            plurx_core::sharing::SourceId::parse("0").expect("zero library");
        reference.item.item_id = plurx_core::sharing::SourceId::parse("0").expect("zero item");
        reference.file_id = plurx_core::sharing::SourceId::parse("0").expect("zero file");
        let key = key();
        let base = key
            .issue(&reference)
            .expect("signed zero tuple")
            .file_base();
        for method in [
            PlaybackMethod::DirectPlay,
            PlaybackMethod::Remux,
            PlaybackMethod::Transcode,
        ] {
            let mut engine = decision_for_file(method, 0);
            engine.prior_kbps = None;
            let decoded = super::super::sharing_decision_decode::DecodedDecision::parse(
                serde_json::to_value(engine).expect("whole engine"),
                0,
            )
            .expect("decoded");
            let actual = serde_json::to_value(
                project_decoded_decision(decoded, &reference, &key).expect("B projection"),
            )
            .expect("payload");
            assert_eq!(actual["file_id"], json!("0"));
            assert_eq!(actual["reference"]["item"]["library_id"], json!("0"));
            assert!(actual["play_url"].as_str().expect("URL").starts_with(&base));
            assert!(!serde_json::to_string(&actual)
                .expect("wire")
                .contains("/api/v1/files/0/"));
        }
    }

    fn start(source: Uuid) -> StartResponse {
        let mut response:StartResponse=serde_json::from_value(json!({"quality_catalog_status":{"complete":true,"causes":[]},"display_aware_auto_protocol":"route-v1","quality_candidate_id":"04040404040404040404040404040404","quality_candidates":[],"session_id":source,"playlist_url":format!("/api/v1/hls/{source}/index.m3u8?native=1&subtitle=2"),"duration_ms":100000,"start_seconds":30.0,"media_origin_ms":29800,"height":2160,"encoder":"fixture-encoder","vod":true,"ladder":[{"height":2160,"total_kbps":22000,"peak_kbps":32000}],"prior_kbps":50000,"delivered_dynamic_range":"dolby_vision","delivered_dolby_vision_profile":8,"plan_notes":["fixture override"]})).expect("engine HTTP shape");
        response.control = crate::playback_control::ControlBootstrap::new(
            &source.to_string(),
            &Uuid::new_v4().to_string(),
            7,
            crate::playback_control::VOD_LEASE_TIMEOUT_MS,
        );
        response
    }
    #[test]
    fn sharing_start_projection_retains_every_engine_field_and_rejects_source_url_escape() {
        let source = Uuid::new_v4();
        let receiver = Uuid::new_v4();
        let receiver_incarnation = Uuid::new_v4();
        let original = start(source);
        let source_incarnation = original
            .control
            .as_ref()
            .expect("control")
            .generation
            .clone();
        let mut expected = serde_json::to_value(&original).expect("engine wire");
        expected["session_id"] = json!(receiver);
        expected["playlist_url"] = json!(format!(
            "/api/v1/hls/{receiver}/index.m3u8?native=1&subtitle=2"
        ));
        expected["control"]["url"] = json!(format!("/api/v1/hls/{receiver}/control"));
        expected["control"]["generation"] = json!(receiver_incarnation);
        expected["control"]["control_epoch"] = json!(9);
        let actual = serde_json::to_value(
            project_shared_start(original, source, receiver, receiver_incarnation, 9)
                .expect("projection"),
        )
        .expect("wire");
        assert_eq!(actual, expected);
        assert_eq!(actual["control"]["control_epoch"], json!(9));
        assert!(!serde_json::to_string(&actual)
            .expect("wire")
            .contains(&source_incarnation));
        assert!(project_shared_start(start(source), source, receiver, Uuid::nil(), 9).is_err());
        for epoch in [0, -1, 9_007_199_254_740_992] {
            assert!(project_shared_start(
                start(source),
                source,
                receiver,
                receiver_incarnation,
                epoch
            )
            .is_err());
        }
        let mut absent_control = start(source);
        absent_control.control = None;
        assert!(
            project_shared_start(absent_control, source, receiver, receiver_incarnation, 9)
                .is_err()
        );
        assert!(
            project_shared_start(start(source), source, source, receiver_incarnation, 9).is_err()
        );
        assert!(project_shared_start(
            start(source),
            Uuid::new_v4(),
            receiver,
            receiver_incarnation,
            9
        )
        .is_err());
        assert!(
            project_shared_start(start(source), source, Uuid::nil(), receiver_incarnation, 9)
                .is_err()
        );
        for seconds in [-1.0, f64::INFINITY, f64::NAN] {
            let mut wrong = start(source);
            wrong.start_seconds = seconds;
            assert!(
                project_shared_start(wrong, source, receiver, receiver_incarnation, 9).is_err()
            );
        }
        for url in [
            format!("https://source.invalid/api/v1/hls/{source}/index.m3u8"),
            format!("/api/v1/hls/{source}/seg1.m4s"),
            format!("/api/v1/hls/{source}/index.m3u8?token=secret"),
        ] {
            let mut wrong = start(source);
            wrong.playlist_url = url;
            assert!(
                project_shared_start(wrong, source, receiver, receiver_incarnation, 9).is_err()
            );
        }
        let mut wrong = start(source);
        wrong.control.as_mut().expect("control").url = "https://source.invalid/control".into();
        assert!(project_shared_start(wrong, source, receiver, receiver_incarnation, 9).is_err());
    }
    #[test]
    fn sharing_pgs_projection_preserves_cues_and_refuses_geometry_or_generation_escape() {
        use crate::pgs_overlay::{OverlayCue, OverlayManifest, OverlayObject};
        let reference = reference();
        let key = key();
        let generation = "a".repeat(64);
        let manifest = OverlayManifest {
            schema: 1,
            generation: generation.clone(),
            file_id: i64::MAX,
            track_index: 2,
            kind: "pgs".into(),
            timebase: "source_ms".into(),
            duration_ms: 1000,
            cues: vec![OverlayCue {
                id: "cue-1".into(),
                start_ms: 50,
                end_ms: 750,
                canvas_width: 1920,
                canvas_height: 1080,
                objects: vec![OverlayObject {
                    image: format!("overlay/{generation}/objects/{}.png", "b".repeat(64)),
                    x: 10,
                    y: 20,
                    width: 100,
                    height: 50,
                }],
            }],
        };
        let mut expected = serde_json::to_value(&manifest).expect("actual engine manifest");
        expected["file_id"] = json!(reference.file_id);
        expected["reference"] = json!({"item":reference.item,"file_id":reference.file_id,"revision":reference.revision,"lifecycle_generation":reference.lifecycle_generation});
        assert_eq!(
            serde_json::to_value(
                project_shared_overlay(manifest.clone(), &reference, &key).expect("projection")
            )
            .expect("wire"),
            expected
        );
        for image in [
            "https://source.invalid/object.png".into(),
            format!("overlay/{}/objects/{}.png", "c".repeat(64), "b".repeat(64)),
            "../object.png".into(),
        ] {
            let mut wrong = manifest.clone();
            wrong.cues[0].objects[0].image = image;
            assert!(project_shared_overlay(wrong, &reference, &key).is_err());
        }
        let mut wrong = manifest.clone();
        wrong.cues[0].objects[0].x = 1900;
        assert!(project_shared_overlay(wrong, &reference, &key).is_err());
        let mut wrong = manifest;
        wrong.cues[0].end_ms = 1001;
        assert!(project_shared_overlay(wrong, &reference, &key).is_err());
    }

    #[test]
    fn sharing_source_control_refusals_are_closed_and_carry_exact_retry_hints() {
        use crate::playback_control::ControlStateError as E;
        for (error, code, retry) in [
            (
                E::StaleGeneration,
                SharedControlRefusalCode::StaleControl,
                None,
            ),
            (E::StaleClient, SharedControlRefusalCode::StaleControl, None),
            (
                E::StaleSequence,
                SharedControlRefusalCode::StaleControl,
                None,
            ),
            (
                E::OwnerChanged,
                SharedControlRefusalCode::OwnerChanged,
                None,
            ),
            (
                E::RateLimited(0),
                SharedControlRefusalCode::RateLimited,
                Some(1),
            ),
            (
                E::RateLimited(u32::MAX),
                SharedControlRefusalCode::RateLimited,
                Some(60_000),
            ),
            (
                E::SessionEnded,
                SharedControlRefusalCode::SessionEnded,
                None,
            ),
            (
                E::PauseExpired,
                SharedControlRefusalCode::SessionEnded,
                None,
            ),
            (
                E::OwnerTransition,
                SharedControlRefusalCode::OwnerTransition,
                Some(500),
            ),
            (E::OwnerLost, SharedControlRefusalCode::OwnerLost, None),
            (
                E::Unavailable,
                SharedControlRefusalCode::Unavailable,
                Some(500),
            ),
        ] {
            let refusal = SharedControlRefusal::from_state(error);
            assert_eq!(refusal.code, code, "{error:?}");
            assert_eq!(refusal.retry_after_ms, retry, "{error:?}");
            assert!(refusal.is_valid(), "{error:?}");
        }
        for (code, retry) in [
            (SharedControlRefusalCode::StaleControl, Some(500)),
            (SharedControlRefusalCode::RateLimited, None),
            (SharedControlRefusalCode::RateLimited, Some(60_001)),
            (SharedControlRefusalCode::Unavailable, None),
            (SharedControlRefusalCode::OwnerTransition, Some(1)),
        ] {
            assert!(!SharedControlRefusal {
                code,
                retry_after_ms: retry
            }
            .is_valid());
        }
        let wire: serde_json::Value =
            serde_json::to_value(SharedControlRefusal::from_state(E::RateLimited(250)))
                .expect("refusal wire");
        assert_eq!(
            wire,
            serde_json::json!({"code":"rate_limited","retry_after_ms":250})
        );
        assert!(serde_json::from_value::<SharedControlRefusal>(
            serde_json::json!({"code":"rate_limited","retry_after_ms":250,"message":"raw"})
        )
        .is_err());
    }

    #[test]
    fn sharing_protocol_fixture_hls_start_projection() {
        use crate::sharing_protocol_fixture::{accepted, b_layer, fixture, mutated, rows};
        let fixture = fixture();
        let start = &fixture["hls_start"];
        let uuid = |value: &serde_json::Value| {
            Uuid::parse_str(value.as_str().expect("fixture uuid")).expect("fixture uuid")
        };
        let source_session = uuid(&start["source"]["session_id"]);
        let receiver = uuid(&start["receiver"]["session_id"]);
        let incarnation = uuid(&start["receiver"]["incarnation_id"]);
        let epoch = start["receiver"]["control_epoch"]
            .as_i64()
            .expect("fixture epoch");
        let project = |source: &serde_json::Value| {
            serde_json::from_value::<StartResponse>(source.clone())
                .ok()
                .and_then(|response| {
                    project_shared_start(response, source_session, receiver, incarnation, epoch)
                        .ok()
                })
                .map(|public| serde_json::to_value(public).expect("public wire"))
        };
        assert_eq!(project(&start["source"]), Some(start["public"].clone()));
        let mut checked = 0;
        for row in rows(&start["mutations"], "Start mutation") {
            if !b_layer(row) {
                continue;
            }
            let projected = project(&mutated(&start["source"], row, &source_session.to_string()));
            assert_eq!(projected.is_some(), accepted(row), "{}", row["id"]);
            if row["layer"] == "both" && accepted(row) {
                assert_eq!(
                    projected,
                    Some(mutated(&start["public"], row, &receiver.to_string())),
                    "{}",
                    row["id"]
                );
            }
            checked += 1;
        }
        assert!(checked >= 14, "Start rows the receiver validates");
    }
}
