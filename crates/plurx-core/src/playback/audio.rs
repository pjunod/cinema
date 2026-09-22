//! Audio delivery negotiation, independent of the video rung.
//!
//! A codec name says what a client can decode. An [`AudioSink`] says what the
//! route attached to that client can reproduce. Keeping those facts separate
//! prevents a video-only quality change from silently deciding that every
//! receiver is stereo.

use serde::Serialize;

use crate::domain::AudioStream;

use super::DeviceProfile;

pub const AUDIO_SAMPLE_RATE: u32 = 48_000;

/// One codec/layout claim for the client's current output route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct AudioSink {
    pub codec: String,
    pub max_channels: u8,
    #[serde(default)]
    pub passthrough: bool,
    /// Sample rates this current route proved it can reproduce for `codec`.
    /// Empty is no claim, never an all-rates wildcard.
    #[serde(default)]
    pub sample_rates_hz: Vec<u32>,
}

/// The delivery envelope whose mux rules constrain audio copying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRoute {
    Progressive,
    RollingHls,
    EncodedVod,
}

/// A measured matrix is not available yet. This identity records what must
/// be measured without inventing gains or assuming channel order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownmixMatrix {
    RequiresLayoutMeasurement { source_channels: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudioAction {
    None,
    Copy {
        codec: String,
        channels: u8,
    },
    Encode {
        codec: String,
        channels: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        layout: Option<&'static str>,
        bitrate_kbps: u32,
        sample_rate: u32,
    },
}

/// The audio bytes a playback decision intends to deliver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AudioDelivery {
    pub action: AudioAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downmix: Option<DownmixMatrix>,
    pub reason: &'static str,
}

impl AudioDelivery {
    pub fn transcodes(&self) -> bool {
        matches!(self.action, AudioAction::Encode { .. })
    }
}

fn channels(stream: &AudioStream) -> u8 {
    stream
        .channels
        .and_then(|channels| u8::try_from(channels).ok())
        .filter(|channels| *channels > 0)
        .unwrap_or(2)
}

fn route_admits_copy(route: AudioRoute, codec: &str) -> bool {
    let codec = codec.to_ascii_lowercase();
    match route {
        AudioRoute::EncodedVod => false,
        AudioRoute::RollingHls => matches!(codec.as_str(), "aac" | "ac3" | "eac3" | "mp3"),
        AudioRoute::Progressive => matches!(
            codec.as_str(),
            "aac" | "ac3" | "eac3" | "mp3" | "flac" | "alac"
        ),
    }
}

fn encoded(
    codec: &'static str,
    channels: u8,
    bitrate_kbps: u32,
    source_channels: u8,
    reason: &'static str,
) -> AudioDelivery {
    AudioDelivery {
        action: AudioAction::Encode {
            codec: codec.to_owned(),
            channels,
            layout: (channels == 6).then_some("5.1"),
            bitrate_kbps,
            sample_rate: AUDIO_SAMPLE_RATE,
        },
        downmix: (channels < source_channels)
            .then_some(DownmixMatrix::RequiresLayoutMeasurement { source_channels }),
        reason,
    }
}

