//! Authenticated direct playback and watch adapters; native services own bytes and effects.
use super::*;
use axum::extract::Query;
use axum::http::Method;
use plurx_compat_jellyfin::subtitle::{delivery, DeliveryMethod, TrackFacts, Transport};
use plurx_core::domain::MediaFile;
use plurx_core::store::{
    JellyfinPlay, JellyfinPlayActivation, JellyfinPlayScope, JellyfinProgressProvenance,
    JellyfinProgressWrite, NewFileGrant, NewJellyfinPlay,
};

pub(super) fn now_ms() -> Result<i64, ApiError> {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| ApiError::ServiceUnavailable("server clock unavailable".into()))?
        .as_millis();
    i64::try_from(ms).map_err(|_| ApiError::ServiceUnavailable("server clock overflow".into()))
}
async fn scope(client: &ClientUser, state: &AppState) -> Result<JellyfinPlayScope, ApiError> {
    let scope = state
        .store
        .jellyfin_login_scope(client.token_hash.clone())
        .await?
        .ok_or(ApiError::Unauthorized)?;
    if scope.user_id != client.identity.native_id {
        return Err(ApiError::Unauthorized);
    }
    Ok(scope)
}
fn fingerprint(file: &MediaFile) -> Result<String, ApiError> {
    serde_json::to_string(file)
        .map(|json| plurx_core::auth::hash_token(&json))
        .map_err(|_| ApiError::ServiceUnavailable("source identity unavailable".into()))
}
pub(super) async fn live_item(
    client: &ClientUser,
    state: &AppState,
    id: &str,
) -> Result<wire::Item, ApiError> {
    let mut query = browse(None)?;
    query.limit = 1;
    query.recursive = true;
    catalog(client, state, query, Some(wire_id(id)?))
        .await?
        .items
        .into_iter()
        .next()
        .ok_or(ApiError::NotFound("catalog item"))
}
pub(super) async fn current_file(
    state: &AppState,
    play: &JellyfinPlay,
) -> Result<MediaFile, ApiError> {
    let snapshot = state
        .store
        .playback_planning_snapshot(play.negotiation.file_id, &[])
        .await?
        .ok_or(ApiError::NotFound("play source"))?;
    let selection: Value = serde_json::from_str(&play.negotiation.selection_json)
        .map_err(|_| ApiError::Conflict("play selection changed; renegotiate".into()))?;
    if let Some(probe) = selection["source"].get("probe") {
        if *probe != serde_json::to_value(&snapshot.probe_json)? {
            return Err(ApiError::Conflict("play probe changed; renegotiate".into()));
        }
    }
    let file = snapshot.file;
    if file.item_id != play.negotiation.item_id
        || fingerprint(&file)? != play.negotiation.source_fingerprint
    {
        return Err(ApiError::Conflict(
            "play source changed; renegotiate".into(),
        ));
    }
    Ok(file)
}
pub(super) async fn binding(
    client: &ClientUser,
    state: &AppState,
    play_id: &str,
    item_id: &str,
    source_id: &str,
) -> Result<JellyfinPlay, ApiError> {
    let play = own_binding(client, state, play_id, item_id, source_id).await?;
    same_generation(&play, &client.generation)?;
    Ok(play)
}
/// This login's exact play for the item/source, from any switch generation.
/// Only cleanup (Stop) may act on a play from an earlier generation.
async fn own_binding(
    client: &ClientUser,
    state: &AppState,
    play_id: &str,
    item_id: &str,
    source_id: &str,
) -> Result<JellyfinPlay, ApiError> {
    let scope = scope(client, state).await?;
    let play_id = wire_id(play_id)?.to_hex();
    let play = state
        .store
        .jellyfin_play(&play_id, &scope)
        .await?
        .ok_or(ApiError::NotFound("play binding"))?;
    if wire_id(item_id)?.to_hex() != play.negotiation.item_wire_id
        || wire_id(source_id)?.to_hex() != play.negotiation.file_wire_id
    {
        return Err(ApiError::BadRequest(
            "item/source does not belong to play".into(),
        ));
    }
    Ok(play)
}
/// A play belongs to the switch generation it was negotiated under. Turning
/// compatibility off ends it, even if compatibility is on again by the time
/// the client comes back with it.
pub(super) fn same_generation(play: &JellyfinPlay, generation: &str) -> Result<(), ApiError> {
    if play.negotiation.switch_generation != generation {
        return Err(ApiError::Conflict(
            "compatibility was turned off after this play was negotiated; renegotiate".into(),
        ));
    }
    Ok(())
}
fn player_id(scope: &JellyfinPlayScope) -> String {
    format!(
        "jellyfin:{}",
        plurx_core::auth::hash_token(&format!(
            "plurx/jellyfin/player/v1:{}:{}:{}",
            scope.user_id,
            scope.device_digest,
            match scope.client_family {
                JellyfinClientFamily::Infuse => "infuse",
                JellyfinClientFamily::AndroidTv => "android_tv",
            }
        ))
    )
}

