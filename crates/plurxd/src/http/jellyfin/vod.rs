//! Negotiation asks the native resolver and exact recipe prerequisites before offering VOD.
use super::*;
use plurx_compat_jellyfin::profile::{
    CodecKind, CodecRule, ContainerRule, Fact, Facts, MediaKind, Property, TranscodingRule,
};
use plurx_compat_jellyfin::subtitle::{
    delivery, DeliveryMethod, SubtitleRule, TrackFacts, Transport,
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
    /// The client's `SubtitleProfiles`, which decide per output rule whether
    /// a text track is a manifest rendition, a sidecar or a burn.
    pub subtitle_rules: &'a [SubtitleRule],
}

pub(super) struct Plan {
    pub body: Value,
    pub fingerprint: String,
    pub bitrate: Option<u32>,
    pub inline_init: bool,
    /// The chosen output can carry VTT renditions in its master.
    pub manifest_capable: bool,
    /// The chosen output's master carries the source's text renditions.
    pub manifest_subtitles: bool,
}

/// The decision facts of one native subtitle track. Text is anything the
/// native extractor turns into WebVTT: every codec that is not a bitmap.
pub(super) fn track_facts(track: &plurx_core::domain::SubtitleStream) -> TrackFacts<'_> {
    TrackFacts {
        codec: &track.codec,
        language: track.language.as_deref(),
        text: !plurx_core::tracks::is_bitmap_subtitle(&track.codec),
    }
}

/// The subtitle profile list an HLS output is judged by. A client that sends
/// none, but whose output declares `ManifestSubtitles` or
/// `EnableSubtitlesInManifest`, has said it wants VTT renditions there; that
/// declaration stands in for an `Hls` entry. Any list the client did send is
/// used as sent.
fn hls_rules(
    rules: &[SubtitleRule],
    manifest_capable: bool,
) -> std::borrow::Cow<'_, [SubtitleRule]> {
    if rules.is_empty() && manifest_capable {
        std::borrow::Cow::Owned(vec![SubtitleRule {
            format: "vtt".into(),
            method: Some(DeliveryMethod::Hls),
            language: None,
            container: None,
        }])
    } else {
        std::borrow::Cow::Borrowed(rules)
    }
}

/// One track's delivery on an HLS output. The native master carries every
/// text track when it carries any, so then every text track is reported as
/// the rendition it is, never also as a sidecar.
pub(super) fn hls_track_delivery(
    rules: &[SubtitleRule],
    track: TrackFacts<'_>,
    manifest_capable: bool,
    manifest_subtitles: bool,
) -> plurx_compat_jellyfin::subtitle::Delivery {
    if manifest_subtitles && track.text {
        return plurx_compat_jellyfin::subtitle::Delivery {
            method: DeliveryMethod::Hls,
            format: "vtt".into(),
        };
    }
    delivery(
        &hls_rules(rules, manifest_capable),
        track,
        Transport::Hls {
            manifest: manifest_capable,
        },
    )
}

/// What one HLS output does with the source's subtitles.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct RuleSubtitles {
    /// The master carries the text renditions.
    pub manifest: bool,
    /// The selected track, as a manifest rendition.
    pub subtitle: Option<i64>,
    /// The selected track, burned in.
    pub burn: Option<i64>,
}

