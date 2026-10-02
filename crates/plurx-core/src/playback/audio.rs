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

/// Only the client's audio authority needed to re-resolve a selected track.
/// Persisted independently of a server-authored output decision so a retry's
/// intent cannot change merely because the source or server was refreshed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct AudioClaim {
    pub decoders: Vec<String>,
    pub sinks: Vec<AudioSink>,
}

impl AudioClaim {
    pub fn from_caps(caps: &super::DeviceCaps) -> Result<Option<Self>, &'static str> {
        if caps.v != super::DeviceCaps::VERSION || caps.audio_sinks.is_empty() {
            return Ok(None);
        }
        caps.validate_audio_sinks()?;
        let mut decoders: Vec<_> = caps
            .audio
            .iter()
            .map(|codec| codec.trim().to_ascii_lowercase())
            .filter(|codec| {
                matches!(
                    codec.as_str(),
                    "aac" | "ac3" | "eac3" | "mp3" | "flac" | "alac"
                )
            })
            .collect();
        decoders.sort();
        decoders.dedup();
        let mut sinks = caps.audio_sinks.clone();
        for sink in &mut sinks {
            sink.codec = sink.codec.trim().to_ascii_lowercase();
            sink.sample_rates_hz.sort_unstable();
        }
        sinks.sort_by(|a, b| a.codec.cmp(&b.codec));
        Ok(Some(Self { decoders, sinks }))
    }

    pub fn profile(&self) -> DeviceProfile {
        DeviceProfile::from_caps_v2(&super::DeviceCaps {
            v: super::DeviceCaps::VERSION,
            audio: self.decoders.clone(),
            audio_sinks: self.sinks.clone(),
            ..super::DeviceCaps::default()
        })
    }

    pub fn valid_snapshot(&self) -> bool {
        self.decoders.len() <= 6
            && self.decoders.iter().all(|codec| {
                matches!(
                    codec.as_str(),
                    "aac" | "ac3" | "eac3" | "mp3" | "flac" | "alac"
                )
            })
            && super::DeviceCaps {
                audio_sinks: self.sinks.clone(),
                ..super::DeviceCaps::default()
            }
            .validate_audio_sinks()
            .is_ok()
    }
}

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

/// How a multichannel source is folded to fewer channels.
///
/// The stereo rows were chosen by the real-content measurement recorded in
/// `docs/streaming/AUDIO-DOWNMIX-REAL-CONTENT-QUALIFICATION.md`: the
/// incumbent `-ac 2` fold matches the ITU-R BS.775 Lo/Ro centre gain on most
/// sources, but it is unlimited, so loud film scenes decode with samples at or
/// above full scale after AAC, and AC-3/E-AC-3 sources silently apply their
/// stored centre/surround levels instead. Every stereo row therefore converts
/// to float first (TrueHD and DTS-HD MA decode to 32-bit integers, and `pan`
/// mixes in the decoder's format), applies a named matrix where the source
/// layout is known, and ends in one look-ahead limiter at −4 dBFS — the
/// ceiling that kept every measured window below −2 dBFS after AAC overshoot.
///
/// A row is selected only from the source's own layout spelling with a
/// matching channel count; an absent or unrecognised spelling takes the
/// limited default fold, never a matrix guessed from the channel count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownmixMatrix {
    /// The incumbent unlimited fold. Retained for durable snapshots and for
    /// non-stereo targets (7.1 → 5.1), which this measurement did not cover.
    RequiresLayoutMeasurement { source_channels: u8 },
    /// `5.1` (back surrounds) → Lo/Ro, limited.
    #[serde(rename = "lo_ro_5_1_back")]
    LoRo51Back,
    /// `5.1(side)` → Lo/Ro, limited.
    #[serde(rename = "lo_ro_5_1_side")]
    LoRo51Side,
    /// `7.1` → Lo/Ro with both surround pairs at −6 dB, limited.
    #[serde(rename = "lo_ro_7_1")]
    LoRo71,
    /// Any other layout: FFmpeg's float default fold to stereo, limited.
    LimitedDefault { source_channels: u8 },
}

