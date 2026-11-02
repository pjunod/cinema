//! Playback decision engine.
//!
//! Pure function of (file media details, device profile) → a verdict of
//! direct play, remux, or transcode (ARCHITECTURE §3). Device profiles are
//! data (`profiles.toml`), so the direct-play matrix is correctable without a
//! release (REQ-PLAY-4). Phase 1 serves DirectPlay and Remux; a Transcode
//! verdict is reported honestly and its serving lands in Phase 2.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::domain::MediaFile;

pub mod audio;
pub mod candidate;
pub mod caps;
pub mod continuous_quality;
pub mod desired;
pub mod enhancement;
pub mod geometry;
pub mod intent;
use crate::transcode::OutputGrade;
pub use audio::{resolve_audio, AudioAction, AudioDelivery, AudioRoute, AudioSink, DownmixMatrix};
pub use caps::{
    DeviceCaps, LearnedLimit, LegacyCaps, Transfer, VideoCaps, DECODER_COMPACTION_CONTRACT,
    MAX_CLIENT_DECODER_ENTRIES,
};
pub use desired::{
    DesiredCodec, DesiredDynamicRange, DesiredQuality, DesiredSelection, DesiredSubtitles,
};
pub use intent::{
    IntentAxis, IntentEnvelopeError, IntentOrder, MediaIntentEnvelope, MAX_LIFETIME_ID,
};

/// Built-in device profiles, parsed once from the embedded TOML.
static PROFILES: LazyLock<HashMap<String, DeviceProfile>> = LazyLock::new(|| {
    let raw = include_str!("profiles.toml");
    toml::from_str::<HashMap<String, DeviceProfile>>(raw)
        .expect("embedded profiles.toml is valid")
        .into_iter()
        .map(|(name, mut p)| {
            p.name = name.clone();
            (name, p)
        })
        .collect()
});

/// Look up a built-in profile by name (e.g. `web-h264`).
pub fn profile(name: &str) -> Option<&'static DeviceProfile> {
    PROFILES.get(name)
}

/// The default profile when a client names none: the browser baseline.
pub fn default_profile() -> &'static DeviceProfile {
    profile("web-h264").expect("web-h264 profile exists")
}

/// Build an ad-hoc profile from a client's runtime-probed capabilities. The web
/// player detects what the *actual* browser can decode (`canPlayType` /
/// `MediaSource.isTypeSupported`) and reports it, so a file only transcodes when
/// this specific browser genuinely can't play it — this is the runtime-probe
/// refinement the fixed named profiles were always a placeholder for.
///
/// `supports_hdr` must fold in the display: HDR is only claimed when the screen
/// is HDR-capable, so HDR-on-SDR still tone-maps (grey/washed-out otherwise).
/// Resolution is intentionally *not* capped to the display — a decodable 4K
/// stream direct-plays and the browser downscales, per "direct play when it
/// works."
pub fn caps_profile(
    containers: Vec<String>,
    video_codecs: Vec<String>,
    audio_codecs: Vec<String>,
    max_height: Option<i64>,
    supports_hdr: bool,
    supports_dolby_vision: bool,
) -> DeviceProfile {
    DeviceProfile {
        name: "client-caps".to_owned(),
        description: "runtime-probed browser capabilities".to_owned(),
        containers,
        video_codecs,
        audio_codecs,
        max_audio_channels: HashMap::new(),
        claimed_audio_decoders: HashSet::new(),
        audio_sink_claims: HashMap::new(),
        max_height,
        video_max_heights: HashMap::new(),
        max_bitrate: None,
        supports_hdr,
        supports_dolby_vision,
        dolby_vision_profiles: Vec::new(),
        remux_dolby_vision: false,
        progressive_hevc_sample_entries: None,
        presents: BTreeMap::new(),
        profile_max_heights: BTreeMap::new(),
        learned_limits: Vec::new(),
        // Set by the caller after construction, like `remux_dolby_vision` and
        // `dolby_vision_profiles`: adding a seventh positional argument to a
        // function with six booleans and vectors in a row is how a caller
        // silently swaps two of them.
        supports_hdr10_transcode: false,
    }
}

/// A manual override from the player's quality menu. `Auto` runs the normal
/// ladder; `Original` never transcodes video (direct/remux only — the caller's
/// error-fallback rescues an undecodable pick); `Transcode` forces a re-encode
/// at a client-chosen height (the height rides on the HLS start request, not
/// the verdict).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Force {
    Auto,
    Original,
    Transcode,
}