/// Resolve audio without consulting the video rung.
///
/// An empty sink map is the legacy contract: compatible copy/remux audio is
/// preserved, while a full video transcode remains stereo AAC at 160 kbit/s.
/// That distinction is intentional. Treating an absent sink claim as a new
/// two-channel refusal would downmix existing direct plays before any client
/// had learned to report its route.
pub fn resolve_audio(
    source: Option<&AudioStream>,
    profile: &DeviceProfile,
    route: AudioRoute,
    audio_offset_ms: i64,
) -> AudioDelivery {
    let Some(source) = source else {
        return AudioDelivery {
            action: AudioAction::None,
            downmix: None,
            reason: "source has no audio stream",
        };
    };
    let codec = source.codec.trim().to_ascii_lowercase();
    let source_channels = channels(source);
    let explicit_sink = profile.audio_sink_claims.get(&codec);
    let legacy_claim = profile.max_audio_channels.is_empty();
    let codec_allowed = profile
        .audio_codecs
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&codec));
    let fits_sink = explicit_sink.is_some_and(|sink| source_channels <= sink.max_channels);
    let rate_admitted = explicit_sink.is_some_and(|sink| {
        source.sample_rate.is_some_and(|rate| {
            u32::try_from(rate)
                .ok()
                .is_some_and(|rate| sink.sample_rates_hz.contains(&rate))
        })
    });
    let trust_admitted = explicit_sink
        .is_some_and(|sink| sink.passthrough || profile.claimed_audio_decoders.contains(&codec));
    let copy_admitted = route_admits_copy(route, &codec)
        && audio_offset_ms == 0
        && ((fits_sink && rate_admitted && trust_admitted)
            || (legacy_claim && codec_allowed && route == AudioRoute::Progressive));
    if copy_admitted {
        return AudioDelivery {
            action: AudioAction::Copy {
                codec,
                channels: source_channels,
            },
            downmix: None,
            reason: "selected audio is compatible with the current sink and route",
        };
    }

    // Until a client supplies a sink claim, reproduce the two existing encode
    // paths exactly: full video transcodes use stereo AAC 160 kbit/s; a
    // copy-video remux converts six channels to AAC 5.1 at 320 kbit/s and
    // otherwise retains the source channel count at 256 kbit/s.
    if legacy_claim {
        return match route {
            AudioRoute::Progressive if source_channels == 6 => encoded(
                "aac",
                6,
                320,
                source_channels,
                "legacy copy-video audio conversion",
            ),
            AudioRoute::Progressive => encoded(
                "aac",
                source_channels,
                256,
                source_channels,
                "legacy copy-video audio conversion",
            ),
            AudioRoute::RollingHls | AudioRoute::EncodedVod => encoded(
                "aac",
                2,
                160,
                source_channels,
                "legacy transcode audio default",
            ),
        };
    }

    if route != AudioRoute::EncodedVod {
        if output_admitted(profile, "eac3", 6) {
            return encoded(
                "eac3",
                source_channels.min(6),
                if source_channels.min(6) == 6 {
                    640
                } else {
                    384
                },
                source_channels,
                "current sink admits E-AC-3",
            );
        }
        if output_admitted(profile, "ac3", 6) {
            return encoded(
                "ac3",
                source_channels.min(6),
                640,
                source_channels,
                "current sink admits AC-3",
            );
        }
    }

    let sink_channels = if output_admitted(profile, "aac", 1) {
        profile
            .max_audio_channels
            .get("aac")
            .copied()
            .unwrap_or(2)
            .min(6)
    } else {
        2
    };
    let output_channels = source_channels.min(sink_channels).max(1);
    encoded(
        "aac",
        output_channels,
        if output_channels == 6 { 320 } else { 160 },
        source_channels,
        if route == AudioRoute::EncodedVod {
            "encoded VOD keeps the film-global AAC lattice"
        } else {
            "AAC fallback for the current sink"
        },
    )
}