fn direct_key(play: &JellyfinPlay) -> crate::delivery::Key {
    crate::delivery::Key::new(
        play.negotiation.scope.user_id,
        play.negotiation.file_id,
        Some(&format!("jellyfin-direct:{}", play.negotiation.play_id)),
    )
}
async fn release(state: &AppState, play: &JellyfinPlay) -> Result<(), ApiError> {
    if let Some(id) = play.native_incarnation_id.as_deref() {
        if let Some(route) = state.store.media_session_route_by_incarnation(id).await? {
            if route.state != "ended"
                && route.user_id == play.negotiation.scope.user_id
                && route.playback_id == play.negotiation.playback_id
            {
                let status = super::super::hls::release_with_terminal(
                    state.clone(),
                    route.session_id,
                    crate::vodserve::Terminal::Deleted,
                    "compatibility playback stopped",
                )
                .await;
                if !status.is_success() {
                    return Err(ApiError::ServiceUnavailable(
                        "native release pending".into(),
                    ));
                }
            }
        }
    }
    state.direct_plays.remove_key(&direct_key(play));
    if let Some(id) = play.direct_grant_id.as_deref() {
        state
            .store
            .revoke_file_grant(id, play.negotiation.scope.user_id, now_ms()? / 1000)
            .await?;
    }
    Ok(())
}
pub(super) async fn activate(
    state: &AppState,
    mut play: JellyfinPlay,
) -> Result<JellyfinPlay, ApiError> {
    current_file(state, &play).await?;
    if play.state == "active" {
        return Ok(play);
    }
    if play.state != "pending" || play.expires_at_ms <= now_ms()? {
        return Err(ApiError::Conflict("play is terminal or expired".into()));
    }
    let selection: Value = serde_json::from_str(&play.negotiation.selection_json)
        .map_err(|_| ApiError::Conflict("play selection changed; renegotiate".into()))?;
    if let Some(vod) = selection.get("vod").filter(|v| !v.is_null()) {
        let started = Box::pin(super::startup::create(state, &play, vod)).await?;
        let outcome = bind_native_start(state, &play, &started.session_id).await;
        if let Ok(Some(bound)) = outcome {
            release_superseded(state, &bound).await;
            return Ok(bound);
        }
        // Another waiter may have bound the same canonical native result:
        // this waiter releases the start only when no binding claims it, and
        // leaves it to native idle expiry when that cannot be established.
        if matches!(
            unclaimed_native_start(state, &play, &started.session_id).await,
            Ok(true)
        ) {
            super::super::hls::release_with_terminal(
                state.clone(),
                started.session_id,
                crate::vodserve::Terminal::Deleted,
                "compatibility activation refused",
            )
            .await;
        }
        return Err(match outcome {
            Err(error) => error,
            Ok(_) => ApiError::Conflict("native play activation refused".into()),
        });
    }
    let now = now_ms()?;
    let negotiated = play.direct_grant_id.clone();
    let id = match negotiated.as_ref() {
        Some(id) => id.clone(),
        None => {
            // A binding negotiated before scoped links existed: mint an internal
            // reference whose secret is discarded, exactly as before.
            let id = uuid::Uuid::new_v4().to_string();
            let secret = uuid::Uuid::new_v4().to_string();
            state
                .store
                .create_file_grant(NewFileGrant {
                    id: id.clone(),
                    token_hash: plurx_core::auth::hash_token(&secret),
                    file_id: play.negotiation.file_id,
                    user_id: play.negotiation.scope.user_id,
                    source_token_hash: play.negotiation.scope.token_digest.clone(),
                    created_at: now / 1000,
                    expires_at: play.negotiation.created_at_ms / 1000 + MEDIA_LINK_TTL_SECS,
                })
                .await?;
            id
        }
    };
    let result = state
        .store
        .activate_jellyfin_play(
            &play.negotiation.play_id,
            &play.negotiation.scope,
            JellyfinPlayActivation::DirectGrant(id.clone()),
            now,
        )
        .await;
    match result {
        Ok(true) => {}
        // A negotiated grant stays with its binding: a concurrent waiter may
        // have activated it, and Stop/logout revoke it through the binding.
        Ok(false) if negotiated.is_some() => {}
        Ok(false) => {
            state
                .store
                .revoke_file_grant(&id, play.negotiation.scope.user_id, now / 1000)
                .await?;
        }
        Err(error) => {
            if negotiated.is_none() {
                // Best effort: the unbound link also expires on its own.
                crate::store_result::observe(
                    crate::store_result::Operation::RevokeUnboundJellyfinMediaLink,
                    crate::store_result::Discard::BestEffort,
                    state
                        .store
                        .revoke_file_grant(&id, play.negotiation.scope.user_id, now / 1000)
                        .await,
                );
            }
            return Err(error.into());
        }
    }
    play = state
        .store
        .jellyfin_play(&play.negotiation.play_id, &play.negotiation.scope)
        .await?
        .ok_or(ApiError::NotFound("play binding"))?;
    if play.state != "active" {
        return Err(ApiError::Conflict("play activation refused".into()));
    }
    release_superseded(state, &play).await;
    Ok(play)
}
/// Bind this play to the native start; `Some` only when the binding is this
/// exact incarnation, recipe and clock (possibly bound by another waiter).
async fn bind_native_start(
    state: &AppState,
    play: &JellyfinPlay,
    session_id: &str,
) -> Result<Option<JellyfinPlay>, ApiError> {
    let route = state
        .store
        .media_session_route(session_id)
        .await?
        .ok_or(ApiError::Conflict(
            "native play activation unavailable".into(),
        ))?;
    if route.user_id != play.negotiation.scope.user_id
        || route.playback_id != play.negotiation.playback_id
    {
        return Err(ApiError::Conflict("native play owner changed".into()));
    }
    state
        .store
        .activate_jellyfin_play(
            &play.negotiation.play_id,
            &play.negotiation.scope,
            JellyfinPlayActivation::MediaIncarnation(route.incarnation_id.clone()),
            now_ms()?,
        )
        .await?;
    let bound = state
        .store
        .jellyfin_play(&play.negotiation.play_id, &play.negotiation.scope)
        .await?;
    Ok(bound.filter(|bound| {
        bound.state == "active"
            && bound.native_incarnation_id.as_deref() == Some(route.incarnation_id.as_str())
            && bound.negotiation.native_request_fingerprint == route.request_fingerprint
            && bound.negotiation.source_origin_ms == route.media_origin_ms
    }))
}
/// `true` only when the start exists and no binding of this play claims it.
async fn unclaimed_native_start(
    state: &AppState,
    play: &JellyfinPlay,
    session_id: &str,
) -> Result<bool, ApiError> {
    let Some(route) = state.store.media_session_route(session_id).await? else {
        return Ok(false);
    };
    let bound = state
        .store
        .jellyfin_play(&play.negotiation.play_id, &play.negotiation.scope)
        .await?;
    Ok(!bound.is_some_and(|bound| {
        bound.native_incarnation_id.as_deref() == Some(route.incarnation_id.as_str())
    }))
}
/// Release the exact native session or direct grant of every play this
/// activation ended. A client that replaces its play without Stopped (an
/// app kill, a quality or method change) would otherwise leave the old
/// native session current until idle expiry, or its direct presence listed.
/// Release is idempotent, so a duplicate waiter repeating it is harmless.
async fn release_superseded(state: &AppState, play: &JellyfinPlay) {
    let superseded = match state
        .store
        .jellyfin_plays_superseded_by(&play.negotiation.play_id, &play.negotiation.scope)
        .await
    {
        Ok(superseded) => superseded,
        Err(error) => {
            tracing::warn!(target: "plurxd::jellyfin", %error, "superseded compatibility plays could not be read for release");
            return;
        }
    };
    for old in superseded {
        if let Err(error) = release(state, &old).await {
            tracing::warn!(target: "plurxd::jellyfin", ?error, "superseded compatibility play release failed");
        }
    }
}
/// Authenticated current-play presence renews only the native passive grant
/// of this exact play, on whichever node owns it. It is not a reader touch,
/// a producer lease or rendered-frame evidence.
async fn renew_passive_presence(state: &AppState, session_id: &str, play: &JellyfinPlay) {
    // The native grant is keyed by the session request the owner created it
    // under: the exact incarnation, not the reserved `jellyfin:` claim id.
    let Some(request) = play.native_incarnation_id.as_deref() else {
        return;
    };
    let user = serde_json::json!(["user_id", play.negotiation.scope.user_id]).to_string();
    match super::super::hls::passive_presence(
        state,
        session_id,
        &user,
        &play.negotiation.playback_id,
        request,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => {
            tracing::debug!(target: "plurxd::jellyfin", "passive presence found no live grant for the current play")
        }
        Err(error) => {
            tracing::debug!(target: "plurxd::jellyfin", ?error, "passive presence could not reach the native owner")
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub(super) struct InfoRequest {
    user_id: Option<String>,
    media_source_id: Option<String>,
    start_time_ticks: Option<Ticks>,
    enable_direct_play: Option<bool>,
    enable_direct_stream: Option<bool>,
    enable_transcoding: Option<bool>,
    allow_video_stream_copy: Option<bool>,
    allow_audio_stream_copy: Option<bool>,
    audio_stream_index: Option<i64>,
    subtitle_stream_index: Option<i64>,
    device_profile: Option<Value>,
    /// Bounded below; only `Http` has a meaning here.
    direct_play_protocols: Option<Vec<String>>,
    max_streaming_bitrate: Option<i64>,
    current_play_session_id: Option<String>,
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, Value>,
}
fn wire_track(stream: &wire::MediaStream) -> TrackFacts<'_> {
    TrackFacts {
        codec: &stream.codec,
        language: stream.language.as_deref(),
        text: !plurx_core::tracks::is_bitmap_subtitle(&stream.codec),
    }
}
fn profiles(
    request: &InfoRequest,
    file: &MediaFile,
    facts: &plurx_compat_jellyfin::profile::Facts,
) -> Vec<plurx_core::playback::DeviceProfile> {
    if request.enable_direct_play == Some(false) {
        return Vec::new();
    }
    let Some(profile) = request.device_profile.as_ref() else {
        return Vec::new();
    };
    let Some(object) = profile.as_object() else {
        return Vec::new();
    };
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "Id" | "Name"
                | "MaxStreamingBitrate"
                | "MaxStaticBitrate"
                | "MusicStreamingTranscodingBitrate"
                | "DirectPlayProfiles"
                | "TranscodingProfiles"
                | "SubtitleProfiles"
                | "CodecProfiles"
                | "ContainerProfiles"
                | "ResponseProfiles"
        )
    }) {
        return Vec::new();
    }
    use plurx_compat_jellyfin::profile::{CodecKind, CodecRule};
    let rules = match profile.get("CodecProfiles").filter(|v| !v.is_null()) {
        None => Vec::new(),
        Some(value) => match serde_json::from_value::<Vec<CodecRule>>(value.clone()) {
            Ok(rules) if rules.len() <= 64 => rules,
            _ => return Vec::new(),
        },
    };
    if rules.iter().any(|rule| {
        !rule.accepts(
            CodecKind::Video,
            file.video_codec.as_deref(),
            file.container.as_deref(),
            None,
            facts,
        ) || !rule.accepts(
            CodecKind::VideoAudio,
            file.audio_streams.first().map(|a| a.codec.as_str()),
            file.container.as_deref(),
            None,
            facts,
        )
    }) {
        return Vec::new();
    }
    use plurx_compat_jellyfin::profile::{ContainerRule, MediaKind};
    let container_rules = match profile.get("ContainerProfiles").filter(|v| !v.is_null()) {
        None => Vec::new(),
        Some(value) => match serde_json::from_value::<Vec<ContainerRule>>(value.clone()) {
            Ok(rules) if rules.len() <= 64 => rules,
            _ => return Vec::new(),
        },
    };
    if container_rules
        .iter()
        .any(|rule| !rule.accepts(MediaKind::Video, file.container.as_deref(), facts))
    {
        return Vec::new();
    }
    let Some(profiles) = profile.get("DirectPlayProfiles").and_then(Value::as_array) else {
        return Vec::new();
    };
    if profiles.len() > 32 {
        return Vec::new();
    }
    let mut limits = Vec::new();
    if let Some(limit) = request.max_streaming_bitrate {
        limits.push(limit);
    }
    for key in ["MaxStreamingBitrate", "MaxStaticBitrate"] {
        if let Some(value) = profile.get(key) {
            let Some(limit) = value.as_i64() else {
                return Vec::new();
            };
            limits.push(limit);
        }
    }
    if limits.iter().any(|limit| *limit <= 0) || (!limits.is_empty() && file.bitrate.is_none()) {
        return Vec::new();
    }
    let max_bitrate = limits.into_iter().min();
    let split = |value: Option<&str>| {
        value
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>()
    };
    profiles
        .iter()
        .filter_map(|profile| {
            let object = profile.as_object()?;
            if object.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "Type" | "Container" | "VideoCodec" | "AudioCodec"
                )
            }) || profile.get("Type").and_then(Value::as_str) != Some("Video")
            {
                return None;
            }
            let containers = split(profile.get("Container").and_then(Value::as_str));
            let video = split(profile.get("VideoCodec").and_then(Value::as_str));
            let audio = split(profile.get("AudioCodec").and_then(Value::as_str));
            if !containers
                .iter()
                .any(|codec| file.container.as_ref() == Some(codec))
                || !video
                    .iter()
                    .any(|codec| file.video_codec.as_ref() == Some(codec))
            {
                return None;
            }
            let mut native =
                plurx_core::playback::caps_profile(containers, video, audio, None, false, false);
            native.max_bitrate = max_bitrate;
            Some(native)
        })
        .collect()
}
fn source_facts(
    file: &MediaFile,
    probe: Option<&str>,
    audio_index: Option<i64>,
) -> plurx_compat_jellyfin::profile::Facts {
    use plurx_compat_jellyfin::profile::{Fact, Facts, Property as P};
    let mut facts = Facts::new();
    for (key, value) in [
        (P::Width, file.width),
        (P::Height, file.height),
        (P::VideoBitDepth, file.bit_depth),
        (
            P::AudioChannels,
            file.audio_streams.first().and_then(|a| a.channels),
        ),
        (
            P::AudioSampleRate,
            file.audio_streams.first().and_then(|a| a.sample_rate),
        ),
    ] {
        if let Some(value) = value.filter(|value| *value > 0) {
            facts.insert(key, Fact::Number(value as f64));
        }
    }
    for (key, value) in [
        (P::VideoProfile, &file.video_profile),
        (P::VideoCodecTag, &file.video_codec_tag),
    ] {
        if let Some(value) = value {
            facts.insert(key, Fact::Text(value.clone()));
        }
    }
    match plurx_core::domain::ScanType::from_field_order(file.field_order.as_deref()) {
        plurx_core::domain::ScanType::Progressive => {
            facts.insert(P::IsInterlaced, Fact::Boolean(false));
        }
        plurx_core::domain::ScanType::Interlaced(_) => {
            facts.insert(P::IsInterlaced, Fact::Boolean(true));
        }
        plurx_core::domain::ScanType::Unknown => {}
    }
    let Some(probe) = probe.and_then(|s| serde_json::from_str::<Value>(s).ok()) else {
        return facts;
    };
    let Some(streams) = probe["streams"].as_array() else {
        return facts;
    };
    let video = streams.iter().find(|s| {
        s["codec_type"] == "video" && s["disposition"]["attached_pic"].as_i64() != Some(1)
    });
    let audio = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .find(|s| audio_index.is_none_or(|index| s["index"].as_i64() == Some(index)));
    for (key, kind) in [(P::NumAudioStreams, "audio"), (P::NumVideoStreams, "video")] {
        facts.insert(
            key,
            Fact::Number(streams.iter().filter(|s| s["codec_type"] == kind).count() as f64),
        );
    }
    facts.insert(P::NumStreams, Fact::Number(streams.len() as f64));
    let number = |v: &Value| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|v| v.parse::<f64>().ok()))
            .filter(|v| v.is_finite() && *v >= 0.0)
    };
    if let Some(video) = video {
        for (key, field) in [
            (P::VideoLevel, "level"),
            (P::VideoBitrate, "bit_rate"),
            (P::RefFrames, "refs"),
        ] {
            if let Some(value) = number(&video[field]) {
                facts.insert(key, Fact::Number(value));
            }
        }
        if let Some(rate) = video["avg_frame_rate"]
            .as_str()
            .and_then(|s| s.split_once('/'))
            .and_then(|(n, d)| Some((n.parse::<f64>().ok()?, d.parse::<f64>().ok()?)))
            .filter(|(n, d)| n.is_finite() && d.is_finite() && *n > 0.0 && *d > 0.0)
        {
            facts.insert(P::VideoFramerate, Fact::Number(rate.0 / rate.1));
        }
        if video["color_transfer"] == "bt709" && file.hdr.is_none() {
            facts.insert(P::VideoRangeType, Fact::Text("SDR".into()));
        }
        if let Some((n, d)) = video["sample_aspect_ratio"]
            .as_str()
            .and_then(|s| s.split_once(':'))
            .and_then(|(n, d)| Some((n.parse::<u64>().ok()?, d.parse::<u64>().ok()?)))
            .filter(|(n, d)| *n > 0 && *d > 0)
        {
            facts.insert(P::IsAnamorphic, Fact::Boolean(n != d));
        }
    }
    if let Some(audio) = audio {
        for (key, field) in [
            (P::AudioBitrate, "bit_rate"),
            (P::AudioBitDepth, "bits_per_raw_sample"),
        ] {
            if let Some(value) = number(&audio[field]).filter(|v| *v > 0.0) {
                facts.insert(key, Fact::Number(value));
            }
        }
        if let Some(value) = audio["profile"].as_str() {
            facts.insert(P::AudioProfile, Fact::Text(value.into()));
        }
    }
    facts
}