impl Force {
    pub fn parse(s: &str) -> Force {
        match s {
            "original" => Force::Original,
            "transcode" => Force::Transcode,
            _ => Force::Auto,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DeviceProfile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub containers: Vec<String>,
    pub video_codecs: Vec<String>,
    pub audio_codecs: Vec<String>,
    /// Current output-route ceilings keyed by normalized audio codec. Empty is
    /// the legacy codec-only claim and preserves its existing copy behavior.
    #[serde(default)]
    pub max_audio_channels: HashMap<String, u8>,
    /// Decoder codecs the current capability document explicitly proved.
    /// This is deliberately not the default-filled `audio_codecs` list.
    #[serde(default)]
    pub claimed_audio_decoders: HashSet<String>,
    /// Current route facts keyed by codec. Keeping the full claim preserves
    /// passthrough and sample-rate evidence instead of reducing it to a
    /// channel count that could accidentally authorize copy.
    #[serde(default)]
    pub audio_sink_claims: HashMap<String, AudioSink>,
    #[serde(default)]
    pub max_height: Option<i64>,
    /// Runtime-probed direct-play ceilings keyed by normalized video codec.
    ///
    /// `max_height` remains the backwards-compatible all-codec ceiling. A
    /// codec-specific value narrows it for clients such as Android where one
    /// hardware decoder may accept 4K HEVC while another only accepts 1080p
    /// AV1. Empty keeps every named profile and older client unchanged.
    #[serde(default)]
    pub video_max_heights: HashMap<String, i64>,
    #[serde(default)]
    pub max_bitrate: Option<i64>,
    #[serde(default)]
    pub supports_hdr: bool,
    /// Whether this client decodes a **Dolby Vision** stream as delivered.
    ///
    /// Separate from `supports_hdr`, and the distinction is the whole point:
    /// a DV track's base layer is often HDR10-compatible, so a client that
    /// reports HDR support looks able to take it — and then refuses, because
    /// what reaches its decoder is a track the container flags as Dolby
    /// Vision. Safari decodes those; Chrome does not, at any profile.
    /// Defaulted off, so a profile that has never heard of DV is assumed not
    /// to handle it.
    #[serde(default)]
    pub supports_dolby_vision: bool,
    /// Dolby Vision profile numbers this client has actually probed. New
    /// clients use this instead of the legacy all-or-nothing flag: Apple
    /// AVPlayer supports specific delivery profiles, not every disc profile.
    #[serde(default)]
    pub dolby_vision_profiles: Vec<u8>,
    /// This client has PROVED it can decode HEVC Main10 and present it as
    /// PQ, at the height in `max_height`, on the display attached right now
    /// (the wire's `hdr10t=1`).
    ///
    /// Strictly narrower than `supports_hdr`, which any HDR-capable codec
    /// satisfies, and deliberately so: this is the bit that decides whether a
    /// Dolby Vision source the client cannot decode is re-encoded to HDR10 or
    /// tone-mapped to SDR, and getting it wrong the optimistic way delivers a
    /// PQ stream to a display that renders it grey. `false` therefore means
    /// NOT PROVEN, never "proven false" — a client with no
    /// `MediaCapabilities`, an old client that never learned to send the
    /// field, and a client that genuinely cannot all read the same here, and
    /// all three get the tone-mapped path they get today.
    #[serde(default)]
    pub supports_hdr10_transcode: bool,
    /// This decoder accepts Dolby Vision through normalized HLS/fMP4 but not
    /// reliably as the source file over a progressive range URL. Apple
    /// AVPlayer can report a healthy P8 DV pipeline, advance the raw MP4's
    /// timeline, and still render only black frames. A copy remux keeps every
    /// video sample/RPU while rebuilding the delivery signaling it expects.
    #[serde(default)]
    pub remux_dolby_vision: bool,
    /// Explicit labels accepted for original progressive HEVC-in-ISO-BMFF.
    /// `None` is the legacy path; an empty list admits no progressive label.
    #[serde(default)]
    pub progressive_hevc_sample_entries: Option<Vec<String>>,
    /// Per codec: the transfer functions this client can PRESENT, not merely
    /// decode. Empty means the caller built this profile the old way — a
    /// named profile, or a `caps_profile` call — and the boolean fields above
    /// are the answer.
    #[serde(default)]
    pub presents: BTreeMap<String, BTreeSet<caps::Transfer>>,
    /// Per (codec, codec profile): the height ceiling, where the client
    /// reported one narrower than the codec's own.
    ///
    /// This is the field that lets a browser stop sending the minimum of its
    /// 8-bit and 10-bit rungs: a device that decodes 8-bit 4K but Main10 only
    /// at 1080p can say so, instead of capping every 4K title at 1080p to
    /// protect the Main10 one (PLAYBACK-CAPS-V2-PLAN §2, edge E5).
    #[serde(default)]
    pub profile_max_heights: BTreeMap<(String, String), i64>,
    /// Decode limits this client learned itself and now applies. Reported by
    /// the client so the server's reasons can name the client's own words for
    /// a demotion the client asked for.
    #[serde(default)]
    pub learned_limits: Vec<caps::LearnedLimit>,
}

impl DeviceProfile {
    fn allows_container(&self, container: &Option<String>) -> bool {
        match container {
            Some(c) => self.containers.iter().any(|x| x.eq_ignore_ascii_case(c)),
            None => false,
        }
    }
    fn allows_video(&self, codec: &Option<String>) -> bool {
        match codec {
            Some(c) => self.video_codecs.iter().any(|x| x.eq_ignore_ascii_case(c)),
            None => false,
        }
    }
    fn allows_audio(&self, codec: &str) -> bool {
        self.audio_codecs
            .iter()
            .any(|x| x.eq_ignore_ascii_case(codec))
    }

    fn allows_audio_stream(&self, stream: &crate::domain::AudioStream) -> bool {
        let codec = stream.codec.to_ascii_lowercase();
        // Sink claims decide what the server encodes and what may be passed
        // through. A client that decodes the codec mixes and resamples for its
        // own route, as every direct play always has, and a codec the claim
        // does not mention keeps the codec-list answer.
        let channels = stream
            .channels
            .and_then(|channels| u8::try_from(channels).ok())
            .filter(|channels| *channels > 0)
            .unwrap_or(2);
        let Some(sink) = self.audio_sink_claims.get(&codec) else {
            return self.allows_audio(&stream.codec);
        };
        // A sink that lists no rates is the flat `achannels` ceiling: no
        // decoder or route evidence beyond a channel count.
        if sink.sample_rates_hz.is_empty() {
            return self.allows_audio(&stream.codec) && channels <= sink.max_channels;
        }
        if self.claimed_audio_decoders.contains(&codec) {
            return true;
        }
        sink.passthrough
            && channels <= sink.max_channels
            && stream.sample_rate.is_some_and(|rate| {
                u32::try_from(rate)
                    .ok()
                    .is_some_and(|rate| sink.sample_rates_hz.contains(&rate))
            })
    }

    /// This client's height ceiling for a source in `codec`, narrowed by the
    /// source's own codec profile where the client reported a narrower one.
    ///
    /// A decoder's ceiling is not one number. A device can take 8-bit 4K HEVC
    /// and Main10 only at 1080p, and before the caps document could say so the
    /// web sent the minimum of its two rungs — capping every 4K 8-bit title at
    /// 1080p to protect the Main10 one, which is a transcode of a file the
    /// browser would have direct-played (PLAYBACK-CAPS-V2-PLAN §2, edge E5).
    ///
    /// The profile match is on the *source's* declared profile, normalized:
    /// ffprobe writes "Main 10", the caps document says "main10".
    ///
    /// When the source's profile is absent or is one the client did not
    /// enumerate, the answer is the **narrowest** ceiling that client gave for
    /// the codec — not the widest, and not the codec-wide one. `video_profile`
    /// is the raw ffprobe field and is nullable in both schemas; ffprobe omits
    /// it for plenty of HEVC streams, and every `MediaFile` the transcode path
    /// synthesises leaves it `None`. Reading the widest ceiling there would
    /// hand a client that said "hevc main 2160, hevc main10 1080" a 4K Main10
    /// title at 2160 on nothing but a missing column — the optimistic reading
    /// this module's header rules out, and a black screen rather than a
    /// transcode. A client that reported one ceiling per codec is unaffected:
    /// its minimum is its ceiling.
    fn codec_height_ceiling(&self, codec: &str, video_profile: Option<&str>) -> Option<i64> {
        let codec = codec.to_ascii_lowercase();
        let by_profile = video_profile.and_then(|profile| {
            let normalized: String = profile
                .chars()
                .filter(|character| !character.is_whitespace())
                .flat_map(char::to_lowercase)
                .collect();
            self.profile_max_heights
                .get(&(codec.clone(), normalized))
                .copied()
        });
        if let Some(height) = by_profile {
            return Some(height);
        }
        let narrowest = self
            .profile_max_heights
            .range((codec.clone(), String::new())..)
            .take_while(|((entry_codec, _), _)| *entry_codec == codec)
            .map(|(_, height)| *height)
            .min();
        narrowest.or_else(|| self.video_max_heights.get(&codec).copied())
    }

    /// The limit this client already applies to this exact media load, if it
    /// reported one.
    ///
    /// Matching is on the identity string and nothing else — no fuzzy
    /// codec-and-height fallback. The identity exists precisely because the
    /// old `codec@height` key poisoned every 4K HEVC title on the strength of
    /// one 90 Mb/s remux, and a lenient match here would reintroduce that
    /// through the back door.
    fn matching_learned_limit(&self, file: &MediaFile) -> Option<&caps::LearnedLimit> {
        if self.learned_limits.is_empty() {
            return None;
        }
        let identity = caps::decode_limit_identity(file);
        self.learned_limits
            .iter()
            .find(|limit| limit.identity == identity)
    }

    /// Drop every learned limit the client's own policy would no longer
    /// apply, as of `now_ms`.
    ///
    /// Called at the request boundary rather than inside [`decide`], which
    /// stays a pure function of its inputs — a decision that silently
    /// depended on the wall clock would be untestable and unreproducible from
    /// a log line.
    pub fn retain_applicable_learned_limits(&mut self, now_ms: i64) {
        self.learned_limits.retain(|limit| limit.applies_at(now_ms));
    }

    /// Does this client say it handles Dolby Vision for this source at all?
    ///
    /// A *claim*, and the wide reading is the right one for the two questions
    /// that ask it: whether the client's HDR bit may be excused
    /// (`dolby_vision_claimed` in `evaluate`) and whether Apple's
    /// `remux_dolby_vision` normalization applies. Both are about the client
    /// having a Dolby Vision pipeline at all, not about which profile comes out
    /// of it, and narrowing them would re-introduce the SDR tone-map that the
    /// comment on `dolby_vision_claimed` exists to record.
    ///
    /// What may actually be *delivered* is the narrower
    /// [`DeviceProfile::decodes_dolby_vision_as_copied`].
    fn allows_dolby_vision(&self, file: &MediaFile) -> bool {
        self.supports_dolby_vision || self.enumerates_dolby_vision_profile(file)
    }

    /// Has this client listed this source's own Dolby Vision profile by number?
    fn enumerates_dolby_vision_profile(&self, file: &MediaFile) -> bool {
        dolby_vision_profile(file)
            .is_some_and(|profile| self.dolby_vision_profiles.contains(&profile))
    }

    /// Can this client decode this source's Dolby Vision **as an unconverted
    /// copy would hand it over**?
    ///
    /// The blanket flag is a claim about the format, and it is honoured for
    /// every profile but the dual-layer ones. [`dolby_vision_is_dual_layer`]
    /// is the exception, and it has to be: a copy that does not convert hands
    /// the client the source's own configuration record, its RPUs and its
    /// type-63 enhancement layer, and no consumer decoder outside Blu-ray
    /// hardware takes that. A client saying "I do Dolby Vision" is not saying
    /// it takes dual-layer; every client that has ever said it meant
    /// single-layer, and reading it the wide way is the delivery that comes
    /// back `MEDIA_ERR_DECODE`.
    ///
    /// So dual-layer needs an *enumeration*. A client that lists 7 gets what it
    /// asked for; everything else is routed by `dv_handling` to the conversion
    /// where this node can build one, and to the compatible base layer or a
    /// re-encode where it cannot — all of which decode.
    pub fn decodes_dolby_vision_as_copied(&self, file: &MediaFile) -> bool {
        if dolby_vision_is_dual_layer(file) {
            return self.enumerates_dolby_vision_profile(file);
        }
        self.allows_dolby_vision(file)
    }
}

/// What the NODE can do — as distinct from what the client can take.
///
/// A grade is only honest when all three parties admit it: the source has to
/// carry it, the client has to present it, and the machine doing the encoding
/// has to have proved it can produce it. Until M4 the third party was not
/// consulted here at all — `/decision` promised HDR10 for a Profile 5 source
/// on the strength of the client's flag alone and the daemon refused it later,
/// so the badge said HDR10 over a tone-mapped SDR picture, which plays, so
/// nobody reports it.
///
/// Every field is boot-PROVED, never inferred from a version string or a
/// hardware claim, and `false` always means "not proved" rather than "cannot".
///
/// PLAYBACK-CAPS-V2-PLAN §4.4 also asks for a `main10_encoders` set. There is
/// no boot inventory of Main10 encoders to build one from — `detect_encoders`
/// greps for `h264_*` only — and the two probes below are stronger than an
/// inventory would be: each runs the real graph into the real encoder and
/// checks it exits clean, so "the encoder exists" is a claim they subsume.
/// The shortest frame the HDR10 rungs were measured at. Below it there is no
/// rung to land on: `hdr10_rung_fits` admits exactly 1080 and 2160, and
/// `output_size` never upscales, so a 720p source has nowhere to go.
pub const HDR10_MIN_MEASURED_HEIGHT: i64 = 1080;
/// The tallest, for the same reason.
pub const HDR10_MAX_MEASURED_HEIGHT: i64 = 2160;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderCaps {
    /// This ffmpeg has the `dovi_rpu` bitstream filter, so a Dolby Vision
    /// configuration can be removed on the way out (7.1+). A fact about the
    /// server, not the device: it decides whether a DV source a client cannot
    /// decode is a remux or a re-encode.
    pub dv_strippable: bool,
    /// This node proved the Dolby Vision RPU → Main10 PQ graph — `tonemapx`
    /// in passthrough mode with `apply_dovi=1`, into libx265 or QSV Main10.
    /// The Profile 5 route, and only that: the filter is
    /// Dolby-Vision-input-only and emits a broken picture at exit 0 on
    /// anything else.
    pub dolby_vision_p5_render: bool,
    /// This node proved the plain HDR10 passthrough encode — a 10-bit scale
    /// straight into Main10 PQ, no tone-map and no RPU. The route for every
    /// PQ source that is not a Profile 5: HDR10, HDR10+, and the HDR10 base
    /// of a Dolby Vision title whose enhancement layer has been stripped.
    ///
    /// Note it re-encodes, so HDR10+ dynamic metadata does not survive it —
    /// an HDR10+ source reaches the client as plain HDR10. The static grade is
    /// what is preserved.
    pub hdr10_passthrough: bool,
    /// The tallest rung this node can actually encode HDR10 at, or 0 for none.
    ///
    /// The renderer proofs above answer "can this graph run"; this answers
    /// "on a frame this size, on the encoder this node will choose". They are
    /// different questions and the second one is the one that was missing:
    /// the HDR10 rungs are measured at 1080p and 2160p only
    /// (`hdr10_rung_fits`), and a node with no hardware encoder resolves Auto
    /// to 720p — so without this term `/decision` promised HDR10 for the
    /// entire HDR library on a software-only node and the session delivered
    /// SDR every time. That is the badge-over-a-tone-mapped-picture failure
    /// this whole type exists to end, and it would have been widened from
    /// Profile 5 to every HDR title.
    pub hdr10_max_height: i64,
    /// This build can convert Dolby Vision Profile 7 RPUs to Profile 8.1 on
    /// the way through a copy (PLAYBACK-CAPS-V2-PLAN §4.8).
    ///
    /// A build fact rather than a node one — the conversion is plurx's own
    /// code, not an ffmpeg filter — but it belongs here with the other
    /// server-side answers, and it is a setting an operator can turn off
    /// without rebuilding.
    pub dolby_vision_convert: bool,
}

impl RenderCaps {
    /// A node that can strip Dolby Vision and has proved nothing else — every
    /// transcode of every HDR source tone-maps, which is what every node did
    /// before M4.
    pub const fn strip_only(dv_strippable: bool) -> Self {
        Self {
            dv_strippable,
            dolby_vision_p5_render: false,
            hdr10_passthrough: false,
            hdr10_max_height: 0,
            dolby_vision_convert: false,
        }
    }

    /// A node that has proved every renderer.
    pub const fn proven(dv_strippable: bool) -> Self {
        Self {
            dv_strippable,
            dolby_vision_p5_render: true,
            hdr10_passthrough: true,
            hdr10_max_height: HDR10_MAX_MEASURED_HEIGHT,
            dolby_vision_convert: true,
        }
    }
}

/// The dynamic range a source carries, as the grade negotiation sees it.
///
/// Coarser than `MediaFile.hdr` on purpose: what matters to an encode is which
/// transfer function has to come out the far end, and by what route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceGrade {
    /// Nothing to preserve.
    Sdr,
    /// PQ, reachable by an ordinary 10-bit copy of the decoded frames: HDR10,
    /// HDR10+, or a Dolby Vision base layer whose RPU is being discarded.
    Pq,
    /// PQ, but only the Dolby Vision RPU renderer can produce the picture — a
    /// Profile 5 source, whose base layer is not an HDR10 grade in any sense.
    PqViaRpu,
    /// HLG. No [`OutputGrade`] spells it, so it tone-maps; see
    /// [`target_grade`].
    Hlg,
    /// Dolby Vision with neither a compatible base to fall back to nor a
    /// renderer that can read its RPU. Nothing can put this on the wire as
    /// HDR, so it tone-maps whatever anyone claims.
    Unrenderable,
}

/// The client's own height ceiling for HEVC, narrowed by a codec-specific
/// entry where the client reported one. `None` is uncapped.
fn hevc_height_ceiling(profile: &DeviceProfile) -> Option<i64> {
    match (profile.max_height, profile.video_max_heights.get("hevc")) {
        (Some(global), Some(codec)) => Some(global.min(*codec)),
        (Some(global), None) => Some(global),
        (None, Some(codec)) => Some(*codec),
        (None, None) => None,
    }
}

fn source_grade(file: &MediaFile) -> SourceGrade {
    match file.hdr.as_deref() {
        Some("dolby_vision") => {
            if has_compatible_dv_base(file) {
                if file
                    .hdr_format
                    .as_deref()
                    .is_some_and(|label| label.contains("HLG-compatible"))
                {
                    SourceGrade::Hlg
                } else {
                    SourceGrade::Pq
                }
            } else if dolby_vision_profile(file) == Some(5) {
                SourceGrade::PqViaRpu
            } else {
                SourceGrade::Unrenderable
            }
        }
        Some("hdr10" | "hdr10plus") => SourceGrade::Pq,
        Some("hlg") => SourceGrade::Hlg,
        // An SDR source, or an HDR flavour nothing downstream distinguishes.
        _ => SourceGrade::Sdr,
    }
}

/// The renderer a source's HDR needs, when it has one this server can walk.
///
/// Two routes, and confusing them is a broken picture rather than an error.
/// The Dolby Vision graph applies an RPU and is input-only: handed ordinary
/// PQ frames it emits crushed shadows and clipping at exit 0. The plain route
/// touches no pixel values and would render a Profile 5 base layer — which is
/// not an HDR10 grade in any sense — as garbage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrRoute {
    /// Ordinary PQ frames: scale in 10-bit, encode Main10, signal the colour
    /// on the encoder. HDR10, HDR10+, and a Dolby Vision base layer whose
    /// enhancement layer is being discarded.
    Passthrough,
    /// The Dolby Vision RPU renderer — a Profile 5 source, which carries its
    /// grade in metadata no other filter reads.
    DolbyVisionRpu,
}