fn output_admitted(profile: &DeviceProfile, codec: &str, minimum_channels: u8) -> bool {
    profile.audio_sink_claims.get(codec).is_some_and(|sink| {
        sink.max_channels >= minimum_channels
            && sink.sample_rates_hz.contains(&AUDIO_SAMPLE_RATE)
            && (sink.passthrough || profile.claimed_audio_decoders.contains(codec))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::{default_profile, DeviceCaps};

    fn source(codec: &str, channels: i64) -> AudioStream {
        AudioStream {
            codec: codec.to_owned(),
            channels: Some(channels),
            sample_rate: Some(i64::from(AUDIO_SAMPLE_RATE)),
            ..AudioStream::default()
        }
    }

    fn claimed(sinks: &[(&str, u8)]) -> DeviceProfile {
        let mut profile = default_profile().clone();
        profile.audio_codecs = sinks.iter().map(|(codec, _)| (*codec).to_owned()).collect();
        profile.max_audio_channels = sinks
            .iter()
            .map(|(codec, channels)| ((*codec).to_owned(), *channels))
            .collect();
        profile.claimed_audio_decoders =
            sinks.iter().map(|(codec, _)| (*codec).to_owned()).collect();
        profile.audio_sink_claims = sinks
            .iter()
            .map(|(codec, channels)| {
                (
                    (*codec).to_owned(),
                    AudioSink {
                        codec: (*codec).to_owned(),
                        max_channels: *channels,
                        passthrough: false,
                        sample_rates_hz: vec![AUDIO_SAMPLE_RATE],
                    },
                )
            })
            .collect();
        profile
    }

    #[test]
    fn truehd_five_one_prefers_eac3_on_the_rolling_route() {
        let delivery = resolve_audio(
            Some(&source("truehd", 6)),
            &claimed(&[("eac3", 6), ("aac", 6)]),
            AudioRoute::RollingHls,
            0,
        );
        assert!(matches!(
            delivery.action,
            AudioAction::Encode {
                ref codec,
                channels: 6,
                bitrate_kbps: 640,
                ..
            } if codec == "eac3"
        ));
    }

    #[test]
    fn encoded_vod_keeps_aac_and_the_film_global_sample_rate() {
        let delivery = resolve_audio(
            Some(&source("truehd", 6)),
            &claimed(&[("eac3", 6), ("aac", 6)]),
            AudioRoute::EncodedVod,
            0,
        );
        assert!(matches!(
            delivery.action,
            AudioAction::Encode {
                ref codec,
                channels: 6,
                bitrate_kbps: 320,
                sample_rate: AUDIO_SAMPLE_RATE,
                layout: Some("5.1"),
            } if codec == "aac"
        ));
    }

    #[test]
    fn compatible_aac_five_one_copies_only_when_the_sink_claims_six_channels() {
        let source = source("aac", 6);
        let six = resolve_audio(
            Some(&source),
            &claimed(&[("aac", 6)]),
            AudioRoute::RollingHls,
            0,
        );
        assert!(matches!(six.action, AudioAction::Copy { channels: 6, .. }));

        let stereo = resolve_audio(
            Some(&source),
            &claimed(&[("aac", 2)]),
            AudioRoute::RollingHls,
            0,
        );
        assert!(matches!(
            stereo.downmix,
            Some(DownmixMatrix::RequiresLayoutMeasurement { source_channels: 6 })
        ));
    }

    #[test]
    fn audio_offset_forces_an_encode() {
        let delivery = resolve_audio(
            Some(&source("aac", 6)),
            &claimed(&[("aac", 6)]),
            AudioRoute::RollingHls,
            25,
        );
        assert!(delivery.transcodes());
    }

    #[test]
    fn absent_sink_claim_preserves_legacy_copy_and_transcode_answers() {
        let profile = default_profile();
        let source = source("aac", 6);
        assert!(matches!(
            resolve_audio(Some(&source), profile, AudioRoute::Progressive, 0).action,
            AudioAction::Copy { channels: 6, .. }
        ));
        assert!(matches!(
            resolve_audio(Some(&source), profile, AudioRoute::RollingHls, 0).action,
            AudioAction::Encode {
                channels: 2,
                bitrate_kbps: 160,
                ..
            }
        ));
    }

    #[test]
    fn sink_claims_are_bounded_and_unambiguous() {
        let mut caps = DeviceCaps {
            audio_sinks: vec![
                AudioSink {
                    codec: "eac3".into(),
                    max_channels: 6,
                    passthrough: true,
                    sample_rates_hz: vec![48_000],
                },
                AudioSink {
                    codec: "EAC3".into(),
                    max_channels: 2,
                    passthrough: false,
                    sample_rates_hz: vec![48_000],
                },
            ],
            ..DeviceCaps::default()
        };
        assert_eq!(
            caps.validate_audio_sinks(),
            Err("audio_sinks contains a duplicate codec")
        );
        caps.audio_sinks[1].codec = "bad codec".into();
        assert_eq!(
            caps.validate_audio_sinks(),
            Err("audio_sinks contains an invalid codec")
        );
        caps.audio_sinks[1].codec = "aac".into();
        caps.audio_sinks[1].max_channels = 0;
        assert_eq!(
            caps.validate_audio_sinks(),
            Err("audio_sinks max_channels must be between 1 and 16")
        );
        caps.audio_sinks[1].max_channels = 2;
        caps.audio_sinks[1].sample_rates_hz = vec![48_000, 48_000];
        assert_eq!(
            caps.validate_audio_sinks(),
            Err("audio_sinks contains a duplicate sample rate")
        );
        caps.audio_sinks[1].sample_rates_hz = vec![1];
        assert_eq!(
            caps.validate_audio_sinks(),
            Err("audio_sinks contains an invalid sample rate")
        );
    }

    #[test]
    fn v2_sink_claims_feed_the_one_profile_translation() {
        let caps = DeviceCaps {
            audio_sinks: vec![AudioSink {
                codec: "EAC3".into(),
                max_channels: 6,
                passthrough: true,
                sample_rates_hz: vec![48_000],
            }],
            ..DeviceCaps::default()
        };
        let profile = DeviceProfile::from_caps_v2(&caps);
        assert_eq!(profile.max_audio_channels.get("eac3"), Some(&6));
        assert!(!profile.audio_codecs.iter().any(|codec| codec == "eac3"));
        assert!(profile.audio_sink_claims["eac3"].passthrough);
    }

    #[test]
    fn sink_only_and_false_passthrough_do_not_authorize_copy() {
        for passthrough in [false, true] {
            let caps = DeviceCaps {
                audio_sinks: vec![AudioSink {
                    codec: "eac3".into(),
                    max_channels: 6,
                    passthrough,
                    sample_rates_hz: vec![48_000],
                }],
                ..DeviceCaps::default()
            };
            let profile = DeviceProfile::from_caps_v2(&caps);
            let delivery = resolve_audio(
                Some(&source("eac3", 6)),
                &profile,
                AudioRoute::RollingHls,
                0,
            );
            assert_eq!(
                matches!(delivery.action, AudioAction::Copy { .. }),
                passthrough,
                "only a true passthrough claim can replace decoder evidence"
            );
        }
    }

    #[test]
    fn copy_requires_known_compatible_source_and_sink_sample_rates() {
        let profile = claimed(&[("aac", 6)]);
        let mut input = source("aac", 6);
        input.sample_rate = None;
        assert!(resolve_audio(Some(&input), &profile, AudioRoute::RollingHls, 0).transcodes());

        input.sample_rate = Some(44_100);
        assert!(resolve_audio(Some(&input), &profile, AudioRoute::RollingHls, 0).transcodes());

        input.sample_rate = Some(48_000);
        assert!(matches!(
            resolve_audio(Some(&input), &profile, AudioRoute::RollingHls, 0).action,
            AudioAction::Copy { .. }
        ));
    }
}
