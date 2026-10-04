//! Negotiation asks the native resolver and exact recipe prerequisites before offering VOD.
use super::*;
use plurx_compat_jellyfin::profile::{
    CodecKind, CodecRule, ContainerRule, Fact, Facts, MediaKind, Property, TranscodingRule,
};
use plurx_core::store::PlaybackPlanningSnapshot;

pub(super) struct Requested<'a> {
    pub profile: &'a Value,
    pub playback_id: &'a str,
    pub play_id: &'a str,
    pub start_ms: i64,
    pub audio: Option<i64>,
    pub subtitle: Option<i64>,
    pub bitrate: Option<i64>,
    pub allow_copy: bool,
    pub allow_encode: bool,
    pub allow_audio_copy: bool,
    pub source_facts: &'a Facts,
}

pub(super) struct Plan {
    pub body: Value,
    pub fingerprint: String,
    pub bitrate: Option<u32>,
    pub inline_init: bool,
}

fn ceiling(profile: &Value, requested: Option<i64>) -> Option<Result<Option<u32>, ApiError>> {
    let mut limits = Vec::new();
    if let Some(value) = requested {
        limits.push(value);
    }
    if let Some(value) = profile.get("MaxStreamingBitrate") {
        limits.push(value.as_i64()?);
    }
    if limits.iter().any(|v| !(64_000..=1_000_000_000).contains(v)) {
        return Some(Err(ApiError::BadRequest(
            "unsupported streaming bitrate ceiling".into(),
        )));
    }
    Some(Ok(limits.into_iter().min().map(|v| v as u32)))
}

fn capabilities(rule: &TranscodingRule) -> Result<plurx_core::playback::DeviceCaps, ApiError> {
    let split = |list: &Option<String>| {
        list.as_deref()
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>()
    };
    let video = split(&rule.video_codec)
        .into_iter()
        .map(|codec| json!({"codec":codec,"present":["sdr"],"dv_profiles":[]}))
        .collect::<Vec<_>>();
    let audio = split(&rule.audio_codec);
    let audio_sinks = rule.max_audio_channels.as_deref().map(|channels|
        audio.iter().map(|codec| json!({"codec":codec,"max_channels":channels.parse::<u32>().unwrap_or(0)})).collect::<Vec<_>>()).unwrap_or_default();
    serde_json::from_value(
        json!({"v":2,"video":video,"audio":audio,"audio_sinks":audio_sinks,
        "containers":[rule.container],"transports":["hls"]}),
    )
    .map_err(|_| ApiError::BadRequest("unsupported output capabilities".into()))
}

fn accepts(
    profile: &Value,
    rule: &TranscodingRule,
    video: &str,
    audio: &str,
    facts: &Facts,
) -> bool {
    if !rule.accepts_output(&rule.container, video, audio, facts) {
        return false;
    }
    let codec_rules = profile
        .get("CodecProfiles")
        .filter(|v| !v.is_null())
        .map(|v| serde_json::from_value::<Vec<CodecRule>>(v.clone()));
    if let Some(rules) = codec_rules {
        let Ok(rules) = rules else {
            return false;
        };
        if rules.len() > 64
            || rules.iter().any(|r| {
                !r.accepts(
                    CodecKind::Video,
                    Some(video),
                    Some(&rule.container),
                    Some("mp4"),
                    facts,
                ) || !r.accepts(
                    CodecKind::VideoAudio,
                    Some(audio),
                    Some(&rule.container),
                    Some("mp4"),
                    facts,
                )
            })
        {
            return false;
        }
    }
    let container_rules = profile
        .get("ContainerProfiles")
        .filter(|v| !v.is_null())
        .map(|v| serde_json::from_value::<Vec<ContainerRule>>(v.clone()));
    if let Some(rules) = container_rules {
        let Ok(rules) = rules else {
            return false;
        };
        if rules.len() > 64
            || rules
                .iter()
                .any(|r| !r.accepts(MediaKind::Video, Some(&rule.container), facts))
        {
            return false;
        }
    }
    true
}