/// Which renderer this source's HDR needs, or `None` when there is no HDR
/// rung for it at all: an SDR source, HLG (no grade spells it), or a Dolby
/// Vision profile with neither a compatible base nor a readable RPU.
///
/// The daemon reads this to choose a pipeline; [`target_grade`] reads the same
/// classification to choose a grade. One function, so the badge and the bytes
/// cannot disagree about which renderer ran.
pub fn hdr_route(file: &MediaFile) -> Option<HdrRoute> {
    match source_grade(file) {
        SourceGrade::Pq => Some(HdrRoute::Passthrough),
        SourceGrade::PqViaRpu => Some(HdrRoute::DolbyVisionRpu),
        SourceGrade::Sdr | SourceGrade::Hlg | SourceGrade::Unrenderable => None,
    }
}

/// The grade a re-encode should target, and the reason it landed there.
///
/// Replaces `reencode_grade`, which asked the question only inside the Dolby
/// Vision branch — so a plain HDR10 title that transcoded for any other reason
/// (a height cap, a bitrate cap, burned subtitles, the quality menu) was
/// tone-mapped to SDR with nothing anywhere saying why. That is the single
/// largest cause of the "or lower" half of Paul's report
/// (PLAYBACK-CAPS-V2-PLAN §2, edge E3).
///
/// Three parties, and the highest grade all three admit wins:
///
/// 1. **The source** must carry PQ, by some route this server can walk.
/// 2. **The client** must present it — `supports_hdr` for HDR at all, and
///    `supports_hdr10_transcode` (`hdr10t=1`) for Main10 PQ specifically.
///    Both are absent-means-not-proven; the failure mode of guessing
///    optimistically is a PQ stream on an SDR display, which renders grey and
///    plays, so nobody reports it.
/// 3. **The node** must have proved the renderer the source's route needs.
///
/// The reason is a fixed string naming which of the three refused, because the
/// badge and the stats overlay show it and a demotion nobody can read is a
/// demotion nobody can fix. It is empty for an SDR source, where there was
/// never a grade to lose.
///
/// **HLG tone-maps, and that is a known gap rather than an oversight.**
/// [`OutputGrade`] has no HLG variant, so the only HDR rung available would
/// re-tag an HLG source as PQ — a grade *change* rather than a preservation,
/// which renders wrong on a display that believed the tag. Widening the grade
/// is its own piece of work.
pub fn target_grade(
    file: &MediaFile,
    profile: &DeviceProfile,
    node: &RenderCaps,
) -> (OutputGrade, &'static str) {
    let source = source_grade(file);
    match source {
        SourceGrade::Sdr => return (OutputGrade::Sdr, ""),
        SourceGrade::Hlg => {
            return (
                OutputGrade::Sdr,
                "tone-mapped to SDR: HLG has no HDR encode rung on this server",
            )
        }
        SourceGrade::Unrenderable => {
            return (
                OutputGrade::Sdr,
                "tone-mapped to SDR: this Dolby Vision profile has no compatible HDR base and \
                 no renderer that reads its RPU",
            )
        }
        SourceGrade::Pq | SourceGrade::PqViaRpu => {}
    }
    if !profile.supports_hdr {
        // `evaluate` has already pushed "HDR (…) presentation was not proven
        // by this client", naming the display bit that actually refused. A
        // second line here would put two entries in the badge for one refusal
        // and blame the Main10 decoder for a decision the display made.
        return (OutputGrade::Sdr, "");
    }
    if !profile.supports_hdr10_transcode {
        return (
            OutputGrade::Sdr,
            "tone-mapped to SDR: this client did not prove it decodes and presents HEVC Main10 PQ",
        );
    }
    // The rung emits HEVC Main10 and nothing else — `video_codec_for(Hdr10)`
    // is libx265 or hevc_qsv — so a client that cannot decode HEVC must not be
    // handed it. Nothing else in the negotiation checks this: `hdr10t=1` says
    // "I present Main10 PQ", and a device with an HDR panel and an H.264-only
    // decoder can honestly send it. The web player happens to derive the flag
    // from an HEVC probe; a third party against the documented query string
    // does not have to.
    if !profile.allows_video(&Some("hevc".to_owned())) {
        return (
            OutputGrade::Sdr,
            "tone-mapped to SDR: this client does not decode HEVC, which is the only codec the \
             HDR10 rung emits",
        );
    }
    // Geometry. The rungs are measured at 1080p and 2160p, and nothing
    // upscales into HDR, so a source shorter than the smallest rung has
    // nowhere to land — and neither does a client whose own ceiling is below
    // it. Auto may still resolve lower than this for bandwidth reasons at
    // session time; the session reports what it actually produced
    // (MEDIA-BADGES-PLAN §3.2). What this refuses is the case where no rung
    // was ever reachable.
    if file.height.unwrap_or(0) < HDR10_MIN_MEASURED_HEIGHT {
        return (
            OutputGrade::Sdr,
            "tone-mapped to SDR: the HDR10 rung is measured at 1080p and above, and this source \
             is smaller",
        );
    }
    if hevc_height_ceiling(profile).is_some_and(|ceiling| ceiling < HDR10_MIN_MEASURED_HEIGHT) {
        return (
            OutputGrade::Sdr,
            "tone-mapped to SDR: this client's height ceiling is below the smallest measured \
             HDR10 rung",
        );
    }
    if node.hdr10_max_height < HDR10_MIN_MEASURED_HEIGHT {
        return (
            OutputGrade::Sdr,
            "tone-mapped to SDR: this node cannot encode HDR10 at any height it would deliver",
        );
    }
    let node_proved = if source == SourceGrade::PqViaRpu {
        node.dolby_vision_p5_render
    } else {
        node.hdr10_passthrough
    };
    if !node_proved {
        return (
            OutputGrade::Sdr,
            "tone-mapped to SDR: this node's ffmpeg did not prove the HDR10 encode for this kind \
             of source",
        );
    }
    (
        OutputGrade::Hdr10,
        "HDR kept: this client presents Main10 PQ and this node proved the HDR10 encode",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackMethod {
    DirectPlay,
    Remux,
    Transcode,
}

#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub method: PlaybackMethod,
    /// Human-readable reasons the file isn't direct-playable (empty ⇒ direct).
    pub reasons: Vec<String>,
    /// For remux/transcode: re-encode audio to AAC because the source audio
    /// codec isn't in the profile.
    pub transcode_audio: bool,
    /// The independently resolved audio output. This is additive to
    /// `transcode_audio`, whose legacy meaning remains "the source audio was
    /// incompatible" until every create path consumes this richer contract.
    pub delivered_audio: AudioDelivery,
    /// Preserve Dolby Vision configuration + RPU metadata on a copy/remux.
    /// False means the client cannot take this source profile and the remux
    /// must expose a compatible HDR base instead.
    pub preserve_dolby_vision: bool,
    /// Rewrite this source's Profile 7 RPUs to Profile 8.1 on the way through.
    ///
    /// Only ever true beside `preserve_dolby_vision`, and the pair is not
    /// redundant: preserving alone means "hand the RPUs over untouched", which
    /// is what a client that decodes Profile 7 gets. Converting means "hand
    /// over rewritten ones", which is what a client that decodes only 8 gets
    /// instead of the HDR10 base it used to be given.
    ///
    /// `#[serde(default)]` because clients built before this field existed
    /// send decisions back on the create path, and a missing key there means
    /// "no conversion" — the answer every one of them was already getting.
    #[serde(default)]
    pub convert_dolby_vision: bool,
    /// Target container for remux/transcode delivery.
    pub container: &'static str,
    /// The dynamic range this plan actually delivers — see
    /// [`delivered_dynamic_range`]. Reported, not decided: it is a readout of
    /// the verdict above, so the play menu's badge can say "DV P7 → HDR10"
    /// instead of claiming the source's grade for a stripped remux. A session
    /// created later overrides it (MEDIA-BADGES-PLAN §3.2).
    pub delivered_dynamic_range: &'static str,
    /// The Dolby Vision profile the delivered bytes carry, when they carry
    /// any — see [`delivered_dolby_vision_profile`].
    ///
    /// The one thing `delivered_dynamic_range` cannot say: a preserved
    /// Profile 7 and a Profile 7 converted to 8.1 are both `"dolby_vision"`,
    /// and only this separates them. Absent on the wire when the delivery
    /// carries no Dolby Vision at all, which is not the same as unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_dolby_vision_profile: Option<u8>,
    /// The grade a re-encode would target. Meaningless for direct play and
    /// remux, where nothing is encoded — [`OutputGrade::Sdr`] there, which is
    /// what every method answered before M5.
    ///
    /// Not serialised: `delivered_dynamic_range` is the wire answer, and two
    /// wire fields saying the same thing are two fields that can disagree.
    /// This one exists so the *server* can carry the choice from [`decide`]
    /// into the session it builds.
    #[serde(skip)]
    pub transcode_grade: OutputGrade,
}

/// The per-dimension compatibility verdicts, shared by [`decide`] and
/// [`decide_forced`] so the two never drift.
struct Checks {
    video_ok: bool,
    height_ok: bool,
    bitrate_ok: bool,
    hdr_ok: bool,
    container_ok: bool,
    packaging_normalization: bool,
    audio_ok: bool,
}

impl Checks {
    /// True when only the container and/or audio codec differ — copy-video
    /// remux territory (nothing needs a video re-encode).
    fn needs_transcode(&self) -> bool {
        !self.video_ok || !self.height_ok || !self.bitrate_ok || !self.hdr_ok
    }
}

/// Run every compatibility check, collecting human reasons for the ones that
/// fail (empty ⇒ direct-playable, profile-wise).
fn evaluate(file: &MediaFile, profile: &DeviceProfile) -> (Checks, Vec<String>) {
    let mut reasons = Vec::new();

    let video_ok = file.video_codec.is_none() || profile.allows_video(&file.video_codec);
    if !video_ok {
        reasons.push(format!(
            "video codec {} unsupported",
            file.video_codec.as_deref().unwrap_or("unknown")
        ));
    }

    let codec_height = file
        .video_codec
        .as_ref()
        .and_then(|codec| profile.codec_height_ceiling(codec, file.video_profile.as_deref()));
    let height_ceiling = match (profile.max_height, codec_height) {
        (Some(global), Some(codec)) => Some(global.min(codec)),
        (global, codec) => global.or(codec),
    };
    let height_ok = match (height_ceiling, file.height) {
        (Some(max), Some(h)) => h <= max,
        _ => true,
    };
    if !height_ok {
        reasons.push("resolution above device maximum".to_owned());
    }

    let bitrate_ok = match (profile.max_bitrate, file.bitrate) {
        (Some(max), Some(b)) => b <= max,
        _ => true,
    };
    if !bitrate_ok {
        reasons.push("bitrate above device maximum".to_owned());
    }

    // A client that claims THIS file's Dolby Vision has already answered the
    // dynamic-range question for it, and answered it more specifically than
    // the generic HDR bit does: DV is only claimed off a display that shows
    // DV (`CapsPolicy.dolbyVisionCaps` refuses to send `dv`/`dvprofile`
    // otherwise), so a DV claim implies an HDR panel. The `hdr` bit can be 0
    // on such a client anyway, because it is computed from a different and
    // narrower source — Android reads it off the RAW MediaCodec registry
    // (`video/hevc` present), while DV is probed through Media3's decoder
    // selector, the registry gap PR #370 already worked around for the DV
    // side. Letting `hdr=0` veto here forced an SDR transcode of a file the
    // box had just said it decodes natively, with `DvHandling::None` agreeing
    // nothing needed re-encoding.
    //
    // Deliberately narrow: only a DV *source* whose exact profile this client
    // negotiated is excused. Plain HDR10/HLG still needs `supports_hdr`, and
    // a client that never claimed DV (or claimed other profiles) still
    // tone-maps, so an SDR-only client cannot be handed HDR by this.
    let dolby_vision_claimed = is_dolby_vision(file) && profile.allows_dolby_vision(file);
    let hdr_ok = file.hdr.is_none() || profile.supports_hdr || dolby_vision_claimed;
    if !hdr_ok {
        reasons.push(format!(
            "HDR ({}) presentation was not proven by this client; tone-mapping to SDR",
            file.hdr.as_deref().unwrap_or("hdr")
        ));
    }

    let dolby_vision_needs_remux = profile.remux_dolby_vision && dolby_vision_claimed;
    let packaging_normalization = progressive_hevc_requires_normalization(file, profile);
    let container_ok = profile.allows_container(&file.container)
        && !dolby_vision_needs_remux
        && !packaging_normalization;
    if dolby_vision_needs_remux {
        reasons.push("Dolby Vision normalized through copy-video HLS for this device".to_owned());
    } else if !container_ok && !packaging_normalization {
        reasons.push(format!(
            "container {} not browser-native",
            file.container.as_deref().unwrap_or("unknown")
        ));
    }

    // Audio is judged on the default track (else the first).
    let audio_stream = file
        .audio_streams
        .iter()
        .find(|a| a.default)
        .or_else(|| file.audio_streams.first());
    let audio_ok = match audio_stream {
        Some(stream) => profile.allows_audio_stream(stream),
        None => true, // no audio track — nothing to reject
    };
    if !audio_ok {
        let codec = audio_stream
            .map(|stream| stream.codec.as_str())
            .unwrap_or("unknown");
        reasons.push(if profile.allows_audio(codec) {
            format!("audio layout for {codec} exceeds the current sink")
        } else {
            // Keep the established reason byte-for-byte for existing clients
            // and tests; a sink-specific refusal earns the new wording above.
            format!("audio codec {codec} unsupported")
        });
    }

    (
        Checks {
            video_ok,
            height_ok,
            bitrate_ok,
            hdr_ok,
            container_ok,
            packaging_normalization,
            audio_ok,
        },
        reasons,
    )
}

pub fn resolve_audio_for_method(
    file: &MediaFile,
    profile: &DeviceProfile,
    method: PlaybackMethod,
) -> AudioDelivery {
    resolve_audio(
        file.audio_streams
            .iter()
            .find(|stream| stream.default)
            .or_else(|| file.audio_streams.first()),
        profile,
        if method == PlaybackMethod::Transcode {
            AudioRoute::RollingHls
        } else {
            AudioRoute::Progressive
        },
        file.audio_offset_ms,
    )
}

/// Whether an otherwise supported progressive HEVC source needs a copy-video
/// delivery because the client explicitly constrained ISO-BMFF sample entries.
pub fn progressive_hevc_requires_normalization(file: &MediaFile, profile: &DeviceProfile) -> bool {
    let hevc = file
        .video_codec
        .as_deref()
        .is_some_and(|codec| matches!(codec.to_ascii_lowercase().as_str(), "hevc" | "h265"));
    let iso_bmff = file.container.as_deref().is_some_and(|container| {
        matches!(
            container.to_ascii_lowercase().as_str(),
            "mp4" | "m4v" | "mov"
        )
    });
    let Some(admitted) = profile.progressive_hevc_sample_entries.as_ref() else {
        return false;
    };
    hevc && iso_bmff
        && file
            .video_codec_tag
            .as_ref()
            .is_none_or(|tag| !admitted.iter().any(|entry| entry == tag))
}

fn progressive_hevc_normalization_reason(file: &MediaFile) -> String {
    match file.video_codec_tag.as_deref() {
        Some(tag) => format!(
            "HEVC sample entry {tag} not admitted for progressive playback; normalizing through \
             copy-video delivery"
        ),
        None => "HEVC sample entry unknown; normalizing for the reported packaging constraint"
            .to_owned(),
    }
}

/// Is this source Dolby Vision — i.e. does it carry a DV configuration a
/// player either understands or chokes on?
pub fn is_dolby_vision(file: &MediaFile) -> bool {
    file.hdr.as_deref() == Some("dolby_vision")
}

/// Would drawing a subtitle into the picture take dynamic range away from the
/// viewer?
///
/// One question, one function, one answer — for `/decision`, for session
/// creation, and for the M6 preparation path, which between them used to hold
/// three different opinions:
///
/// - `/decision` exempted **every** transcode, on the reasoning that a
///   re-encode can tone-map on the way. That was true before the HDR10 rung
///   existed. Since M4 a transcode's delivered grade is negotiated, so an HDR
///   title transcoding for a height cap really is putting HDR10 on the wire,
///   and burning into it really does drop the grade.
/// - Session creation keyed on the **source** flag and demanded an
///   acknowledgement the web client never sent and Apple computed from the
///   wrong range — so it refused sessions that were already tone-mapped.
/// - The preparation path ran no guard at all.
///
/// `base_delivered_range` must be the grade of the plan **without** the burn.
/// Never the grade of the resulting burn session: that session is SDR
/// *because of* the burn, so judging the burn by it always permits the burn.
///
/// | Case | base range (no burn) | Verdict |
/// |---|---|---|
/// | HDR copy / remux / direct | `dolby_vision` · `hdr10` · `hlg` | refuse |
/// | HDR source, transcode negotiated to HDR10 | `hdr10` | refuse — the burn would drop the grade |
/// | HDR source, transcode already tone-mapped | `sdr` | allow |
/// | SDR source, any method | `sdr` | allow |
#[must_use]
pub fn burn_would_discard_hdr(base_delivered_range: &str, requires_burn: bool) -> bool {
    requires_burn && matches!(base_delivered_range, "dolby_vision" | "hdr10" | "hlg")
}

/// The dynamic range of the bytes a delivery actually puts on the wire —
/// what the viewer is *getting*, as opposed to what the file carries.
///
/// A reporter, never a decider: it reads a verdict that has already been
/// made and names its dynamic-range outcome, so the badge in the play menu
/// can stop claiming the source's grade for a tone-mapped transcode of it
/// (MEDIA-BADGES-PLAN §2.1). The vocabulary is `MediaFile.hdr`'s plus
/// `"sdr"`, so a client compares source against delivered with string
/// equality.
///
/// The three deliveries are total:
///
/// - **Transcode** answers whatever grade the session's encoder actually
///   produces, which `grade` names. It was an unconditional `"sdr"` until
///   M5, and for [`OutputGrade::Sdr`] it still is: those rungs are H.264
///   8-bit (`transcode/encoder.rs` — libx264 `-profile:v high`,
///   h264_nvenc/qsv/vaapi/videotoolbox) and every filter chain ends in
///   `yuv420p`/`nv12`, so an HDR source has been tone-mapped by the time it
///   reaches the encoder. The `ToneMap::None` escape hatch still lands on
///   8-bit `format=yuv420p` (`transcode/mod.rs`), so "sdr" stays honest
///   there — washed out at worst, never a wider grade than claimed.
///   [`OutputGrade::Hdr10`] is the HEVC Main10 PQ rung
///   (`Pipeline::DoviPassthrough`), and reporting `"sdr"` for it would put an
///   SDR badge on a stream a viewer is watching in HDR.
/// - **Direct play / remux of a non-DV source** delivers the source's grade:
///   the video is copied byte-for-byte.
/// - **Direct play / remux of a DV source** either preserves the Dolby
///   Vision configuration (`preserve_dolby_vision`) or strips it to the base
///   layer. Under [`decide`] a strip is only reachable through
///   `has_compatible_dv_base`, so the remux carries an "(HDR10-compatible)"
///   or "(HLG-compatible)" marker to read the base's grade off.
///
/// [`decide_forced`] with [`Force::Original`] is the one arm that reaches a
/// stripping remux outside that guarantee: it means "no video re-encode", so
/// it copies a DV source the client cannot decode whatever `dv_handling`
/// said. With no compatibility marker at all — a Profile 5 source, whose base
/// layer is not an HDR10 grade in any sense — this still answers `"hdr10"`,
/// and that over-claims. The over-claim is deliberately left rather than
/// guessed at: narrowing it needs `dv_strippable`, a fact about the server
/// rather than about the delivery this signature describes, and the stream in
/// question is one no client that asked for it can play — the error path
/// rescues it into a transcode, whose session then reports `"sdr"` for what
/// the viewer actually got.
pub fn delivered_dynamic_range(
    file: &MediaFile,
    method: PlaybackMethod,
    preserve_dolby_vision: bool,
    grade: OutputGrade,
) -> &'static str {
    if method == PlaybackMethod::Transcode {
        return grade.delivered_dynamic_range();
    }
    match file.hdr.as_deref() {
        Some("dolby_vision") if preserve_dolby_vision => "dolby_vision",
        Some("dolby_vision") => {
            if file
                .hdr_format
                .as_deref()
                .is_some_and(|label| label.contains("HLG-compatible"))
            {
                "hlg"
            } else {
                "hdr10"
            }
        }
        Some("hdr10") => "hdr10",
        Some("hlg") => "hlg",
        // An SDR source, an HDR flavour nothing downstream distinguishes
        // (HDR10+ probes as "hdr10"), or a file nobody ever probed.
        _ => "sdr",
    }
}