/// −4 dBFS as linear amplitude: the ceiling the real-content receipt chose.
const DOWNMIX_LIMIT: &str = "alimiter=limit=0.6309573444801932:level=0:latency=1";

impl DownmixMatrix {
    /// The stereo fold for a source with `source_channels` channels and the
    /// probe's opaque `layout` spelling.
    pub fn stereo_for(source_channels: u8, layout: Option<&str>) -> Self {
        match (layout, source_channels) {
            (Some("5.1"), 6) => Self::LoRo51Back,
            (Some("5.1(side)"), 6) => Self::LoRo51Side,
            (Some("7.1"), 8) => Self::LoRo71,
            _ => Self::LimitedDefault { source_channels },
        }
    }

    /// The filter chain this matrix contributes to the audio `-af`, or
    /// `None` for the incumbent fold, which `-ac` alone performs.
    pub fn filter(&self) -> Option<String> {
        let pan = match self {
            Self::RequiresLayoutMeasurement { .. } => return None,
            Self::LoRo51Back => "pan=stereo|FL=FL+0.707*FC+0.707*BL|FR=FR+0.707*FC+0.707*BR",
            Self::LoRo51Side => "pan=stereo|FL=FL+0.707*FC+0.707*SL|FR=FR+0.707*FC+0.707*SR",
            Self::LoRo71 => "pan=stereo|FL=FL+0.707*FC+0.5*SL+0.5*BL|FR=FR+0.707*FC+0.5*SR+0.5*BR",
            Self::LimitedDefault { .. } => {
                return Some(format!(
                    "aformat=sample_fmts=fltp:channel_layouts=stereo,{DOWNMIX_LIMIT}"
                ))
            }
        };
        Some(format!("aformat=sample_fmts=fltp,{pan},{DOWNMIX_LIMIT}"))
    }

    fn valid(&self) -> bool {
        match self {
            Self::RequiresLayoutMeasurement { source_channels }
            | Self::LimitedDefault { source_channels } => *source_channels > 0,
            Self::LoRo51Back | Self::LoRo51Side | Self::LoRo71 => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
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
        layout: Option<String>,
        bitrate_kbps: u32,
        sample_rate: u32,
    },
}

/// The audio bytes a playback decision intends to deliver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct AudioDelivery {
    pub action: AudioAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downmix: Option<DownmixMatrix>,
    pub reason: String,
}

impl AudioDelivery {
    pub fn parse_encoded_snapshot(snapshot: &str) -> Option<Self> {
        let audio: Self = serde_json::from_str(snapshot).ok()?;
        audio.is_encoded_vod_compatible().then_some(audio)
    }

    pub fn is_encoded_vod_compatible(&self) -> bool {
        self.valid_snapshot()
            && (matches!(&self.action, AudioAction::None)
                || matches!(&self.action, AudioAction::Encode { codec, .. } if codec == "aac"))
    }
    pub fn transcodes(&self) -> bool {
        matches!(self.action, AudioAction::Encode { .. })
    }

    /// Canonical byte semantics, excluding the explanatory reason. This is
    /// shared by cache identity, durable snapshots and handoff comparison.
    pub fn byte_identity(&self) -> String {
        serde_json::to_string(&(&self.action, self.downmix))
            .expect("audio delivery contains only serializable values")
    }

    pub fn bitrate_kbps(&self) -> Option<u32> {
        match self.action {
            AudioAction::None => Some(0),
            AudioAction::Encode { bitrate_kbps, .. } => Some(bitrate_kbps),
            AudioAction::Copy { .. } => None,
        }
    }

    /// Copied source bitrate is not probed per track. Use the established
    /// copy-plan headroom estimate, never label it a measured source bitrate.
    pub fn budget_kbps(&self) -> u32 {
        self.bitrate_kbps().unwrap_or(640)
    }

    /// The measured downmix's filter chain, if this delivery folds channels
    /// with one. Callers join it into the single audio `-af` they emit.
    pub fn downmix_filter(&self) -> Option<String> {
        self.downmix.as_ref().and_then(DownmixMatrix::filter)
    }