// Source-only composition: negotiation will call this only with an authenticated HLS handler
// installed, so no partially implemented URL is advertised by the current effort branch.
pub(super) async fn negotiate(
    state: &AppState,
    user_id: i64,
    snapshot: &PlaybackPlanningSnapshot,
    ask: Requested<'_>,
) -> Result<Option<Plan>, ApiError> {
    let Some(object) = ask.profile.as_object() else {
        return Ok(None);
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
        return Ok(None);
    }
    let Some(Ok(bitrate)) = ceiling(ask.profile, ask.bitrate) else {
        return Ok(None);
    };
    let Some(raw_rules) = ask.profile.get("TranscodingProfiles") else {
        return Ok(None);
    };
    let Ok(rules) = plurx_compat_jellyfin::profile::video_transcoding_rules(raw_rules) else {
        return Ok(None);
    };
    let file = &snapshot.file;
    let audio = ask
        .audio
        .and_then(|index| file.audio_streams.get(index as usize))
        .or_else(|| file.audio_streams.first());
    let burn = ask
        .subtitle
        .and_then(|index| file.subtitle_streams.get(index as usize))
        .filter(|track| plurx_core::tracks::subtitle_requires_burn(&track.codec))
        .map(|_| ask.subtitle.expect("selected track"));
    for rule in rules {
        if rule.protocol != "hls" || !matches!(rule.container.as_str(), "ts" | "mp4") {
            continue;
        }
        let manifest_subtitles =
            rule.enable_subtitles_in_manifest || rule.manifest_subtitles.is_some();
        if ask.subtitle.is_some() && burn.is_none() && !manifest_subtitles {
            continue;
        }
        let caps = capabilities(&rule)?;
        super::super::stream::validate_device_caps(&caps)?;
        let profile = plurx_core::playback::DeviceProfile::from_caps_v2(&caps);
        let mut selected_file = file.clone();
        if let Some(audio) = audio {
            selected_file.audio_streams = vec![audio.clone()];
        }
        let decision = plurx_core::playback::decide(
            &selected_file,
            &profile,
            &super::super::stream::render_caps_from_snapshot(state, snapshot),
        );
        let can_copy = burn.is_none()
            && ask.allow_copy
            && decision.method != plurx_core::playback::PlaybackMethod::Transcode;
        // A rejected copy remains one independently checked native encoded candidate from this
        // same rule; it never borrows codecs or limits from a different output profile.
        for copy in [true, false] {
            if copy && !can_copy || !copy && !ask.allow_encode {
                continue;
            }
            let mut body = json!({"playback_id":ask.playback_id,"request_id":format!("jellyfin:{}", ask.play_id),
                "quality_auto":true,"start":ask.start_ms as f64 / 1000.0,"audio":ask.audio,
                "subtitle":if burn.is_some() { None } else { ask.subtitle },"subtitle_burn":burn,"native_subtitles":manifest_subtitles,
                "copy":copy,"aac":copy && audio.is_some_and(|a| !ask.allow_audio_copy || !rule.audio_codec.as_deref().unwrap_or("").split(',').any(|c| c.trim().eq_ignore_ascii_case(&a.codec))),
                "preserve_dolby_vision":false,"hdr10":false,"presentation":"vod","transport":"native","caps":caps});
            let native = serde_json::from_value(body.clone())
                .map_err(|_| ApiError::BadRequest("native request normalization failed".into()))?;
            let Ok((resolved, encoding)) = Box::pin(super::super::hls::preview_for_compatibility(
                state, user_id, snapshot, native, bitrate,
            ))
            .await
            else {
                continue;
            };
            let mut facts = ask.source_facts.clone();
            let (video, output_audio) = if let Some(encoding) = encoding {
                let contract = encoding.plan.output_contract();
                if contract.output_grade() != plurx_core::transcode::OutputGrade::Sdr {
                    continue;
                }
                // Source-only facts cannot describe encoded bytes. Start with measured output
                // axes and leave unavailable output facts absent for the protocol predicate.
                facts.clear();
                if let Some(width) = contract.effective_width() {
                    facts.insert(Property::Width, Fact::Number(f64::from(width)));
                }
                if let Some(height) = contract.effective_height() {
                    facts.insert(Property::Height, Fact::Number(f64::from(height)));
                }
                facts.insert(Property::VideoRangeType, Fact::Text("SDR".into()));
                facts.insert(Property::IsAvc, Fact::Boolean(true));
                facts.insert(Property::NumVideoStreams, Fact::Number(1.0));
                facts.insert(
                    Property::NumAudioStreams,
                    Fact::Number(if audio.is_some() { 1.0 } else { 0.0 }),
                );
                facts.insert(
                    Property::NumStreams,
                    Fact::Number(if audio.is_some() { 2.0 } else { 1.0 }),
                );
                facts.insert(
                    Property::VideoFramerate,
                    Fact::Number(
                        f64::from(encoding.grid.numerator) / f64::from(encoding.grid.denominator),
                    ),
                );
                if encoding.options.effective_rate_control
                    == plurx_core::transcode::EffectiveRateControl::Vbr
                {
                    facts.insert(
                        Property::VideoBitrate,
                        Fact::Number(f64::from(encoding.options.video_bitrate_kbps) * 1500.0),
                    );
                }
                facts.insert(
                    Property::VideoBitDepth,
                    Fact::Number(if contract.output_pixel_format().contains("10") {
                        10.0
                    } else {
                        8.0
                    }),
                );
                if let Some(value) = contract.output_profile() {
                    facts.insert(Property::VideoProfile, Fact::Text(value.into()));
                }
                if audio.is_some() {
                    facts.insert(
                        Property::AudioChannels,
                        Fact::Number(f64::from(encoding.options.audio_channels)),
                    );
                    facts.insert(
                        Property::AudioBitrate,
                        Fact::Number(f64::from(encoding.options.audio_bitrate_kbps) * 1000.0),
                    );
                    facts.insert(Property::AudioSampleRate, Fact::Number(48_000.0));
                    facts.insert(Property::AudioProfile, Fact::Text("LC".into()));
                }
                (
                    contract.output_codec().to_owned(),
                    if audio.is_some() {
                        "aac".to_owned()
                    } else {
                        String::new()
                    },
                )
            } else {
                let crate::transcode::SessionKind::Copy { aac, .. } = resolved.request.kind else {
                    continue;
                };
                if file.hdr.as_deref().is_some_and(|v| v != "sdr") {
                    continue;
                }
                if aac {
                    facts.insert(Property::AudioProfile, Fact::Text("LC".into()));
                    facts.insert(Property::AudioSampleRate, Fact::Number(48_000.0));
                    facts.insert(
                        Property::AudioBitrate,
                        Fact::Number(if audio.and_then(|a| a.channels) == Some(6) {
                            320_000.0
                        } else {
                            256_000.0
                        }),
                    );
                }
                (
                    file.video_codec.clone().unwrap_or_default(),
                    if aac {
                        "aac".into()
                    } else {
                        audio.map(|a| a.codec.clone()).unwrap_or_default()
                    },
                )
            };
            if !accepts(ask.profile, &rule, &video, &output_audio, &facts) {
                continue;
            }
            body["height"] = json!(resolved.height);
            // Freeze the advertised plan only after checking the actual activation body.
            let final_body = serde_json::from_value(body.clone())
                .map_err(|_| ApiError::BadRequest("native request normalization failed".into()))?;
            let Ok((confirmed, _)) = Box::pin(super::super::hls::preview_for_compatibility(
                state, user_id, snapshot, final_body, bitrate,
            ))
            .await
            else {
                continue;
            };
            if confirmed.intent_fingerprint != resolved.intent_fingerprint {
                continue;
            }
            return Ok(Some(Plan {
                body,
                fingerprint: resolved.intent_fingerprint,
                bitrate,
                inline_init: rule.container == "ts",
            }));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_output_checks_keep_codec_predicates_and_channel_limits_on_one_rule() {
        let profile = json!({"CodecProfiles":[{"Type":"Video","Codec":"hevc","Conditions":[{"Property":"Height","Condition":"LessThanEqual","Value":"1080","IsRequired":true}]}],"TranscodingProfiles":[{"Type":"Video","Container":"ts","VideoCodec":"hevc,h264","AudioCodec":"aac","Protocol":"hls","MaxAudioChannels":"2"},{"Type":"Video","Container":"mp4","VideoCodec":"vp9","AudioCodec":"opus","Protocol":"hls"}]});
        let rules = plurx_compat_jellyfin::profile::video_transcoding_rules(
            &profile["TranscodingProfiles"],
        )
        .expect("two distinct tuples");
        let facts = Facts::from([
            (Property::Height, Fact::Number(1080.0)),
            (Property::AudioChannels, Fact::Number(2.0)),
        ]);
        assert!(accepts(&profile, &rules[0], "hevc", "aac", &facts));
        assert!(!accepts(&profile, &rules[0], "vp9", "opus", &facts));
        let mut large = facts.clone();
        large.insert(Property::Height, Fact::Number(2160.0));
        assert!(!accepts(&profile, &rules[0], "hevc", "aac", &large));
        let mut six = facts;
        six.insert(Property::AudioChannels, Fact::Number(6.0));
        assert!(!accepts(&profile, &rules[0], "hevc", "aac", &six));
        let caps = capabilities(&rules[0]).expect("one rule caps");
        assert_eq!(caps.video.len(), 2);
        assert_eq!(caps.audio, vec!["aac"]);
        assert_eq!(caps.audio_sinks[0].max_channels, 2);
        assert!(caps
            .video
            .iter()
            .all(|v| v.present == vec![plurx_core::playback::Transfer::Sdr]
                && v.dv_profiles.is_empty()));
        assert!(
            ceiling(&json!({"MaxStreamingBitrate":200_000_000}), Some(750_000))
                .expect("valid")
                .expect("valid")
                == Some(750_000)
        );
        assert!(ceiling(&json!({}), Some(-1))
            .expect("typed refusal")
            .is_err());
    }
}