/// The Dolby Vision profile the delivered *bytes* carry, when they carry any.
///
/// A companion to [`delivered_dynamic_range`], and the field that separates
/// two deliveries it cannot: a Profile 7 title preserved for a device that
/// enumerates 7, and the same title converted to 8.1 for a device that does
/// not, both answer `"dolby_vision"`. The grade is the same — that is the
/// point of the conversion — but the profile on screen is not the profile on
/// disk, and the badge that says `DV P7` for both is telling one of them
/// something untrue about its own file (MEDIA-BADGES-PLAN §2.3).
///
/// `None` means **there is no profile to name**, which covers two cases a
/// reader must not conflate: a delivery that carries no Dolby Vision at all
/// (a transcode, a strip, a source that never had any), and a Dolby Vision
/// delivery whose source row does not say which profile — a pre-M2 row whose
/// label is the bare string "Dolby Vision", which [`dolby_vision_profile`]
/// documents as producible. A client must therefore read absence as "no
/// answer", never as "this stream is not Dolby Vision":
/// [`delivered_dynamic_range`] beside it is the field that answers that.
///
/// Narrowing the second case would mean claiming a profile from a row that
/// does not state one, which is the class of guess this milestone exists to
/// stop — the conversion is the only delivery whose profile is known without
/// asking the row, and it is the one case answered outright below.
pub fn delivered_dolby_vision_profile(
    file: &MediaFile,
    method: PlaybackMethod,
    preserve_dolby_vision: bool,
    convert_dolby_vision: bool,
) -> Option<u8> {
    // Every transcode re-encodes the picture, and no plurx encode rung
    // produces Dolby Vision.
    if method == PlaybackMethod::Transcode || !preserve_dolby_vision {
        return None;
    }
    if !is_dolby_vision(file) {
        return None;
    }
    if convert_dolby_vision {
        // Not read back from the file: the conversion's output is 8.1 by
        // construction, and the source row says 7. Reading the row here would
        // report the profile the conversion exists to replace.
        return Some(8);
    }
    dolby_vision_profile(file)
}