    pub fn codec(&self) -> Option<&str> {
        match &self.action {
            AudioAction::None => None,
            AudioAction::Copy { codec, .. } | AudioAction::Encode { codec, .. } => Some(codec),
        }
    }

    /// A durable snapshot is server-authored, but storage corruption must not
    /// become arbitrary FFmpeg argv on a later owner. Unmeasured matrices
    /// deliberately remain a requirement; they supply no filter expression.
    pub fn valid_snapshot(&self) -> bool {
        let valid = match &self.action {
            AudioAction::None => self.downmix.is_none(),
            AudioAction::Copy { codec, channels } => {
                !codec.is_empty()
                    && codec.len() <= 64
                    && !codec.chars().any(char::is_control)
                    && *channels > 0
                    && self.downmix.is_none()
            }
            AudioAction::Encode {
                codec,
                channels,
                layout,
                bitrate_kbps,
                sample_rate,
            } => {
                matches!(codec.as_str(), "aac" | "ac3" | "eac3")
                    && *channels > 0
                    && (1..=1536).contains(bitrate_kbps)
                    && *sample_rate == AUDIO_SAMPLE_RATE
                    && (layout.is_none() || (*channels == 6 && layout.as_deref() == Some("5.1")))
            }
        };
        valid
            && self.reason.len() <= 256
            && self.downmix.is_none_or(|matrix| {
                matrix.valid()
                    && match matrix {
                        // A measured matrix is a stereo fold, never a label
                        // on some other output shape.
                        DownmixMatrix::RequiresLayoutMeasurement { .. } => true,
                        _ => matches!(&self.action, AudioAction::Encode { channels: 2, .. }),
                    }
            })
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
    source: &AudioStream,
    reason: &'static str,
) -> AudioDelivery {
    let source_channels = self::channels(source);
    AudioDelivery {
        action: AudioAction::Encode {
            codec: codec.to_owned(),
            channels,
            layout: (channels == 6).then(|| "5.1".to_owned()),
            bitrate_kbps,
            sample_rate: AUDIO_SAMPLE_RATE,
        },
        downmix: (channels < source_channels).then(|| {
            if channels == 2 {
                DownmixMatrix::stereo_for(source_channels, source.channel_layout.as_deref())
            } else {
                DownmixMatrix::RequiresLayoutMeasurement { source_channels }
            }
        }),
        reason: reason.to_owned(),
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
            reason: "source has no audio stream".to_owned(),
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
    // A sink's channel count is what its route reproduces, which decides what
    // the server *encodes*. A client that decodes the codec itself mixes any
    // decodable layout down for its own output — exactly what it does with a
    // direct play — so a copy only has to fit the route when the bitstream is
    // passed through undecoded to a receiver.
    let fits_sink = explicit_sink.is_some_and(|sink| {
        source_channels <= sink.max_channels || profile.claimed_audio_decoders.contains(&codec)
    });
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
            reason: "selected audio is compatible with the current sink and route".to_owned(),
        };
    }