/// `None` when this output cannot deliver the selected track: a text track
/// that would have to be burned, which the facade does not offer.
pub(super) fn rule_subtitles(
    rules: &[SubtitleRule],
    tracks: &[TrackFacts<'_>],
    selected: Option<i64>,
    manifest_capable: bool,
) -> Option<RuleSubtitles> {
    let effective = hls_rules(rules, manifest_capable);
    let transport = Transport::Hls {
        manifest: manifest_capable,
    };
    let manifest = tracks
        .iter()
        .any(|track| delivery(&effective, *track, transport).method == DeliveryMethod::Hls);
    let Some(index) = selected else {
        return Some(RuleSubtitles {
            manifest,
            subtitle: None,
            burn: None,
        });
    };
    let track = *usize::try_from(index).ok().and_then(|i| tracks.get(i))?;
    Some(
        match hls_track_delivery(rules, track, manifest_capable, manifest).method {
            DeliveryMethod::Hls => RuleSubtitles {
                manifest,
                subtitle: Some(index),
                burn: None,
            },
            // A sidecar the client fetches itself; the video carries none.
            DeliveryMethod::External | DeliveryMethod::Embed => RuleSubtitles {
                manifest,
                subtitle: None,
                burn: None,
            },
            DeliveryMethod::Encode if !track.text => RuleSubtitles {
                manifest,
                subtitle: None,
                burn: Some(index),
            },
            DeliveryMethod::Encode => return None,
        },
    )
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
    let tracks = file
        .subtitle_streams
        .iter()
        .map(track_facts)
        .collect::<Vec<_>>();
    for rule in rules {
        if rule.protocol != "hls" || !matches!(rule.container.as_str(), "ts" | "mp4") {
            continue;
        }
        // Jellyfin decides each track's delivery from the subtitle profiles
        // for this output: a manifest rendition only where an `Hls` entry
        // wins and the output can carry one, otherwise a sidecar or a burn.
        let manifest_capable =
            rule.enable_subtitles_in_manifest || rule.manifest_subtitles.is_some();
        let Some(RuleSubtitles {
            manifest: manifest_subtitles,
            subtitle,
            burn,
        }) = rule_subtitles(ask.subtitle_rules, &tracks, ask.subtitle, manifest_capable)
        else {
            continue;
        };
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
                "subtitle":subtitle,"subtitle_burn":burn,"native_subtitles":manifest_subtitles,
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
                manifest_capable,
                manifest_subtitles,
            }));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rules(value: Value) -> Vec<SubtitleRule> {
        plurx_compat_jellyfin::subtitle::subtitle_rules(Some(&value)).expect("rules")
    }
    const SRT: TrackFacts<'static> = TrackFacts {
        codec: "subrip",
        language: Some("eng"),
        text: true,
    };
    const ASS: TrackFacts<'static> = TrackFacts {
        codec: "ass",
        language: Some("eng"),
        text: true,
    };
    const PGS: TrackFacts<'static> = TrackFacts {
        codec: "hdmv_pgs_subtitle",
        language: None,
        text: false,
    };
    #[test]
    fn hls_subtitles_follow_the_profile_and_never_report_a_track_twice() {
        let tracks = [SRT, ASS, PGS];
        let outcome = |rules: &[SubtitleRule], selected, capable| {
            rule_subtitles(rules, &tracks, selected, capable)
        };
        // Both pinned clients put External first: sidecars, no renditions.
        let android = rules(
            json!([{"Format":"vtt","Method":"Embed"},{"Format":"vtt","Method":"External"},{"Format":"vtt","Method":"Hls"}]),
        );
        assert_eq!(
            outcome(&android, Some(0), true),
            Some(RuleSubtitles {
                manifest: false,
                subtitle: None,
                burn: None
            })
        );
        // An Hls-first profile on a capable output: renditions, and the
        // selection rides in the master.
        let hls =
            rules(json!([{"Format":"vtt","Method":"Hls"},{"Format":"vtt","Method":"External"}]));
        assert_eq!(
            outcome(&hls, Some(1), true),
            Some(RuleSubtitles {
                manifest: true,
                subtitle: Some(1),
                burn: None
            })
        );
        // ...but not on an output that cannot carry them.
        assert_eq!(
            outcome(&hls, Some(0), false),
            Some(RuleSubtitles {
                manifest: false,
                subtitle: None,
                burn: None
            })
        );
        // Mixed profiles: once the master carries renditions, every text
        // track is reported as one, never also as a sidecar.
        let mixed = rules(
            json!([{"Format":"vtt","Method":"External","Language":"eng"},{"Format":"vtt","Method":"Hls"}]),
        );
        let french = TrackFacts {
            language: Some("fre"),
            ..SRT
        };
        let both = [SRT, french];
        let decided = rule_subtitles(&mixed, &both, Some(0), true).expect("delivered");
        assert_eq!(
            decided,
            RuleSubtitles {
                manifest: true,
                subtitle: Some(0),
                burn: None
            }
        );
        assert_eq!(
            hls_track_delivery(&mixed, SRT, true, true).method,
            DeliveryMethod::Hls
        );
        // A bitmap selection burns; a text track that could only be burned
        // refuses this output.
        assert_eq!(
            outcome(&android, Some(2), true),
            Some(RuleSubtitles {
                manifest: false,
                subtitle: None,
                burn: Some(2)
            })
        );
        let ttml_only = rules(json!([{"Format":"ttml","Method":"External"}]));
        assert_eq!(outcome(&ttml_only, Some(0), false), None);
        // No list at all: a manifest-capable output's own declaration stands
        // in for an Hls entry; without one, text cannot be delivered.
        assert_eq!(
            outcome(&[], Some(0), true),
            Some(RuleSubtitles {
                manifest: true,
                subtitle: Some(0),
                burn: None
            })
        );
        assert_eq!(outcome(&[], Some(0), false), None);
        assert_eq!(
            outcome(&[], None, false),
            Some(RuleSubtitles {
                manifest: false,
                subtitle: None,
                burn: None
            })
        );
    }
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