/// Profile number from the Dolby Vision configuration record, or from the
/// scan's rich label for a row the M2 backfill has not reached.
///
/// Unknown is deliberately `None`: claiming every DV profile from a generic
/// HDR bit is the bug this profile-aware path replaces.
///
/// The column is asked first and the label is the fallback, not the reverse.
/// Reading a number back out of a display string is exactly as fragile as it
/// sounds — a record with no `dv_profile`, or a detection that only matched
/// the codec tag, produces the bare string "Dolby Vision" and no profile at
/// all. The fallback exists for rows written before the columns did, and can
/// be deleted once no such row remains.
pub fn dolby_vision_profile(file: &MediaFile) -> Option<u8> {
    // `and_then`, not an early return: a column outside a profile number's
    // range is a fact the record got wrong, and the label is still the better
    // answer for that row rather than nothing at all.
    if let Some(profile) = file.dolby_vision.profile.and_then(|p| u8::try_from(p).ok()) {
        return Some(profile);
    }
    let label = file.hdr_format.as_deref()?;
    let after = label.to_ascii_lowercase();
    let after = after.split("profile").nth(1)?.trim_start();
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Is this source **dual-layer** — a base layer plus a type-63 enhancement
/// layer, which no consumer decoder outside Blu-ray hardware takes?
///
/// The dual-layer profiles are 7 (over an HDR10 or SDR base) and 4 (over an
/// HLG base). Profile 4 is effectively extinct and is named here anyway,
/// because it is exactly as undecodable and there is no reason for the rule to
/// have a hole shaped like it.
///
/// The profile is asked first and answers on its own, in **both** directions.
/// A record naming a single-layer profile is single-layer even if its
/// `el_present_flag` says otherwise — muxers write that flag over streams that
/// have no enhancement layer, and reading it the other way would strip Dolby
/// Vision out of a Profile 5 delivery, which has no compatible base to fall
/// back to and would tone-map to SDR. `el_present` is consulted only where
/// there is no profile to ask, which is the one case where it is the only
/// witness there is.
///
/// [`dolby_vision_profile`]'s label fallback is deliberate: a row scanned
/// before the columns existed answers "Profile 7" from its prose, and that is
/// the conservative answer — such a row also fails
/// [`file_can_convert_to_p81`], so it can neither convert nor be preserved,
/// and the base layer it falls back to is what it has always been served.
///
/// Three places ask this and must agree, or a stream describes itself honestly
/// and decodes nowhere: which clients may be handed an unconverted copy
/// ([`DeviceProfile::decodes_dolby_vision_as_copied`]), what a build that
/// enumerated nothing may be trusted with (`legacy_trusted_review` in the
/// daemon), and what a copy path with no RPU rewrite in it may serve
/// (`served_copy_options`).
pub fn dolby_vision_is_dual_layer(file: &MediaFile) -> bool {
    if !is_dolby_vision(file) {
        return false;
    }
    match dolby_vision_profile(file) {
        Some(4 | 7) => true,
        Some(_) => false,
        None => file.dolby_vision.el_present == Some(true),
    }
}

/// Does this Dolby Vision source have a base layer a non-DV client can watch?
///
/// The compatibility id says what that client sees: 1 and 6 are HDR10, 4 is
/// HLG, 2 is SDR and 0 is none. The three HDR ids are a base worth stripping
/// to; SDR and none are not — which is exactly what the label's
/// "(HDR10-compatible)" and "(HLG-compatible)" markers were derived from, so
/// the column and the fallback answer the same question from the same fact.
fn has_compatible_dv_base(file: &MediaFile) -> bool {
    if let Some(compat) = file.dolby_vision.bl_compat_id {
        return matches!(compat, 1 | 4 | 6);
    }
    file.hdr_format
        .as_deref()
        .is_some_and(|label| label.contains("HDR10-compatible") || label.contains("HLG-compatible"))
}

/// Does this source need the RPU-driven renderer rather than the ordinary
/// HDR10 base-layer route — i.e. is it the Profile 5 case?
///
/// This is the same question `plurxd`'s `TranscodeManager::needs_dovi_reshape`
/// answers, minus its refusal messages, and it exists here because the *grade*
/// choice has to agree with the *renderer* choice or the badge lies. A source
/// this returns false for is one the daemon renders with zscale or a vendor
/// graph — neither of which reads an RPU, and neither of which the HDR10
/// passthrough rung is implemented for. The two are held together by
/// `plurxd`'s `the_grade_predicate_and_the_renderer_predicate_agree`.
pub fn dolby_vision_needs_rpu_render(file: &MediaFile) -> bool {
    is_dolby_vision(file) && !has_compatible_dv_base(file) && dolby_vision_profile(file) == Some(5)
}

/// What a Dolby Vision source needs doing about it for THIS client.
///
/// The failure this exists to prevent: a DV remux was handed to Chrome
/// because the base layer is HDR10-compatible and the client claimed HDR.
/// Chrome refuses the track outright (`MEDIA_ERR_DECODE`) — the DV
/// configuration is in the sample entry whatever the base layer looks like —
/// so the player's error path rescued it into a transcode at the Auto rung,
/// and a 4K disc remux played at 1080p with nothing saying why. Safari, which
/// decodes DV, played the same file perfectly: that difference is what named
/// the cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DvHandling {
    /// Not DV, or the client decodes it: nothing to do.
    None,
    /// The client can't take DV, but the server can strip it to its HDR10
    /// base — which only ffmpeg can do, so direct play (the raw file) is out
    /// and a remux is the minimum.
    Strip,
    /// The client can't take Profile 7 but does take Profile 8, the source's
    /// base layer is HDR10, and this build can convert. The RPUs are rewritten
    /// on the way through and the client gets Dolby Vision rather than the
    /// HDR10 base it would otherwise be handed.
    ///
    /// Checked before [`DvHandling::Strip`], because every source that
    /// converts also strips and stripping is the strictly worse answer: it
    /// throws away metadata the client could have used.
    Convert,
    /// The client can't take DV and the server can't remove it. The only
    /// stream this client will play is a re-encoded one — at the carried
    /// [`OutputGrade`].
    ///
    /// The grade rides *inside* the variant rather than beside the enum
    /// because it only means anything here. A `DvHandling` with a grade field
    /// would let `None`/`Strip` carry one too, and then some future reader has
    /// to know which combinations are real: `Strip` is a copy, so an HDR10
    /// grade on it would describe an encode that never happens. The payload
    /// spells the invariant instead of documenting it, and every existing
    /// `== DvHandling::None` comparison keeps working unchanged.
    Reencode(OutputGrade),
}