    // Until a client supplies a sink claim, reproduce the two existing encode
    // paths exactly: full video transcodes use stereo AAC 160 kbit/s; a
    // copy-video remux converts six channels to AAC 5.1 at 320 kbit/s and
    // otherwise retains the source channel count at 256 kbit/s.
    if legacy_claim {
        return match route {
            AudioRoute::Progressive if source_channels == 6 => {
                encoded("aac", 6, 320, source, "legacy copy-video audio conversion")
            }
            AudioRoute::Progressive => encoded(
                "aac",
                source_channels,
                256,
                source,
                "legacy copy-video audio conversion",
            ),
            AudioRoute::RollingHls | AudioRoute::EncodedVod => {
                encoded("aac", 2, 160, source, "legacy transcode audio default")
            }
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
                source,
                "current sink admits E-AC-3",
            );
        }
        if output_admitted(profile, "ac3", 6) {
            return encoded(
                "ac3",
                source_channels.min(6),
                640,
                source,
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
        source,
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

    #[test]
    fn audio_claim_canonicalizes_order_without_inventing_sink_authority() {
        let mut caps = DeviceCaps {
            v: DeviceCaps::VERSION,
            audio: vec!["EAC3".into(), "aac".into(), "AAC".into()],
            audio_sinks: vec![
                AudioSink {
                    codec: "EAC3".into(),
                    max_channels: 6,
                    passthrough: true,
                    sample_rates_hz: vec![48_000, 44_100],
                },
                AudioSink {
                    codec: "aac".into(),
                    max_channels: 2,
                    passthrough: false,
                    sample_rates_hz: vec![48_000],
                },
            ],
            ..DeviceCaps::default()
        };
        let claim = AudioClaim::from_caps(&caps)
            .expect("valid sinks")
            .expect("explicit claim");
        caps.audio.reverse();
        caps.audio_sinks.reverse();
        caps.audio_sinks[1].sample_rates_hz.reverse();
        assert_eq!(
            AudioClaim::from_caps(&caps).expect("valid reordered sinks"),
            Some(claim.clone())
        );
        assert!(claim.valid_snapshot());
        caps.audio_sinks.clear();
        assert_eq!(
            AudioClaim::from_caps(&caps).expect("empty sinks"),
            None,
            "decoder names alone do not prove a sink"
        );
    }

    #[test]
    fn audio_snapshot_preserves_semantics_without_trusting_filter_text() {
        let mut audio = resolve_audio(
            Some(&source("dts", 6)),
            &claimed(&[("aac", 6)]),
            AudioRoute::EncodedVod,
            0,
        );
        let identity = audio.byte_identity();
        let value = serde_json::to_string(&audio).expect("snapshot");
        assert_eq!(
            AudioDelivery::parse_encoded_snapshot(&value),
            Some(audio.clone())
        );
        audio.reason = "a different explanation".to_owned();
        assert_eq!(identity, audio.byte_identity());
        if let AudioAction::Encode { codec, .. } = &mut audio.action {
            *codec = "eac3".to_owned();
        }
        assert_ne!(identity, audio.byte_identity());
        assert!(
            AudioDelivery::parse_encoded_snapshot(
                &serde_json::to_string(&audio).expect("snapshot")
            )
            .is_none(),
            "encoded VOD retains the AAC lattice"
        );
        if let AudioAction::Encode { layout, .. } = &mut audio.action {
            *layout = Some("5.1,volume=100".to_owned());
        }
        assert!(!audio.valid_snapshot());
    }

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
                ref layout,
            } if codec == "aac" && layout.as_deref() == Some("5.1")
        ));
    }

    #[test]
    fn a_decoding_client_copies_five_one_whatever_its_route_and_a_bitstream_must_fit() {
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
        // The client decodes AAC itself and mixes it down for its stereo
        // route, as it does on a direct play: copy, no server fold.
        assert!(matches!(
            stereo.action,
            AudioAction::Copy { channels: 6, .. }
        ));

        // A passthrough-only sink (no decoder) must fit the receiver.
        let mut receiver = claimed(&[("eac3", 2)]);
        receiver.claimed_audio_decoders.clear();
        if let Some(sink) = receiver.audio_sink_claims.get_mut("eac3") {
            sink.passthrough = true;
        }
        let bitstream = resolve_audio(
            Some(&super::tests::source("eac3", 6)),
            &receiver,
            AudioRoute::RollingHls,
            0,
        );
        assert!(bitstream.transcodes(), "{bitstream:?}");

        // A codec the client cannot decode is folded by the server. No layout
        // spelling in the facts: the limited default fold, never a matrix
        // guessed from the channel count.
        let dts = resolve_audio(
            Some(&super::tests::source("dts", 6)),
            &claimed(&[("aac", 2)]),
            AudioRoute::RollingHls,
            0,
        );
        assert_eq!(
            dts.downmix,
            Some(DownmixMatrix::LimitedDefault { source_channels: 6 })
        );
    }

    fn laid_out(codec: &str, channels: i64, layout: &str) -> AudioStream {
        AudioStream {
            channel_layout: Some(layout.to_owned()),
            ..source(codec, channels)
        }
    }

    #[test]
    fn stereo_fold_takes_the_named_matrix_only_from_a_matching_layout() {
        let stereo = claimed(&[("aac", 2)]);
        let fold = |stream: AudioStream| {
            resolve_audio(Some(&stream), &stereo, AudioRoute::RollingHls, 0).downmix
        };
        assert_eq!(
            fold(laid_out("dts", 6, "5.1(side)")),
            Some(DownmixMatrix::LoRo51Side)
        );
        assert_eq!(
            fold(laid_out("dts", 6, "5.1")),
            Some(DownmixMatrix::LoRo51Back)
        );
        assert_eq!(
            fold(laid_out("truehd", 8, "7.1")),
            Some(DownmixMatrix::LoRo71)
        );
        // A spelling whose channel count disagrees is not trusted.
        assert_eq!(
            fold(laid_out("dts", 8, "5.1")),
            Some(DownmixMatrix::LimitedDefault { source_channels: 8 })
        );
        assert_eq!(
            fold(laid_out("ac3", 5, "5.0(side)")),
            Some(DownmixMatrix::LimitedDefault { source_channels: 5 })
        );
        // Stereo sources fold nothing.
        assert_eq!(fold(laid_out("dts", 2, "stereo")), None);
        // The legacy (absent-claim) transcode default folds the same way: it
        // is the path every multichannel title takes today.
        let legacy = resolve_audio(
            Some(&laid_out("truehd", 8, "7.1")),
            default_profile(),
            AudioRoute::EncodedVod,
            0,
        );
        assert_eq!(legacy.downmix, Some(DownmixMatrix::LoRo71));
    }

    #[test]
    fn non_stereo_targets_keep_the_incumbent_fold() {
        let delivery = resolve_audio(
            Some(&laid_out("truehd", 8, "7.1")),
            &claimed(&[("eac3", 6)]),
            AudioRoute::RollingHls,
            0,
        );
        assert_eq!(
            delivery.downmix,
            Some(DownmixMatrix::RequiresLayoutMeasurement { source_channels: 8 })
        );
        assert_eq!(delivery.downmix_filter(), None);
    }

    #[test]
    fn downmix_filters_are_float_matrix_then_minus_four_dbfs_limiter() {
        let limit = "alimiter=limit=0.6309573444801932:level=0:latency=1";
        assert_eq!(
            DownmixMatrix::LoRo51Side.filter().expect("measured fold"),
            format!("aformat=sample_fmts=fltp,pan=stereo|FL=FL+0.707*FC+0.707*SL|FR=FR+0.707*FC+0.707*SR,{limit}")
        );
        assert_eq!(
            DownmixMatrix::LoRo51Back.filter().expect("measured fold"),
            format!("aformat=sample_fmts=fltp,pan=stereo|FL=FL+0.707*FC+0.707*BL|FR=FR+0.707*FC+0.707*BR,{limit}")
        );
        assert_eq!(
            DownmixMatrix::LoRo71.filter().expect("measured fold"),
            format!("aformat=sample_fmts=fltp,pan=stereo|FL=FL+0.707*FC+0.5*SL+0.5*BL|FR=FR+0.707*FC+0.5*SR+0.5*BR,{limit}")
        );
        assert_eq!(
            DownmixMatrix::LimitedDefault { source_channels: 6 }
                .filter()
                .expect("measured fold"),
            format!("aformat=sample_fmts=fltp:channel_layouts=stereo,{limit}")
        );
        assert_eq!(
            DownmixMatrix::RequiresLayoutMeasurement { source_channels: 6 }.filter(),
            None
        );
    }

    #[test]
    fn a_measured_matrix_on_a_non_stereo_snapshot_is_refused() {
        let mut delivery = resolve_audio(
            Some(&laid_out("dts", 6, "5.1(side)")),
            &claimed(&[("aac", 2)]),
            AudioRoute::EncodedVod,
            0,
        );
        assert!(delivery.valid_snapshot());
        let snapshot = delivery.byte_identity();
        assert!(snapshot.contains("lo_ro_5_1_side"), "{snapshot}");
        if let AudioAction::Encode { channels, .. } = &mut delivery.action {
            *channels = 6;
        }
        assert!(!delivery.valid_snapshot());
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