pub(super) async fn info_post(
    client: ClientUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
    Json(mut request): Json<InfoRequest>,
) -> Result<Response, ApiError> {
    merge_info_query(&client, raw.as_deref(), &mut request)?;
    info(&client, &state, &id, request).await
}
pub(super) async fn info_get(
    client: ClientUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let mut request = InfoRequest::default();
    merge_info_query(&client, raw.as_deref(), &mut request)?;
    info(&client, &state, &id, request).await
}
fn merge_info_query(
    client: &ClientUser,
    raw: Option<&str>,
    request: &mut InfoRequest,
) -> Result<(), ApiError> {
    fn integer(value: &str) -> Result<i64, ApiError> {
        value
            .parse()
            .map_err(|_| ApiError::BadRequest("invalid playback integer".into()))
    }
    fn assign<T: PartialEq>(field: &mut Option<T>, value: T) -> Result<(), ApiError> {
        if field.as_ref().is_some_and(|old| old != &value) {
            return Err(ApiError::BadRequest(
                "conflicting playback parameters".into(),
            ));
        }
        *field = Some(value);
        Ok(())
    }
    if let Some(user) = request.user_id.as_mut() {
        check_user(client, user)?;
        *user = wire_id(user)?.to_hex();
    }
    if let Some(play) = request.current_play_session_id.as_mut() {
        *play = wire_id(play)?.to_hex();
    }
    for (key, value) in query_pairs(raw)? {
        match key.to_ascii_lowercase().as_str() {
            "userid" => {
                check_user(client, &value)?;
                assign(&mut request.user_id, wire_id(&value)?.to_hex())?;
            }
            "mediasourceid" => {
                let normalized = wire_id(&value)?.to_hex();
                if request.media_source_id.as_ref().is_some_and(|old| {
                    wire_id(old).ok().map(|id| id.to_hex()) != Some(normalized.clone())
                }) {
                    return Err(ApiError::BadRequest("conflicting media source".into()));
                }
                request.media_source_id = Some(normalized);
            }
            "enabledirectplay" => assign(
                &mut request.enable_direct_play,
                match value.to_ascii_lowercase().as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err(ApiError::BadRequest("invalid direct flag".into())),
                },
            )?,
            "enabledirectstream"
            | "enabletranscoding"
            | "allowvideostreamcopy"
            | "allowaudiostreamcopy" => {
                let flag = match value.to_ascii_lowercase().as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err(ApiError::BadRequest("invalid playback flag".into())),
                };
                match key.to_ascii_lowercase().as_str() {
                    "enabledirectstream" => assign(&mut request.enable_direct_stream, flag)?,
                    "enabletranscoding" => assign(&mut request.enable_transcoding, flag)?,
                    "allowvideostreamcopy" => assign(&mut request.allow_video_stream_copy, flag)?,
                    _ => assign(&mut request.allow_audio_stream_copy, flag)?,
                }
            }
            "maxstreamingbitrate" => assign(&mut request.max_streaming_bitrate, integer(&value)?)?,
            "audiostreamindex" => assign(&mut request.audio_stream_index, integer(&value)?)?,
            "subtitlestreamindex" => assign(&mut request.subtitle_stream_index, integer(&value)?)?,
            "starttimeticks" => assign(
                &mut request.start_time_ticks,
                Ticks::try_from(integer(&value)?)
                    .map_err(|_| ApiError::BadRequest("invalid playback time".into()))?,
            )?,
            "currentplaysessionid" => assign(
                &mut request.current_play_session_id,
                wire_id(&value)?.to_hex(),
            )?,
            "directplayprotocols" => assign(
                &mut request.direct_play_protocols,
                value.split(',').map(|p| p.trim().to_owned()).collect(),
            )?,
            "api_key" | "apikey" | "isplayback" | "autoopenlivestream" => {}
            _ => return Err(ApiError::BadRequest("unsupported playback query".into())),
        }
    }
    // Jellyfin's MediaProtocol names; anything else is not a protocol.
    if request
        .direct_play_protocols
        .as_ref()
        .is_some_and(|protocols| {
            protocols.len() > 8
                || protocols.iter().any(|p| {
                    !["File", "Http", "Rtmp", "Rtsp", "Udp", "Rtp", "Ftp"]
                        .iter()
                        .any(|known| known.eq_ignore_ascii_case(p))
                })
        })
    {
        return Err(ApiError::BadRequest("invalid direct play protocol".into()));
    }
    Ok(())
}