/// Whether this client should be served a Profile 7 title converted to
/// Profile 8.1 rather than stripped to its HDR10 base.
///
/// The three conditions, and each one is doing work:
///
/// - **The source is Profile 7 with an HDR10 base.** A P7 stream is a
///   base layer plus an enhancement layer no consumer decoder takes — no
///   shipping player outside Blu-ray hardware has ever decoded dual-layer.
///   Profile 8.1 is that same base layer with the same RPUs, minus the
///   references to the layer nobody could use. The base has to be HDR10
///   specifically: 8.1 *means* an HDR10 base, and an HLG base would be 8.4 —
///   a different conversion, with a target plurx has no rung for.
/// - **The client takes 8 but not 7.** A client that enumerates 7 gets the
///   stream untouched; converting for it would discard an enhancement layer
///   it asked for. A client that takes neither gets the ordinary strip.
/// - **This build can do it**, which an operator can turn off.
///
/// The value of the conversion is exactly the gap between those two client
/// answers: today a Profile 7 title reaches Safari and Apple TV as HDR10,
/// because the only thing plurx could do with an undecodable profile was
/// remove the Dolby Vision entirely. Converted, it reaches them as Dolby
/// Vision.
pub fn dolby_vision_converts_to_p81(
    file: &MediaFile,
    profile: &DeviceProfile,
    node: &RenderCaps,
) -> bool {
    node.dolby_vision_convert
        && file_can_convert_to_p81(file)
        && profile.dolby_vision_profiles.contains(&8)
        && !profile.dolby_vision_profiles.contains(&7)
}

/// The half of [`dolby_vision_converts_to_p81`] that is about the **file**
/// alone: Profile 7 over an HDR10 base, described by columns rather than
/// prose.
///
/// Split out because the fragment index needs it without a client. Which
/// clients convert is a per-session question and an index is per-file, so the
/// indexer asks "could any client want this pipeline for this file", and the
/// decider asks "does this client want it now". Two questions, one predicate
/// for the part they share — a file the indexer skipped and the decider then
/// routed to a conversion is a session looking up an index nothing built.
///
/// **The columns are required here, and the label fallback is not enough.**
/// Everywhere else in this module a row scanned before M2 can answer from its
/// `hdr_format` prose, and that is right: the question is what to deliver, and
/// a label that says "Profile 7 (HDR10-compatible)" answers it. The conversion
/// asks something the prose cannot answer. Its output must declare a Dolby
/// Vision configuration record that describes the converted stream, plurx
/// writes that record itself (ffmpeg copies one from the input container, and
/// a converting copy's input container is the Profile 7 source), and building
/// one needs the *level* and the *compatibility id* as numbers. A label-only row would be routed to
/// a conversion whose index could never be built — a permanent
/// `vod_index_pending`, and a fall through to live recovery on every play.
///
/// So the rule is: no columns, no conversion. Such a row keeps the strip it
/// has always had until a rescan fills them in, which is a working delivery
/// rather than a broken one.
pub fn file_can_convert_to_p81(file: &MediaFile) -> bool {
    is_dolby_vision(file)
        && file.dolby_vision.profile == Some(7)
        && file
            .dolby_vision
            .bl_compat_id
            .is_some_and(|compat| matches!(compat, 1 | 6))
        // The range the record can actually hold, not merely "present".
        // `DolbyVisionRecord::new` caps the level at 0x3f because the field is
        // six bits, so a row whose scan produced anything larger would route
        // here, reach the converting index pass, and fail to have its record
        // built at all — a permanent `vod_index_pending` and a fall through to
        // live recovery on every play, which is the exact outcome the column
        // requirement above exists to prevent. Real levels are 1 to 13.
        && file
            .dolby_vision
            .level
            .is_some_and(|level| (1..=0x3f).contains(&level))
}

fn dv_handling(
    file: &MediaFile,
    profile: &DeviceProfile,
    node: &RenderCaps,
    target: OutputGrade,
) -> DvHandling {
    if !is_dolby_vision(file) || profile.decodes_dolby_vision_as_copied(file) {
        DvHandling::None
    } else if dolby_vision_converts_to_p81(file, profile, node) {
        DvHandling::Convert
    } else if node.dv_strippable && has_compatible_dv_base(file) {
        DvHandling::Strip
    } else {
        DvHandling::Reencode(target)
    }
}

/// Decide how to play `file` on a device described by `profile` — the automatic
/// ladder: direct play when everything matches, remux for a container/audio
/// mismatch (copy video, maybe re-encode audio), transcode only when the video
/// itself won't decode (codec/resolution/bitrate/HDR).
///
/// `node` is a set of facts about the SERVER, not the device: whether this
/// ffmpeg build can remove a Dolby Vision configuration on the way out, and
/// which HDR encodes it has proved. The first decides whether a DV source a
/// client cannot decode is a remux or a re-encode; the rest decide whether a
/// re-encode of any HDR source keeps its grade or tone-maps
/// ([`target_grade`]).
pub fn decide(file: &MediaFile, profile: &DeviceProfile, node: &RenderCaps) -> Decision {
    let (mut c, mut reasons) = evaluate(file, profile);
    // A converted stream carries Dolby Vision too, so it preserves. The two
    // flags are set together and read together: `preserve` decides whether the
    // copy's bitstream filter keeps the RPU NAL units, and `convert` decides
    // whether they are rewritten in the fragments it writes. Keeping them without
    // rewriting them would hand a Profile 7 stream to a decoder that asked for
    // 8; rewriting them without keeping them would rewrite nothing.
    let convert_dolby_vision = dolby_vision_converts_to_p81(file, profile, node);
    let preserve_dolby_vision = convert_dolby_vision
        || (is_dolby_vision(file) && profile.decodes_dolby_vision_as_copied(file));
    let (target, grade_reason) = target_grade(file, profile, node);
    // Whether the Dolby Vision branch below has already explained the grade in
    // its own words. It gets to speak first because its reason carries the
    // static-metadata caveat, which is specific to a re-encode of a source
    // whose HDR metadata lives in an RPU nobody else has.
    let mut grade_explained = false;
    // Whether the strip branch fired. Its reason is deferred rather than
    // pushed there, because a later demotion can turn the remux it describes
    // into a transcode — and "the compatible HDR base was kept untouched" on
    // a delivery that re-encoded it is exactly the class of lie this
    // milestone exists to stop telling.
    let mut stripped_dolby_vision = false;

    match dv_handling(file, profile, node, target) {
        DvHandling::None => {}
        DvHandling::Convert => {
            // The source cannot be handed over as-is. Explain the client's
            // compatibility here, not the eventual processing: session
            // admission may reconstruct FEL into a Profile 8.1 encode, and a
            // later video constraint may require a non-DV transcode instead.
            // The session's enhancement details describe what it selected.
            c.container_ok = false;
            reasons.push(
                "Dolby Vision Profile 7 is not supported directly by this device; \
                 Profile 8.1 is supported"
                    .to_owned(),
            );
        }
        DvHandling::Strip => {
            // Not a transcode: the base layer is kept untouched and only the
            // DV configuration goes. But it takes ffmpeg, so the raw file
            // cannot be handed over as-is.
            c.container_ok = false;
            stripped_dolby_vision = true;
        }
        DvHandling::Reencode(grade) => {
            c.video_ok = false;
            reasons.push(if has_compatible_dv_base(file) && !node.dv_strippable {
                "this Dolby Vision profile is unsupported by this device and this ffmpeg \
                 cannot expose its compatible HDR base (requires dovi_rpu in ffmpeg 7.1+)"
                    .to_owned()
            } else {
                "this Dolby Vision profile is unsupported by this device and has no \
                 compatible HDR base; transcoding"
                    .to_owned()
            });
            if grade == OutputGrade::Hdr10 {
                // Said out loud because the alternative reads as a bug. This
                // client is being handed a re-encode of a Dolby Vision title
                // and a badge that says HDR10, and the only thing separating
                // that from an over-claim is a capability bit it sent.
                //
                // The static-metadata caveat is in the reason string rather
                // than only in a code comment because it is the one thing a
                // viewer's display can notice: a true Profile 5 source carries
                // no MDCV/CLL SEI at all (Dolby's L6 metadata lives in the
                // RPU), so there is nothing to inherit and nothing honest to
                // synthesise. The output is correctly tagged PQ/BT.2020, which
                // is what selects a display's HDR mode; what it lacks is the
                // mastering-display hint some panels use for headroom.
                reasons.push(
                    if hdr_route(file) == Some(HdrRoute::DolbyVisionRpu) {
                        // A true Profile 5 source carries no MDCV/CLL SEI at
                        // all — Dolby's L6 metadata lives inside the RPU — so
                        // there is nothing to inherit and nothing honest to
                        // synthesise. The output is correctly tagged PQ/BT.2020,
                        // which is what selects a display's HDR mode; what it
                        // lacks is the mastering-display hint some panels use
                        // for headroom.
                        "re-encoding to HEVC Main10 PQ (HDR10) rather than tone-mapping, \
                         because this client reported it decodes and presents Main10 PQ; \
                         HDR10 static mastering metadata is not carried — a Profile 5 \
                         source has none to inherit"
                    } else {
                        // This source DOES carry mastering-display and
                        // content-light metadata, and whether it survives the
                        // re-encode depends on the graph: x265 forwards frame
                        // side data, a hardware download can drop it, and
                        // hevc_qsv does not carry it at all. Said plainly
                        // rather than repeating Profile 5's excuse, which is
                        // false here.
                        "re-encoding to HEVC Main10 PQ (HDR10) rather than tone-mapping, \
                         because this client reported it decodes and presents Main10 PQ; \
                         the source's HDR10 mastering metadata may not survive the \
                         re-encode"
                    }
                    .to_owned(),
                );
            }
            // Only an HDR10 outcome was explained above. A Dolby Vision
            // re-encode that tone-maps has said why the DEVICE cannot take the
            // source, which is a different question from why the GRADE was
            // lost — and on a node that never proved the renderer, the second
            // answer is the one an operator needs.
            grade_explained = grade == OutputGrade::Hdr10;
        }
    }

    // A limit this client learned the hard way, on this exact media load.
    //
    // Without this the browser applied it alone: one stuttery session routed
    // every matching title to a forced transcode for a month, and the server
    // never knew — so `/decision` promised a direct play the client had
    // already decided against, and every reason string the badge and the
    // admin session list showed described a delivery that never happened.
    //
    // The demotion is a transcode rather than a remux because the limit is a
    // statement about the DECODER: the client could not keep up with these
    // frames, so handing it the same frames in a different envelope changes
    // nothing.
    let learned_limit = profile.matching_learned_limit(file);
    if let Some(limit) = learned_limit {
        // What this demotion costs the picture, if it costs anything. The
        // grade is read BEFORE `video_ok` is cleared, because the question is
        // what the ordinary decision would have delivered — a transcode's
        // own grade is a different answer and would report "HDR10 → SDR" for
        // a rung that is still HDR10.
        let lost_range = match file.hdr.as_deref() {
            Some(range @ ("dolby_vision" | "hdr10" | "hlg"))
                if !c.needs_transcode() && target == OutputGrade::Sdr =>
            {
                Some(range)
            }
            _ => None,
        };
        c.video_ok = false;
        // The browser's own wording, verbatim. A server paraphrase would put
        // two different explanations of one decision in front of the same
        // viewer, in the same UI, and the reconciliation would be theirs to
        // do.
        reasons.push(limit.reason(lost_range));
    }

    if stripped_dolby_vision {
        reasons.push(if c.needs_transcode() {
            // The strip was planned and then overtaken. Say what actually
            // happens to the picture, not what the strip alone would have
            // done to it.
            "Dolby Vision metadata removed for this device; re-encoding from the \
             compatible HDR base"
                .to_owned()
        } else {
            "Dolby Vision metadata removed for this device; compatible HDR base kept".to_owned()
        });
    }

    // A manual A/V sync correction can only be applied by ffmpeg, so direct
    // play is off the table for that file — remux at minimum.
    let has_av_offset = file.audio_offset_ms != 0;
    if has_av_offset {
        reasons.push(format!(
            "audio-sync correction {:+} ms",
            file.audio_offset_ms
        ));
    }

    let method = if c.needs_transcode() {
        PlaybackMethod::Transcode
    } else if !c.container_ok || !c.audio_ok || has_av_offset {
        PlaybackMethod::Remux
    } else {
        PlaybackMethod::DirectPlay
    };
    if method == PlaybackMethod::Remux && c.packaging_normalization {
        reasons.push(progressive_hevc_normalization_reason(file));
    }

    // A verdict that did not end up a transcode encodes nothing, so it has no
    // grade to report. Leaving a stale Hdr10 here would put an HDR10 badge on
    // a remux whose bytes are the source's own.
    let transcode_grade = if method == PlaybackMethod::Transcode {
        target
    } else {
        OutputGrade::Sdr
    };
    // Every HDR source that transcodes now says what happened to its grade and
    // why. Before M4 only the Dolby Vision branch ever spoke, so a plain HDR10
    // title that transcoded for a height cap was tone-mapped in silence.
    if method == PlaybackMethod::Transcode && !grade_explained && !grade_reason.is_empty() {
        reasons.push(grade_reason.to_owned());
    }

    // A verdict that ended up a transcode re-encodes the picture, so there is
    // no copy pipe left to convert RPUs in and nothing to convert them for.
    let convert_dolby_vision = convert_dolby_vision && method != PlaybackMethod::Transcode;
    let audio = resolve_audio_for_method(file, profile, method);
    Decision {
        method,
        reasons,
        transcode_audio: !c.audio_ok,
        delivered_audio: audio,
        preserve_dolby_vision: preserve_dolby_vision && method != PlaybackMethod::Transcode,
        convert_dolby_vision,
        container: "mp4",
        delivered_dynamic_range: delivered_dynamic_range(
            file,
            method,
            preserve_dolby_vision && method != PlaybackMethod::Transcode,
            transcode_grade,
        ),
        delivered_dolby_vision_profile: delivered_dolby_vision_profile(
            file,
            method,
            preserve_dolby_vision && method != PlaybackMethod::Transcode,
            convert_dolby_vision,
        ),
        transcode_grade,
    }
}