async fn info(
    client: &ClientUser,
    state: &AppState,
    id: &str,
    mut request: InfoRequest,
) -> Result<Response, ApiError> {
    if let Some(uid) = request.user_id.as_deref() {
        check_user(client, uid)?;
    }
    if request.extra.keys().any(|key| {
        !matches!(
            key.as_str(),
            "IsPlayback"
                | "AutoOpenLiveStream"
                | "EnableDirectStream"
                | "EnableTranscoding"
                | "AllowVideoStreamCopy"
                | "AllowAudioStreamCopy"
        )
    }) {
        return Err(ApiError::BadRequest(
            "unsupported playback constraint".into(),
        ));
    }
    let item = live_item(client, state, id).await?;
    let scope = scope(client, state).await?;
    let sources = item
        .media_sources
        .ok_or(ApiError::NotFound("play sources"))?;
    let source = sources
        .into_iter()
        .find(|s| {
            request
                .media_source_id
                .as_ref()
                .is_none_or(|id| wire_id(id).ok() == Some(s.id))
        })
        .ok_or(ApiError::NotFound("play source"))?;
    let file_id = state
        .store
        .jellyfin_resolve_entity(JellyfinEntityKind::File, &source.id.to_hex())
        .await?
        .ok_or(ApiError::NotFound("source"))?;
    let snapshot = state
        .store
        .playback_planning_snapshot(file_id, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await?
        .ok_or(ApiError::NotFound("source"))?;
    let file = snapshot.file.clone();
    if request.audio_stream_index.is_none() {
        request.audio_stream_index = source
            .media_streams
            .iter()
            .filter(|stream| stream.stream_type == wire::StreamType::Audio)
            .find(|stream| stream.is_default)
            .or_else(|| {
                source
                    .media_streams
                    .iter()
                    .find(|stream| stream.stream_type == wire::StreamType::Audio)
            })
            .map(|stream| stream.index);
    }
    let mut selected_file = file.clone();
    if let Some(index) = request.audio_stream_index {
        let audio = source
            .media_streams
            .iter()
            .filter(|s| s.stream_type == wire::StreamType::Audio)
            .position(|s| s.index == index)
            .ok_or(ApiError::BadRequest(
                "audio index does not belong to source".into(),
            ))?;
        selected_file.audio_streams =
            vec![file
                .audio_streams
                .get(audio)
                .cloned()
                .ok_or(ApiError::BadRequest(
                    "audio probe indices are incomplete".into(),
                ))?];
    }
    let node = super::super::stream::render_caps(state).await;
    let facts = source_facts(
        &selected_file,
        snapshot.probe_json.as_deref(),
        request.audio_stream_index,
    );
    let direct = profiles(&request, &selected_file, &facts)
        .iter()
        .any(|profile| {
            plurx_core::playback::decide(&selected_file, profile, &node).method
                == plurx_core::playback::PlaybackMethod::DirectPlay
        });
    if let Some(index) = request.audio_stream_index {
        if index < 0
            || !source
                .media_streams
                .iter()
                .any(|s| s.stream_type == wire::StreamType::Audio && s.index == index)
        {
            return Err(ApiError::BadRequest(
                "audio index does not belong to source".into(),
            ));
        }
    }
    if let Some(index) = request.subtitle_stream_index {
        if index >= 0
            && !source
                .media_streams
                .iter()
                .any(|s| s.stream_type == wire::StreamType::Subtitle && s.index == index)
        {
            return Err(ApiError::BadRequest(
                "subtitle index does not belong to source".into(),
            ));
        }
        if index < -1 {
            return Err(ApiError::BadRequest("invalid subtitle selection".into()));
        }
    }
    // A malformed list grants nothing: every track then resolves to Encode.
    let subtitle_rules = plurx_compat_jellyfin::subtitle::subtitle_rules(
        request
            .device_profile
            .as_ref()
            .and_then(|profile| profile.get("SubtitleProfiles")),
    )
    .unwrap_or_else(|_| {
        tracing::debug!(target: "plurxd::jellyfin", "malformed SubtitleProfiles; no subtitle delivery granted");
        Vec::new()
    });
    let direct_transport = Transport::Direct {
        container: file.container.as_deref(),
    };
    // A client that declares no DirectPlayProfiles but enables direct play
    // over HTTP picks static delivery itself: Infuse sends exactly this and
    // then requests `/Videos/{id}/stream?Static=true`, even where Jellyfin
    // answered with a transcode (J0). It plays the file and its tracks as
    // they are, so no subtitle choice turns that into a transcode.
    let client_static = request.enable_direct_play == Some(true)
        && request
            .direct_play_protocols
            .as_ref()
            .is_some_and(|protocols| protocols.iter().any(|p| p.eq_ignore_ascii_case("http")))
        && request
            .device_profile
            .as_ref()
            .is_some_and(|profile| profile.get("DirectPlayProfiles").is_none_or(Value::is_null));
    // As in Jellyfin, a selected track the file cannot deliver to this client
    // as-is (Encode) makes the play a transcode.
    let direct = client_static
        || direct
            && request
                .subtitle_stream_index
                .filter(|index| *index >= 0)
                .and_then(|index| {
                    source
                        .media_streams
                        .iter()
                        .find(|s| s.stream_type == wire::StreamType::Subtitle && s.index == index)
                })
                .is_none_or(|stream| {
                    delivery(&subtitle_rules, wire_track(stream), direct_transport).method
                        != DeliveryMethod::Encode
                });
    let item_id = state
        .store
        .jellyfin_resolve_entity(JellyfinEntityKind::Item, &item.id.to_hex())
        .await?
        .ok_or(ApiError::NotFound("item"))?;
    if file.item_id != item_id {
        return Err(ApiError::BadRequest("source membership changed".into()));
    }
    let play_id = uuid::Uuid::new_v4().simple().to_string();
    let playback_id = player_id(&scope);
    let native_audio = request
        .audio_stream_index
        .and_then(|index| {
            source
                .media_streams
                .iter()
                .filter(|s| s.stream_type == wire::StreamType::Audio)
                .position(|s| s.index == index)
        })
        .map(|i| i as i64);
    let native_subtitle = request
        .subtitle_stream_index
        .filter(|i| *i >= 0)
        .and_then(|index| {
            source
                .media_streams
                .iter()
                .filter(|s| s.stream_type == wire::StreamType::Subtitle)
                .position(|s| s.index == index)
        })
        .map(|i| i as i64);
    let vod = if direct {
        None
    } else {
        let Some(profile) = request.device_profile.as_ref() else {
            return Ok(not_supported());
        };
        Box::pin(super::vod::negotiate(
            state,
            client.identity.native_id,
            &snapshot,
            super::vod::Requested {
                profile,
                playback_id: &playback_id,
                play_id: &play_id,
                start_ms: request.start_time_ticks.map_or(0, Ticks::milliseconds),
                audio: native_audio,
                subtitle: native_subtitle,
                bitrate: request.max_streaming_bitrate,
                allow_copy: request.enable_direct_stream != Some(false)
                    && request.allow_video_stream_copy != Some(false),
                allow_encode: request.enable_transcoding != Some(false),
                allow_audio_copy: request.allow_audio_stream_copy != Some(false),
                source_facts: &facts,
                subtitle_rules: &subtitle_rules,
            },
        ))
        .await?
    };
    if !direct && vod.is_none() {
        return Ok(not_supported());
    }
    let selection=json!({"audio":request.audio_stream_index,"subtitle":request.subtitle_stream_index,"source":{"size":file.size,"mtime":file.mtime,"probe":snapshot.probe_json},"vod":vod.as_ref().map(|v| json!({"body":v.body,"bitrate":v.bitrate,"inline_init":v.inline_init}))}).to_string();
    let profile_json = serde_json::to_string(&request.device_profile)
        .map_err(|_| ApiError::BadRequest("invalid device profile".into()))?;
    let fresh = state
        .store
        .playback_planning_snapshot(file_id, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await?
        .ok_or(ApiError::NotFound("play source"))?;
    if fingerprint(&fresh.file)? != fingerprint(&file)? || fresh.probe_json != snapshot.probe_json {
        return Err(ApiError::Conflict(
            "source changed during negotiation; retry".into(),
        ));
    }
    // A direct play's scoped media link: one unguessable secret for this title,
    // issued only to this authenticated negotiation, expiring within 24 hours
    // of it and revoked by Stop or by revoking the login. The secret is
    // returned once (as the source ETag, which clients copy into the direct
    // URL they build); only its digest is stored, in the native file grant.
    let created_at_ms = now_ms()?;
    let media_link = if direct {
        let secret = plurx_core::auth::generate_token()
            .map_err(|_| ApiError::ServiceUnavailable("media link unavailable".into()))?;
        let id = uuid::Uuid::new_v4().to_string();
        state
            .store
            .create_file_grant(NewFileGrant {
                id: id.clone(),
                token_hash: plurx_core::auth::hash_token(&secret),
                file_id,
                user_id: scope.user_id,
                source_token_hash: scope.token_digest.clone(),
                created_at: created_at_ms / 1000,
                expires_at: created_at_ms / 1000 + MEDIA_LINK_TTL_SECS,
            })
            .await?;
        Some((id, secret))
    } else {
        None
    };
    let play = NewJellyfinPlay {
        play_id: play_id.clone(),
        scope: scope.clone(),
        playback_id,
        item_id,
        file_id,
        item_wire_id: item.id.to_hex(),
        file_wire_id: source.id.to_hex(),
        source_fingerprint: fingerprint(&file)?,
        profile_fingerprint: plurx_core::auth::hash_token(&profile_json),
        native_request_fingerprint: vod.as_ref().map_or_else(
            || plurx_core::auth::hash_token(&selection),
            |v| v.fingerprint.clone(),
        ),
        selection_json: selection,
        source_origin_ms: if vod.is_some() {
            0
        } else {
            request.start_time_ticks.map_or(0, Ticks::milliseconds)
        },
        created_at_ms,
        media_grant_id: media_link.as_ref().map(|(id, _)| id.clone()),
        switch_generation: client.generation.clone(),
    };
    let created = state.store.create_jellyfin_play(play).await;
    if !matches!(created, Ok(true)) {
        if let Some((id, _)) = media_link.as_ref() {
            // Best effort: the play never bound, and the link expires on its own.
            crate::store_result::observe(
                crate::store_result::Operation::RevokeUnboundJellyfinMediaLink,
                crate::store_result::Discard::BestEffort,
                state
                    .store
                    .revoke_file_grant(id, scope.user_id, created_at_ms / 1000)
                    .await,
            );
        }
        created?;
        return Err(ApiError::ServiceUnavailable(
            "negotiation capacity exhausted".into(),
        ));
    }
    if let Some(previous) = request.current_play_session_id {
        if let Some(previous) = state
            .store
            .jellyfin_play(&wire_id(&previous)?.to_hex(), &scope)
            .await?
        {
            if previous.state == "pending"
                && state
                    .store
                    .withdraw_pending_jellyfin_play(
                        &previous.negotiation.play_id,
                        &scope,
                        now_ms()?,
                    )
                    .await?
            {
                // Withdrawn while still pending, so the snapshot is exact: no
                // native session can have been bound to it.
                if let Err(error) = release(state, &previous).await {
                    tracing::warn!(target: "plurxd::jellyfin", ?error, "abandoned negotiation release failed");
                }
            }
        }
    }
    let deliveries = source
        .media_streams
        .iter()
        .filter(|s| s.stream_type == wire::StreamType::Subtitle)
        .map(|s| {
            let track = wire_track(s);
            let choice = match vod.as_ref() {
                Some(plan) => super::vod::hls_track_delivery(
                    &subtitle_rules,
                    track,
                    plan.manifest_capable,
                    plan.manifest_subtitles,
                ),
                None => delivery(&subtitle_rules, track, direct_transport),
            };
            (s.index, track.text, choice)
        })
        .collect::<Vec<_>>();
    // Returned URLs are relative to the client's configured server address,
    // which already ends in `/jellyfin`: both pinned clients prefix it (a
    // returned `/jellyfin/...` becomes `/jellyfin/jellyfin/...`). Media and
    // subtitle requests from Android TV carry no header, only this `ApiKey`.
    let credential = client
        .url_credential
        .as_deref()
        .map(|token| format!("&ApiKey={token}"))
        .unwrap_or_default();
    let item_wire = item.id.to_hex();
    let source_wire = source.id.to_hex();
    let mut source = serde_json::to_value(source)
        .map_err(|_| ApiError::ServiceUnavailable("source DTO unavailable".into()))?;
    if let Some(streams) = source["MediaStreams"].as_array_mut() {
        for stream in streams {
            let Some((index, text, choice)) = deliveries
                .iter()
                .find(|(index, ..)| stream["Type"] == "Subtitle" && stream["Index"] == *index)
            else {
                continue;
            };
            stream["DeliveryMethod"] = json!(choice.method.as_str());
            stream["IsTextSubtitleStream"] = json!(text);
            stream["SupportsExternalStream"] = json!(text);
            if choice.method == DeliveryMethod::External {
                stream["DeliveryUrl"] = json!(format!(
                    "/Videos/{item_wire}/{source_wire}/Subtitles/{index}/0/Stream.{}?{}",
                    choice.format,
                    credential.trim_start_matches('&')
                )
                .trim_end_matches('?'));
                stream["IsExternalUrl"] = json!(false);
            }
        }
    }
    source["SupportsDirectPlay"] = json!(direct);
    source["SupportsTranscoding"] = json!(vod.is_some());
    if let Some(index) = request.audio_stream_index {
        source["DefaultAudioStreamIndex"] = json!(index);
    }
    if let Some(index) = request.subtitle_stream_index {
        source["DefaultSubtitleStreamIndex"] = json!(index);
    }
    if vod.is_some() {
        source["TranscodingUrl"] = json!(format!(
            "/Videos/{item_wire}/master.m3u8?MediaSourceId={source_wire}&PlaySessionId={play_id}{credential}"
        ));
        source["TranscodingContainer"] = json!(if vod.as_ref().is_some_and(|v| v.inline_init) {
            "ts"
        } else {
            "mp4"
        });
        source["TranscodingSubProtocol"] = json!("hls");
    } else {
        if let Some((_, secret)) = media_link.as_ref() {
            source["ETag"] = json!(secret);
        }
        source["DirectStreamUrl"] = json!(format!(
            "/Videos/{item_wire}/stream?MediaSourceId={source_wire}&PlaySessionId={play_id}&Static=true{credential}"
        ));
    }
    Ok(Json(json!({"MediaSources":[source],"PlaySessionId":play_id})).into_response())
}
/// Jellyfin's in-band refusal, marked so the metrics can tell it from a play.
fn not_supported() -> Response {
    let mut response = Json(json!({"MediaSources":[],"ErrorCode":"NotSupported"})).into_response();
    response
        .extensions_mut()
        .insert(super::metrics::NotSupported);
    response
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct PlayingEvent {
    item_id: String,
    media_source_id: String,
    play_session_id: String,
    position_ticks: Option<Ticks>,
    run_time_ticks: Option<Ticks>,
    user_id: Option<String>,
    play_method: Option<String>,
}
async fn resolve_event(
    client: &ClientUser,
    state: &AppState,
    event: &PlayingEvent,
) -> Result<JellyfinPlay, ApiError> {
    let play = resolve_any_generation(client, state, event).await?;
    same_generation(&play, &client.generation)?;
    Ok(play)
}
async fn resolve_any_generation(
    client: &ClientUser,
    state: &AppState,
    event: &PlayingEvent,
) -> Result<JellyfinPlay, ApiError> {
    if let Some(uid) = event.user_id.as_deref() {
        check_user(client, uid)?;
    }
    let play = own_binding(
        client,
        state,
        &event.play_session_id,
        &event.item_id,
        &event.media_source_id,
    )
    .await?;
    let selection: Value = serde_json::from_str(&play.negotiation.selection_json)?;
    let hls = selection.get("vod").is_some_and(|v| !v.is_null());
    // PlayMethod is the client's own label for what it is doing, not
    // authority: the binding is. Infuse reports `DirectStream` for a static
    // file it fetches with Range, and Android uses the same label for a copy
    // through the transcoding URL. Refuse only a label that names the other
    // delivery outright.
    let contradicts = match event.play_method.as_deref() {
        None | Some("DirectStream") => false,
        Some("DirectPlay") => hls,
        Some("Transcode") => !hls,
        Some(_) => true,
    };
    if contradicts {
        return Err(ApiError::BadRequest(
            "play method does not match negotiation".into(),
        ));
    }
    Ok(play)
}

fn progress_write(
    play: &JellyfinPlay,
    event: &PlayingEvent,
    final_commit: bool,
) -> Option<JellyfinProgressWrite> {
    event.position_ticks.map(|position| JellyfinProgressWrite {
        provenance: JellyfinProgressProvenance {
            play_id: play.negotiation.play_id.clone(),
            scope: play.negotiation.scope.clone(),
            manual_revision: play.manual_revision,
        },
        item_id: play.negotiation.item_id,
        position_ms: position.milliseconds(),
        duration_ms: event.run_time_ticks.map(Ticks::milliseconds),
        final_commit,
    })
}
pub(super) async fn playing(
    client: ClientUser,
    State(state): State<AppState>,
    Json(event): Json<PlayingEvent>,
) -> Result<StatusCode, ApiError> {
    let play = activate(&state, resolve_event(&client, &state, &event).await?).await?;
    if let Some(write) = progress_write(&play, &event, false) {
        super::super::watch::apply_jellyfin_progress(
            &state,
            write,
            if play.native_incarnation_id.is_some() {
                "transcode"
            } else {
                "direct_play"
            },
            &direct_key(&play),
        )
        .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn progress(
    client: ClientUser,
    State(state): State<AppState>,
    Json(report): Json<PlayingEvent>,
) -> Result<StatusCode, ApiError> {
    let play = resolve_event(&client, &state, &report).await?;
    if play.state != "active" {
        return Err(ApiError::Conflict("play is not active".into()));
    }
    current_file(&state, &play).await?;
    if play.native_incarnation_id.is_some() {
        let session = super::transport::route(&state, &play).await?;
        renew_passive_presence(&state, &session, &play).await;
    }
    if let Some(write) = progress_write(&play, &report, false) {
        super::super::watch::apply_jellyfin_progress(
            &state,
            write,
            if play.native_incarnation_id.is_some() {
                "transcode"
            } else {
                "direct_play"
            },
            &direct_key(&play),
        )
        .await?;
    } else if play.native_incarnation_id.is_none() {
        state.direct_plays.touch_key(&direct_key(&play));
    }
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn stopped(
    client: ClientUser,
    State(state): State<AppState>,
    Json(report): Json<PlayingEvent>,
) -> Result<StatusCode, ApiError> {
    let play = resolve_any_generation(&client, &state, &report).await?;
    if play.state == "ended" {
        release(&state, &play).await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    // A play from before the switch was saved off is already over: Stop ends
    // and releases it, but writes no progress through it.
    let current = same_generation(&play, &client.generation).is_ok();
    // Cleanup follows every result, including durable failure. An active row is
    // retained on failure so a repeated stop can retry its terminal reconciliation.
    let result = async {
        if current && play.state == "active" {
            if let Some(write) = progress_write(&play, &report, true) {
                super::super::watch::apply_jellyfin_progress(
                    &state,
                    write,
                    if play.native_incarnation_id.is_some() {
                        "transcode"
                    } else {
                        "direct_play"
                    },
                    &direct_key(&play),
                )
                .await?;
            }
        }
        state
            .store
            .end_jellyfin_play(
                &play.negotiation.play_id,
                &play.negotiation.scope,
                now_ms()?,
            )
            .await?;
        Ok::<(), ApiError>(())
    }
    .await;
    // Release the row as END left it: a publication racing this Stop may have
    // bound a native session after the read above.
    let ended = state
        .store
        .jellyfin_play(&play.negotiation.play_id, &play.negotiation.scope)
        .await
        .ok()
        .flatten()
        .unwrap_or(play);
    let released = release(&state, &ended).await;
    result?;
    released?;
    Ok(StatusCode::NO_CONTENT)
}
pub(super) struct DirectRequest {
    pub(super) media_source_id: String,
    pub(super) play_session_id: String,
}
/// Jellyfin query names are case-insensitive. A client that builds its own
/// direct URL (Android TV) sends `mediaSourceId` and `tag`, never a play id.
#[derive(Default)]
struct DirectQuery {
    media_source_id: Option<String>,
    play_session_id: Option<String>,
    tag: Option<String>,
}
fn direct_query(raw: Option<&str>) -> Result<DirectQuery, ApiError> {
    let mut query = DirectQuery::default();
    for (key, value) in query_pairs(raw)? {
        let slot = if key.eq_ignore_ascii_case("mediaSourceId") {
            &mut query.media_source_id
        } else if key.eq_ignore_ascii_case("playSessionId") {
            &mut query.play_session_id
        } else if key.eq_ignore_ascii_case("tag") {
            &mut query.tag
        } else {
            continue;
        };
        if slot.as_ref().is_some_and(|old| *old != value) {
            return Err(ApiError::BadRequest(format!("conflicting {key} values")));
        }
        *slot = Some(value);
    }
    Ok(query)
}
pub(super) const MEDIA_LINK_TTL_SECS: i64 = 86_400;
fn media_link_gone() -> ApiError {
    ApiError::typed(
        StatusCode::GONE,
        "media_link_gone",
        "This playback link has expired or its playback was stopped.",
    )
}
/// Resolve a scoped media link to its play and the login it was issued under.
/// The link authorizes this one title's direct bytes; the login it names must
/// still authenticate, so token revocation and idle expiry end it as well.
async fn media_link_play(
    state: &AppState,
    generation: &str,
    item_id: &str,
    media_source_id: Option<&str>,
    secret: &str,
) -> Result<(ClientUser, JellyfinPlay), ApiError> {
    if secret.len() != 64
        || !secret
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ApiError::NotFound("media link"));
    }
    let grant = state
        .store
        .file_grant_by_hash(&plurx_core::auth::hash_token(secret))
        .await?
        .ok_or(ApiError::NotFound("media link"))?;
    if grant.revoked_at.is_some() || !grant.source_active || grant.expires_at <= now_ms()? / 1000 {
        return Err(media_link_gone());
    }
    let play = state
        .store
        .jellyfin_play_for_direct_grant(&grant.id)
        .await?
        .ok_or_else(media_link_gone)?;
    if play.state == "ended"
        || play.negotiation.file_id != grant.file_id
        || play.negotiation.scope.user_id != grant.user_id
        || play.negotiation.switch_generation != generation
    {
        return Err(media_link_gone());
    }
    if wire_id(item_id)?.to_hex() != play.negotiation.item_wire_id
        || media_source_id
            .map(wire_id)
            .transpose()?
            .is_some_and(|source| source.to_hex() != play.negotiation.file_wire_id)
    {
        return Err(ApiError::BadRequest(
            "item/source does not belong to play".into(),
        ));
    }
    let digest = play.negotiation.scope.token_digest.clone();
    let user = super::super::extract::authenticate_token_digest(
        state,
        digest.clone(),
        plurx_core::store::TokenAudience::JellyfinCompatibility,
    )
    .await?;
    if user.id != play.negotiation.scope.user_id {
        return Err(ApiError::Unauthorized);
    }
    Ok((
        ClientUser::for_login(state, digest, generation.to_owned()).await?,
        play,
    ))
}
async fn direct_play(
    caller: MediaCaller,
    state: &AppState,
    item_id: &str,
    raw: Option<&str>,
) -> Result<(ClientUser, JellyfinPlay), ApiError> {
    let query = direct_query(raw)?;
    let caller_generation = caller.generation.clone();
    match (
        caller.client,
        query.play_session_id.as_deref(),
        query.tag.as_deref(),
    ) {
        (Some(client), Some(play_id), _) => {
            let source = query
                .media_source_id
                .as_deref()
                .ok_or_else(|| ApiError::BadRequest("MediaSourceId is required".into()))?;
            let play = binding(&client, state, play_id, item_id, source).await?;
            Ok((client, play))
        }
        (caller, _, Some(secret)) => {
            let (link_client, play) = media_link_play(
                state,
                &caller_generation,
                item_id,
                query.media_source_id.as_deref(),
                secret,
            )
            .await?;
            // A presented login must be the one the link was issued to.
            if caller.is_some_and(|client| client.token_hash != link_client.token_hash) {
                return Err(ApiError::Unauthorized);
            }
            Ok((link_client, play))
        }
        // Infuse builds `/Videos/{id}/stream?MediaSourceId=…&Static=true` with
        // its login header and no play id (J0 trace). Resolve only this
        // login's own live direct negotiation of exactly this source.
        (Some(client), None, None) => {
            let source = query
                .media_source_id
                .as_deref()
                .ok_or_else(|| ApiError::BadRequest("MediaSourceId is required".into()))?;
            let scope = scope(&client, state).await?;
            let play = state
                .store
                .jellyfin_current_direct_play(
                    &scope,
                    &wire_id(item_id)?.to_hex(),
                    &wire_id(source)?.to_hex(),
                )
                .await?
                .ok_or(ApiError::NotFound("play binding"))?;
            same_generation(&play, &client.generation)?;
            Ok((client, play))
        }
        (None, _, None) => Err(ApiError::Unauthorized),
    }
}
async fn serve_direct(
    caller: MediaCaller,
    state: AppState,
    item_id: String,
    raw: Option<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (client, play) = direct_play(caller, &state, &item_id, raw.as_deref()).await?;
    live_item(&client, &state, &item_id).await?;
    let play = activate(&state, play).await?;
    let grant = state
        .store
        .file_grant_by_id(
            play.direct_grant_id
                .as_deref()
                .ok_or(ApiError::Unauthorized)?,
        )
        .await?
        .ok_or(ApiError::Unauthorized)?;
    if grant.user_id != client.identity.native_id
        || grant.file_id != play.negotiation.file_id
        || !grant.source_active
        || grant.revoked_at.is_some()
        || grant.expires_at <= now_ms()? / 1000
    {
        return Err(ApiError::Unauthorized);
    }
    let user = state
        .store
        .get_user(client.identity.native_id)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    super::super::stream::direct(
        super::super::extract::AuthUser(user),
        State(state),
        Path(play.negotiation.file_id),
        Query(super::super::stream::DirectQuery {
            stream: Some(format!("jellyfin-direct:{}", play.negotiation.play_id)),
        }),
        method,
        headers,
    )
    .await
}
pub(super) async fn direct(
    caller: MediaCaller,
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    RawQuery(raw): RawQuery,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    serve_direct(caller, state, item_id, raw, method, headers).await
}
pub(super) async fn direct_extension(
    caller: MediaCaller,
    State(state): State<AppState>,
    Path((item_id, filename)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if matches!(filename.as_str(), "master.m3u8" | "main.m3u8") {
        for (key, _) in query_pairs(raw.as_deref())? {
            if !matches!(
                key.to_ascii_lowercase().as_str(),
                "mediasourceid" | "playsessionid" | "api_key" | "apikey"
            ) {
                return Err(ApiError::BadRequest(
                    "HLS parameters must match the negotiated play".into(),
                ));
            }
        }
        let client = caller.client.ok_or(ApiError::Unauthorized)?;
        // Query names are case-insensitive: Infuse lower-cases the first
        // letter of every key of the URL it was given (J0 trace).
        let query = direct_query(raw.as_deref())?;
        let (Some(media_source_id), Some(play_session_id)) =
            (query.media_source_id, query.play_session_id)
        else {
            return Err(ApiError::BadRequest(
                "HLS parameters must match the negotiated play".into(),
            ));
        };
        let request = DirectRequest {
            media_source_id,
            play_session_id,
        };
        return Box::pin(super::transport::root(
            client, state, item_id, request, filename, method, headers,
        ))
        .await;
    }
    if !matches!(
        filename.as_str(),
        "stream.mp4" | "stream.mkv" | "stream.webm" | "stream.ts" | "stream.avi"
    ) {
        return Err(ApiError::NotFound("media route"));
    }
    serve_direct(caller, state, item_id, raw, method, headers).await
}
async fn manual(
    client: ClientUser,
    state: AppState,
    uid: String,
    id: String,
    watched: bool,
) -> Result<Json<Value>, ApiError> {
    check_user(&client, &uid)?;
    let item = live_item(&client, &state, &id).await?;
    let native = state
        .store
        .jellyfin_resolve_entity(JellyfinEntityKind::Item, &item.id.to_hex())
        .await?
        .ok_or(ApiError::NotFound("item"))?;
    let origin = scope(&client, &state).await?;
    super::super::watch::apply_watched_with_origin(
        &state,
        origin.user_id,
        native,
        watched,
        Some(&origin),
    )
    .await?;
    let fresh = live_item(&client, &state, &id).await?;
    Ok(Json(serde_json::to_value(fresh.user_data).map_err(
        |_| ApiError::ServiceUnavailable("watch DTO unavailable".into()),
    )?))
}
pub(super) async fn mark_played(
    client: ClientUser,
    State(state): State<AppState>,
    Path((uid, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    manual(client, state, uid, id, true).await
}
pub(super) async fn mark_unplayed(
    client: ClientUser,
    State(state): State<AppState>,
    Path((uid, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    manual(client, state, uid, id, false).await
}

/// `POST`/`DELETE /UserPlayedItems/{itemId}`: the 10.9+ form of the
/// user-scoped route, with the user optional in the query.
pub(super) async fn played_item(
    client: ClientUser,
    State(state): State<AppState>,
    method: Method,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let mut user = None;
    for (key, value) in query_pairs(raw.as_deref())? {
        if key.eq_ignore_ascii_case("userId") {
            if user.as_ref().is_some_and(|old| *old != value) {
                return Err(ApiError::BadRequest("conflicting userId values".into()));
            }
            user = Some(value);
        }
    }
    let user = match user {
        Some(user) => user,
        None => client
            .identity
            .wire_id
            .clone()
            .ok_or(ApiError::Unauthorized)?,
    };
    manual(client, state, user, id, method == Method::POST).await
}

/// The login's own play named by `PlaySessionId` (case-insensitive key).
async fn named_play(
    client: &ClientUser,
    state: &AppState,
    raw: Option<&str>,
) -> Result<JellyfinPlay, ApiError> {
    let mut play_id = None;
    let mut device = None;
    for (key, value) in query_pairs(raw)? {
        let slot = if key.eq_ignore_ascii_case("playSessionId") {
            &mut play_id
        } else if key.eq_ignore_ascii_case("deviceId") {
            &mut device
        } else {
            continue;
        };
        if slot.as_ref().is_some_and(|old| *old != value) {
            return Err(ApiError::BadRequest(format!("conflicting {key} values")));
        }
        *slot = Some(value);
    }
    // Never a kill or renewal by device alone: a play is always named.
    let play_id =
        play_id.ok_or_else(|| ApiError::BadRequest("PlaySessionId is required".into()))?;
    let scope = scope(client, state).await?;
    if device.is_some_and(|device| plurx_core::auth::hash_token(&device) != scope.device_digest) {
        return Err(ApiError::Forbidden);
    }
    state
        .store
        .jellyfin_play(&wire_id(&play_id)?.to_hex(), &scope)
        .await?
        .ok_or(ApiError::NotFound("play binding"))
}

/// `POST /Sessions/Playing/Ping`: keep this login's named active play alive,
/// exactly as a position-less Progress does. It never activates or revives
/// a play, and it is not playback evidence.
pub(super) async fn ping(
    client: ClientUser,
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<StatusCode, ApiError> {
    let play = named_play(&client, &state, raw.as_deref()).await?;
    same_generation(&play, &client.generation)?;
    if play.state != "active" {
        return Err(ApiError::Conflict("play is not active".into()));
    }
    if play.native_incarnation_id.is_some() {
        let session = super::transport::route(&state, &play).await?;
        renew_passive_presence(&state, &session, &play).await;
    } else {
        state.direct_plays.touch_key(&direct_key(&play));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /Videos/ActiveEncodings`: stop the encoding of this login's named
/// play, and only that one. The binding stays, so a later Stopped still
/// commits its final position; a direct play has no encoding to stop.
pub(super) async fn active_encodings(
    client: ClientUser,
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<StatusCode, ApiError> {
    let play = named_play(&client, &state, raw.as_deref()).await?;
    // An ended play's encoding was released when it ended.
    if play.state != "ended" && play.native_incarnation_id.is_some() {
        release(&state, &play).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Reuse native digest exclusion and token deletion, retaining other logins.
pub(super) async fn logout(
    client: ClientUser,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    let scope = scope(&client, &state).await?;
    let exclusion = crate::http::internal_auth_revocation::ClusterCacheRevocation::begin_digest(
        &state,
        &client.token_hash,
    )
    .await?;
    let plays = state
        .store
        .end_jellyfin_login_plays(&scope, now_ms()?)
        .await?;
    // Revocation never waits on media cleanup: a busy release slot or an
    // unreachable owner must not leave a signed-out login valid.
    auth::revoke_token_under_exclusion(&state, &client.token_hash, exclusion).await?;
    for play in plays {
        if let Err(error) = release(&state, &play).await {
            tracing::warn!(target: "plurxd::jellyfin", ?error, "logout release left to native expiry");
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod player_identity_tests {
    use super::*;
    #[test]
    fn player_identity_is_stable_across_login_replacement_but_separates_user_device_and_family() {
        let original = JellyfinPlayScope {
            user_id: 1,
            token_digest: "a".repeat(64),
            device_digest: "b".repeat(64),
            client_family: JellyfinClientFamily::Infuse,
        };
        let mut changed = original.clone();
        changed.token_digest = "c".repeat(64);
        assert_eq!(player_id(&original), player_id(&changed));
        changed.user_id = 2;
        assert_ne!(player_id(&original), player_id(&changed));
        changed = original.clone();
        changed.device_digest = "d".repeat(64);
        assert_ne!(player_id(&original), player_id(&changed));
        changed = original.clone();
        changed.client_family = JellyfinClientFamily::AndroidTv;
        assert_ne!(player_id(&original), player_id(&changed));
    }
}