/// Like [`decide`], but honoring a manual quality override from the player.
pub fn decide_forced(
    file: &MediaFile,
    profile: &DeviceProfile,
    force: Force,
    node: &RenderCaps,
) -> Decision {
    match force {
        Force::Auto => decide(file, profile, node),
        Force::Transcode => {
            let (_, mut reasons) = evaluate(file, profile);
            reasons.insert(0, "forced transcode (manual quality)".to_owned());
            // The quality menu's Transcode used to be pinned to SDR, on the
            // reasoning that widening it would be a grade change nobody asked
            // for on a control whose purpose is "make this smaller". M4 turns
            // that around: the control asks for a smaller *stream*, not a
            // worse *picture*, and the rung that keeps the picture is the one
            // every other transcode now gets. The negotiation still refuses
            // wherever any of the three parties does.
            let (grade, grade_reason) = target_grade(file, profile, node);
            if !grade_reason.is_empty() {
                reasons.push(grade_reason.to_owned());
            }
            let audio = resolve_audio_for_method(file, profile, PlaybackMethod::Transcode);
            Decision {
                method: PlaybackMethod::Transcode,
                reasons,
                transcode_audio: true,
                delivered_audio: audio,
                preserve_dolby_vision: false,
                // A forced transcode re-encodes the picture: there is no copy
                // pipe left for a conversion to sit inside.
                convert_dolby_vision: false,
                container: "mp4",
                delivered_dynamic_range: delivered_dynamic_range(
                    file,
                    PlaybackMethod::Transcode,
                    false,
                    grade,
                ),
                delivered_dolby_vision_profile: None,
                transcode_grade: grade,
            }
        }
        Force::Original => {
            // Never re-encode video: direct-play when the browser can take the
            // container + audio, else a copy-video remux. If the pick turns out
            // undecodable, the client's error path falls back to transcode.
            let (c, _) = evaluate(file, profile);
            let has_av_offset = file.audio_offset_ms != 0;
            // A DV source this client can't decode still may not be handed
            // over raw — Original means "no video re-encode", which a strip
            // remux honours (the base layer is untouched). When even that is
            // unavailable the remux is the client's error path to rescue, as
            // it always was.
            let dv = dv_handling(file, profile, node, OutputGrade::Sdr);
            let method = if c.container_ok && c.audio_ok && !has_av_offset && dv == DvHandling::None
            {
                PlaybackMethod::DirectPlay
            } else {
                PlaybackMethod::Remux
            };
            let mut reasons = vec!["forced original quality (no video transcode)".to_owned()];
            if c.packaging_normalization {
                reasons.push(progressive_hevc_normalization_reason(file));
            }
            if dv == DvHandling::Strip {
                reasons.push(
                    "Dolby Vision metadata removed for this device; compatible HDR base kept"
                        .to_owned(),
                );
            }
            if dv == DvHandling::Convert {
                // This decision's compatibility reason also survives session
                // admission; only the session knows whether FEL is rebuilt.
                reasons.push(
                    "Dolby Vision Profile 7 is not supported directly by this device; \
                     Profile 8.1 is supported"
                        .to_owned(),
                );
            }
            // Original means "no video re-encode", which the conversion
            // honours: it copies every picture byte and rewrites only the
            // per-frame metadata. So a forced-Original session on a client
            // that takes Profile 8 gets the conversion too, and `dv_handling`
            // above has already answered `Convert` for it.
            let convert_dolby_vision = dv == DvHandling::Convert;
            let preserve_dolby_vision = convert_dolby_vision
                || (is_dolby_vision(file) && profile.decodes_dolby_vision_as_copied(file));
            let audio = resolve_audio_for_method(file, profile, method);
            Decision {
                method,
                reasons,
                transcode_audio: !c.audio_ok,
                delivered_audio: audio,
                preserve_dolby_vision,
                convert_dolby_vision,
                container: "mp4",
                // Original never re-encodes video, so there is no grade: the
                // method is DirectPlay or Remux and this argument is inert.
                delivered_dynamic_range: delivered_dynamic_range(
                    file,
                    method,
                    preserve_dolby_vision,
                    OutputGrade::Sdr,
                ),
                delivered_dolby_vision_profile: delivered_dolby_vision_profile(
                    file,
                    method,
                    preserve_dolby_vision,
                    convert_dolby_vision,
                ),
                transcode_grade: OutputGrade::Sdr,
            }
        }
    }
}

/// Should a remux be delivered as HLS segments rather than progressively?
///
/// Chrome's progressive read-ahead is a hard ~2.2 seconds and no response
/// header can raise it (PERF-PLAN §4.3bis, measured). That is a fine margin
/// for ordinary web video and a fatal one for a remux, where any transient
/// supply gap longer than two seconds is a visible stall. That is true at
/// 10 Mb/s as well as 69 Mb/s: persisted production events included a
/// four-minute progressive stall on the former while average storage
/// headroom still measured 20x. Average bitrate and average storage speed
/// cannot predict a short path gap. The same bytes sent as HLS go through MSE
/// instead, where the buffer is the player's to set.
///
/// Video quality is identical either way — this chooses a *transport*, not a
/// ladder rung. Which is why the rule can afford to be generous: the cost of
/// a false positive is a playlist round-trip at startup, and the cost of a
/// false negative is the stall this exists to remove.
///
/// Returns the viewer-facing reason for the transport hint.
pub fn prefer_segmented(bitrate_bps: Option<i64>) -> Option<String> {
    // An unprobed file has no bitrate, and guessing one from resolution would
    // route half a library on an inference. Leave it on the path it has.
    let bitrate = bitrate_bps.filter(|b| *b > 0)? as f64;
    Some(format!(
        "{:.0} Mb/s remux — progressive fMP4 buffers about 2.2 s; HLS allows deeper read-ahead",
        bitrate / 1e6
    ))
}

#[cfg(test)]
mod tests;
