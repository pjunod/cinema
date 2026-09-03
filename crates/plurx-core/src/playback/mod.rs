//! Playback decision engine.
//!
//! Pure function of (file media details, device profile) → a verdict of
//! direct play, remux, or transcode (ARCHITECTURE §3). Device profiles are
//! data (`profiles.toml`), so the direct-play matrix is correctable without a
//! release (REQ-PLAY-4). Phase 1 serves DirectPlay and Remux; a Transcode
//! verdict is reported honestly and its serving lands in Phase 2.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::domain::MediaFile;

pub mod caps;
use crate::transcode::OutputGrade;
pub use caps::{DeviceCaps, LearnedLimit, LegacyCaps, Transfer, VideoCaps};

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
        max_height,
        video_max_heights: HashMap::new(),
        max_bitrate: None,
        supports_hdr,
        supports_dolby_vision,
        dolby_vision_profiles: Vec::new(),
        remux_dolby_vision: false,
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

    fn allows_dolby_vision(&self, file: &MediaFile) -> bool {
        self.supports_dolby_vision
            || dolby_vision_profile(file)
                .is_some_and(|profile| self.dolby_vision_profiles.contains(&profile))
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
    let container_ok = profile.allows_container(&file.container) && !dolby_vision_needs_remux;
    if dolby_vision_needs_remux {
        reasons.push("Dolby Vision normalized through copy-video HLS for this device".to_owned());
    } else if !container_ok {
        reasons.push(format!(
            "container {} not browser-native",
            file.container.as_deref().unwrap_or("unknown")
        ));
    }

    // Audio is judged on the default track (else the first).
    let audio_codec = file
        .audio_streams
        .iter()
        .find(|a| a.default)
        .or_else(|| file.audio_streams.first())
        .map(|a| a.codec.clone());
    let audio_ok = match &audio_codec {
        Some(c) => profile.allows_audio(c),
        None => true, // no audio track — nothing to reject
    };
    if !audio_ok {
        reasons.push(format!(
            "audio codec {} unsupported",
            audio_codec.as_deref().unwrap_or("unknown")
        ));
    }

    (
        Checks {
            video_ok,
            height_ok,
            bitrate_ok,
            hdr_ok,
            container_ok,
            audio_ok,
        },
        reasons,
    )
}

/// Is this source Dolby Vision — i.e. does it carry a DV configuration a
/// player either understands or chokes on?
pub fn is_dolby_vision(file: &MediaFile) -> bool {
    file.hdr.as_deref() == Some("dolby_vision")
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
    if !is_dolby_vision(file) || profile.allows_dolby_vision(file) {
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
    let preserve_dolby_vision =
        convert_dolby_vision || (is_dolby_vision(file) && profile.allows_dolby_vision(file));
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
            // Not a transcode and not a strip: the base layer and every
            // picture byte are copied, and only the per-frame metadata is
            // rewritten. It takes ffmpeg, so the file cannot be handed over
            // as-is — but nothing is re-encoded and nothing of the picture is
            // lost. What *is* lost is the enhancement layer, which is why the
            // reason says so rather than claiming a free upgrade: on a
            // full-enhancement-layer source that layer carried real residual
            // detail. The session's own reasons name which kind it was, once
            // the first RPU has been read and the answer is known.
            c.container_ok = false;
            reasons.push(
                "Dolby Vision Profile 7 converted to Profile 8.1 for this device; the \
                 HDR10-compatible base layer is copied untouched and the dual-layer \
                 enhancement layer, which no consumer decoder takes, is dropped"
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
    Decision {
        method,
        reasons,
        transcode_audio: !c.audio_ok,
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
            Decision {
                method: PlaybackMethod::Transcode,
                reasons,
                transcode_audio: true,
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
            if dv == DvHandling::Strip {
                reasons.push(
                    "Dolby Vision metadata removed for this device; compatible HDR base kept"
                        .to_owned(),
                );
            }
            if dv == DvHandling::Convert {
                reasons.push(
                    "Dolby Vision Profile 7 converted to Profile 8.1 for this device; the \
                     HDR10-compatible base layer is copied untouched and the dual-layer \
                     enhancement layer, which no consumer decoder takes, is dropped"
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
                || (is_dolby_vision(file) && profile.allows_dolby_vision(file));
            Decision {
                method,
                reasons,
                transcode_audio: !c.audio_ok,
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
mod tests {
    use super::*;
    use crate::domain::{AudioStream, DolbyVisionFacts, MediaFile};

    fn file(container: &str, vcodec: &str, acodec: &str) -> MediaFile {
        MediaFile {
            id: 1,
            item_id: 1,
            path: "/x".into(),
            size: 1,
            mtime: 1,
            duration_ms: Some(1000),
            container: Some(container.to_owned()),
            video_codec: Some(vcodec.to_owned()),
            video_profile: None,
            width: Some(1920),
            height: Some(1080),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            bitrate: Some(8_000_000),
            audio_streams: vec![AudioStream {
                index: 0,
                codec: acodec.to_owned(),
                channels: Some(2),
                default: true,
                ..Default::default()
            }],
            subtitle_streams: vec![],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: crate::domain::DolbyVisionFacts::default(),
        }
    }

    #[test]
    fn profiles_load() {
        assert!(profile("web-h264").is_some());
        assert!(profile("directplay-any").is_some());
        assert_eq!(default_profile().name, "web-h264");
    }

    #[test]
    fn mp4_h264_aac_direct_plays_on_web() {
        let d = decide(
            &file("mp4", "h264", "aac"),
            default_profile(),
            &RenderCaps::proven(true),
        );
        assert_eq!(d.method, PlaybackMethod::DirectPlay);
        assert!(d.reasons.is_empty());
    }

    #[test]
    fn audio_only_audiobook_direct_plays_on_web() {
        let mut audiobook = file("m4b", "h264", "aac");
        audiobook.video_codec = None;
        audiobook.video_profile = None;
        audiobook.width = None;
        audiobook.height = None;
        audiobook.bit_depth = None;
        audiobook.hdr = None;
        audiobook.hdr_format = None;

        let decision = decide(&audiobook, default_profile(), &RenderCaps::proven(true));
        assert_eq!(decision.method, PlaybackMethod::DirectPlay);
        assert!(decision.reasons.is_empty());
    }

    #[test]
    fn mkv_h264_aac_remuxes_on_web() {
        // Right codecs, wrong container → remux, no audio transcode.
        let d = decide(
            &file("mkv", "h264", "aac"),
            default_profile(),
            &RenderCaps::proven(true),
        );
        assert_eq!(d.method, PlaybackMethod::Remux);
        assert!(!d.transcode_audio);
    }

    #[test]
    fn mkv_h264_ac3_remuxes_with_audio_transcode() {
        let d = decide(
            &file("mkv", "h264", "ac3"),
            default_profile(),
            &RenderCaps::proven(true),
        );
        assert_eq!(d.method, PlaybackMethod::Remux);
        assert!(
            d.transcode_audio,
            "ac3 not in web profile → re-encode audio"
        );
    }

    #[test]
    fn hevc_transcodes_on_web_but_direct_plays_on_native() {
        let hevc = file("mkv", "hevc", "aac");
        assert_eq!(
            decide(&hevc, default_profile(), &RenderCaps::proven(true)).method,
            PlaybackMethod::Transcode
        );
        let native = profile("directplay-any").expect("profile");
        assert_eq!(
            decide(&hevc, native, &RenderCaps::proven(true)).method,
            PlaybackMethod::DirectPlay
        );
    }

    #[test]
    fn hdr_forces_transcode_on_sdr_profile() {
        let mut f = file("mp4", "h264", "aac");
        f.hdr = Some("hdr10".to_owned());
        let d = decide(&f, default_profile(), &RenderCaps::proven(true));
        assert_eq!(d.method, PlaybackMethod::Transcode);
        assert!(d.reasons.iter().any(|r| r.contains("HDR")));
    }

    // A browser that reports HEVC (e.g. Safari) turns a would-be transcode into
    // a copy-video remux — the whole point of runtime capability probing.
    #[test]
    fn hevc_direct_or_remuxes_when_browser_reports_it() {
        let hevc_mp4 = file("mp4", "hevc", "aac");
        let caps = caps_profile(
            vec!["mp4".into(), "webm".into()],
            vec!["h264".into(), "hevc".into()],
            vec!["aac".into(), "opus".into()],
            None,
            false,
            false,
        );
        assert_eq!(
            decide(&hevc_mp4, &caps, &RenderCaps::proven(true)).method,
            PlaybackMethod::DirectPlay
        );
        // Same codecs, MKV container → remux (copy video), not transcode.
        let hevc_mkv = file("mkv", "hevc", "aac");
        assert_eq!(
            decide(&hevc_mkv, &caps, &RenderCaps::proven(true)).method,
            PlaybackMethod::Remux
        );
    }

    #[test]
    fn automatic_routing_matrix_covers_every_compatibility_dimension() {
        let baseline = file("mp4", "h264", "aac");
        assert_eq!(
            decide(&baseline, default_profile(), &RenderCaps::proven(true)).method,
            PlaybackMethod::DirectPlay
        );

        let mut wrong_container = baseline.clone();
        wrong_container.container = Some("mkv".into());
        assert_eq!(
            decide(
                &wrong_container,
                default_profile(),
                &RenderCaps::proven(true)
            )
            .method,
            PlaybackMethod::Remux
        );

        let mut wrong_audio = baseline.clone();
        wrong_audio.audio_streams[0].codec = "dts".into();
        let audio = decide(&wrong_audio, default_profile(), &RenderCaps::proven(true));
        assert_eq!(audio.method, PlaybackMethod::Remux);
        assert!(audio.transcode_audio);

        let mut corrected_sync = baseline.clone();
        corrected_sync.audio_offset_ms = 125;
        assert_eq!(
            decide(
                &corrected_sync,
                default_profile(),
                &RenderCaps::proven(true)
            )
            .method,
            PlaybackMethod::Remux
        );

        let mut wrong_video = baseline.clone();
        wrong_video.video_codec = Some("mpeg2video".into());
        assert_eq!(
            decide(&wrong_video, default_profile(), &RenderCaps::proven(true)).method,
            PlaybackMethod::Transcode
        );

        let mut capped = default_profile().clone();
        capped.max_height = Some(720);
        assert_eq!(
            decide(&baseline, &capped, &RenderCaps::proven(true)).method,
            PlaybackMethod::Transcode
        );

        capped.max_height = None;
        capped.max_bitrate = Some(4_000_000);
        assert_eq!(
            decide(&baseline, &capped, &RenderCaps::proven(true)).method,
            PlaybackMethod::Transcode
        );

        let mut hdr = baseline.clone();
        hdr.hdr = Some("hdr10".into());
        assert_eq!(
            decide(&hdr, default_profile(), &RenderCaps::proven(true)).method,
            PlaybackMethod::Transcode
        );

        let mut unprobed_container = baseline;
        unprobed_container.container = None;
        assert_eq!(
            decide(
                &unprobed_container,
                default_profile(),
                &RenderCaps::proven(true)
            )
            .method,
            PlaybackMethod::Remux,
            "unknown compatibility is not permission to hand over the raw container"
        );
    }

    #[test]
    fn manual_quality_matrix_pins_auto_original_and_every_rung() {
        assert_eq!(Force::parse("auto"), Force::Auto);
        assert_eq!(Force::parse("original"), Force::Original);
        assert_eq!(Force::parse("transcode"), Force::Transcode);
        assert_eq!(Force::parse("future-value"), Force::Auto);

        let unsupported_video = file("mp4", "hevc", "aac");
        assert_eq!(
            decide_forced(
                &unsupported_video,
                default_profile(),
                Force::Auto,
                &RenderCaps::proven(true)
            )
            .method,
            PlaybackMethod::Transcode
        );
        assert_eq!(
            decide_forced(
                &unsupported_video,
                default_profile(),
                Force::Original,
                &RenderCaps::proven(true)
            )
            .method,
            PlaybackMethod::DirectPlay,
            "Original keeps the source bytes when its container and audio are usable; the client owns the rescue"
        );

        let incompatible_envelope = file("mkv", "hevc", "dts");
        let original = decide_forced(
            &incompatible_envelope,
            default_profile(),
            Force::Original,
            &RenderCaps::proven(true),
        );
        assert_eq!(original.method, PlaybackMethod::Remux);
        assert!(original.transcode_audio);

        let direct = file("mp4", "h264", "aac");
        assert_eq!(
            decide_forced(
                &direct,
                default_profile(),
                Force::Transcode,
                &RenderCaps::proven(true)
            )
            .method,
            PlaybackMethod::Transcode
        );
    }

    /// The Chrome-refuses-Dolby-Vision failure, as a decision. Both films it
    /// was found on (a P7 and a P8 disc remux, both "HDR10-compatible") were
    /// remuxed to Chrome because the client claimed HDR — and Chrome refused
    /// the track outright, so the player's error path re-encoded a 4K remux
    /// down to the Auto rung. Safari played the same files untouched.
    #[test]
    fn dolby_vision_is_not_handed_to_a_browser_that_cannot_decode_it() {
        let mut dv = file("mkv", "hevc", "aac");
        dv.hdr = Some("dolby_vision".to_owned());
        dv.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".to_owned());
        let hdr_client = |dolby: bool| {
            caps_profile(
                vec!["mkv".into(), "mp4".into()],
                vec!["hevc".into(), "h264".into()],
                vec!["aac".into()],
                None,
                true,
                dolby,
            )
        };

        // Safari: decodes DV, so nothing changes — the file direct-plays.
        let safari = hdr_client(true);
        assert_eq!(
            decide(&dv, &safari, &RenderCaps::proven(true)).method,
            PlaybackMethod::DirectPlay,
            "a client that decodes Dolby Vision is handed it untouched"
        );
        assert!(decide(&dv, &safari, &RenderCaps::proven(true)).preserve_dolby_vision);

        // Chrome, on a server that can strip: a remux, not a re-encode. The
        // base layer is kept, so the viewer still gets the source's pixels —
        // but it takes ffmpeg, so the raw file may not be handed over.
        let chrome = hdr_client(false);
        let stripped = decide(&dv, &chrome, &RenderCaps::proven(true));
        assert_eq!(stripped.method, PlaybackMethod::Remux);
        assert!(
            stripped.reasons.iter().any(|r| r.contains("Dolby Vision")),
            "and it says so: {:?}",
            stripped.reasons
        );
        assert!(!stripped.preserve_dolby_vision);

        // Chrome, on a server that cannot strip (ffmpeg < 7.1): the only
        // stream this browser will play is a re-encoded one. Deciding that up
        // front is the point — the alternative is what shipped: a remux the
        // browser refuses, then a rescue nobody asked for.
        let reencoded = decide(&dv, &chrome, &RenderCaps::proven(false));
        assert_eq!(reencoded.method, PlaybackMethod::Transcode);
        assert!(
            reencoded.reasons.iter().any(|r| r.contains("7.1")),
            "naming the fix, not just the symptom: {:?}",
            reencoded.reasons
        );

        // An HDR10 file is untouched by any of this — the rule is about the
        // Dolby Vision configuration, not about HDR.
        let mut hdr10 = file("mp4", "hevc", "aac");
        hdr10.hdr = Some("hdr10".to_owned());
        assert_eq!(
            decide(&hdr10, &chrome, &RenderCaps::proven(false)).method,
            PlaybackMethod::DirectPlay
        );
    }

    /// Original means "no video re-encode", and a DV strip honours that — the
    /// base layer is copied. But it still cannot be direct play, because the
    /// raw file is what the browser refuses.
    #[test]
    fn forced_original_strips_dolby_vision_rather_than_handing_over_the_raw_file() {
        let mut dv = file("mp4", "hevc", "aac"); // container+audio both fine
        dv.hdr = Some("dolby_vision".to_owned());
        dv.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".to_owned());
        let chrome = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            true,
            false,
        );
        let d = decide_forced(&dv, &chrome, Force::Original, &RenderCaps::proven(true));
        assert_eq!(
            d.method,
            PlaybackMethod::Remux,
            "direct play would hand over the DV track the browser refuses"
        );
        assert!(
            !d.transcode_audio,
            "and the audio it can already play is copied"
        );

        let safari = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            true,
            true,
        );
        assert_eq!(
            decide_forced(&dv, &safari, Force::Original, &RenderCaps::proven(true)).method,
            PlaybackMethod::DirectPlay
        );
    }

    /// The whole point of M5, at the decision layer.
    ///
    /// A Chrome-shaped client — HEVC Main10 decoder, HDR display, no Dolby
    /// Vision at any profile — handed a Profile 5 file used to be laddered
    /// down to an SDR H.264 transcode, because the only re-encode grade that
    /// existed was SDR. With `hdr10t=1` it gets the HEVC Main10 PQ rung
    /// instead, and the badge says so.
    #[test]
    fn a_main10_pq_client_gets_the_hdr10_rung_for_a_profile5_source() {
        let mut p5 = file("mkv", "hevc", "aac");
        p5.hdr = Some("dolby_vision".to_owned());
        p5.hdr_format = Some("Dolby Vision · Profile 5".to_owned());

        let mut chrome = caps_profile(
            vec!["mp4".into(), "webm".into()],
            vec!["h264".into(), "hevc".into()],
            vec!["aac".into()],
            Some(2160),
            true,
            false,
        );
        chrome.supports_hdr10_transcode = true;

        let hdr10 = decide(&p5, &chrome, &RenderCaps::proven(true));
        assert_eq!(hdr10.method, PlaybackMethod::Transcode);
        assert!(!hdr10.preserve_dolby_vision);
        assert_eq!(hdr10.transcode_grade, OutputGrade::Hdr10);
        assert_eq!(hdr10.delivered_dynamic_range, "hdr10");
        assert!(
            hdr10
                .reasons
                .iter()
                .any(|reason| reason.contains("Main10 PQ")),
            "the grade change has to be explainable: {:?}",
            hdr10.reasons
        );
        // The static-metadata gap is disclosed, not hidden: a real Profile 5
        // source carries no MDCV/CLL to inherit.
        assert!(
            hdr10.reasons.iter().any(|reason| reason.contains("static")),
            "{:?}",
            hdr10.reasons
        );

        // The same client, same file, without the proof: today's path,
        // unchanged in every field.
        let mut unproven = chrome.clone();
        unproven.supports_hdr10_transcode = false;
        let sdr = decide(&p5, &unproven, &RenderCaps::proven(true));
        assert_eq!(sdr.method, PlaybackMethod::Transcode);
        assert_eq!(sdr.transcode_grade, OutputGrade::Sdr);
        assert_eq!(sdr.delivered_dynamic_range, "sdr");
        assert!(
            !sdr.reasons
                .iter()
                .any(|reason| reason.contains("re-encoding to HEVC Main10 PQ")),
            "{:?}",
            sdr.reasons
        );
        assert!(
            sdr.reasons
                .iter()
                .any(|reason| reason.contains("this client did not prove")),
            "the refusal names the client that refused: {:?}",
            sdr.reasons
        );
        // …and nothing else about the verdict moved.
        assert!(!sdr.preserve_dolby_vision);
        assert!(!sdr.transcode_audio);
        assert_eq!(sdr.container, "mp4");

        // A client that decodes the source's Dolby Vision still gets Dolby
        // Vision. The HDR10 rung is a consolation prize, never a downgrade of
        // a client that never needed one — and `hdr10t=1` must not become a
        // reason to stop preserving.
        let mut safari = chrome.clone();
        safari.dolby_vision_profiles = vec![5, 8];
        let preserved = decide(&p5, &safari, &RenderCaps::proven(true));
        assert!(preserved.preserve_dolby_vision);
        assert_eq!(preserved.delivered_dynamic_range, "dolby_vision");
        assert_eq!(preserved.transcode_grade, OutputGrade::Sdr);
        assert_ne!(preserved.method, PlaybackMethod::Transcode);
    }

    /// The columns answer, and the label is only the fallback.
    ///
    /// Reading a profile number back out of a display string is exactly as
    /// fragile as it sounds. The case that mattered: a scan that saw the codec
    /// tag but no configuration record wrote the bare string "Dolby Vision",
    /// which parses to no profile at all — so the file was unclaimable by
    /// every client, forever, however good its actual record was.
    #[test]
    fn dolby_vision_facts_come_from_the_columns_before_the_label() {
        let mut file = file("mkv", "hevc", "aac");
        file.hdr = Some("dolby_vision".to_owned());

        // A row the backfill has not reached: the label is all there is.
        file.hdr_format = Some("Dolby Vision · Profile 8 (HDR10-compatible)".to_owned());
        assert_eq!(dolby_vision_profile(&file), Some(8));
        assert!(has_compatible_dv_base(&file));

        // The same row with columns: they win, and they can say things the
        // label cannot spell.
        file.dolby_vision = DolbyVisionFacts {
            profile: Some(7),
            level: Some(6),
            bl_compat_id: Some(6),
            el_present: Some(true),
            rpu_present: Some(true),
        };
        assert_eq!(dolby_vision_profile(&file), Some(7));
        assert!(has_compatible_dv_base(&file));

        // A file whose label says nothing at all is still claimable once its
        // columns are filled in — this is the state the backfill exists for.
        file.hdr_format = Some("Dolby Vision".to_owned());
        assert_eq!(dolby_vision_profile(&file), Some(7));
        assert!(
            has_compatible_dv_base(&file),
            "the compatibility id is a fact the bare label never carried"
        );

        // A compatibility id of 0 is a real answer, not a missing one: a
        // Profile 5 has no base a non-DV client can watch, whatever a stale
        // label claims.
        file.dolby_vision = DolbyVisionFacts {
            profile: Some(5),
            bl_compat_id: Some(0),
            rpu_present: Some(true),
            el_present: Some(false),
            ..DolbyVisionFacts::default()
        };
        file.hdr_format = Some("Dolby Vision · Profile 5 (HDR10-compatible)".to_owned());
        assert!(
            !has_compatible_dv_base(&file),
            "the column overrules a label that disagrees with it"
        );
        assert!(dolby_vision_needs_rpu_render(&file));

        // And the decision that hangs off it: a client listing profile 7 can
        // claim a file whose label never named one.
        let mut chrome = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            true,
            false,
        );
        chrome.dolby_vision_profiles = vec![7];
        file.dolby_vision = DolbyVisionFacts {
            profile: Some(7),
            bl_compat_id: Some(6),
            ..DolbyVisionFacts::default()
        };
        file.hdr_format = Some("Dolby Vision".to_owned());
        assert!(
            decide(&file, &chrome, &RenderCaps::proven(true)).preserve_dolby_vision,
            "an unlabelled Profile 7 was unclaimable by every client before M2"
        );
    }

    /// The whole negotiation, one row per (source, client, node) triple.
    ///
    /// The point of the matrix rather than a handful of cases: the rule is
    /// "the highest grade all three admit", and a rule with three inputs has
    /// exactly one way to be tested — every combination that can occur, with
    /// the answer written down beside it.
    #[test]
    fn the_grade_is_the_highest_all_three_parties_admit() {
        let hdr_client = || {
            let mut client = caps_profile(
                vec!["mp4".into()],
                vec!["h264".into(), "hevc".into()],
                vec!["aac".into()],
                None,
                true,
                false,
            );
            client.supports_hdr10_transcode = true;
            client
        };
        let source = |hdr: Option<&str>, label: Option<&str>| {
            let mut file = file("mkv", "hevc", "aac");
            file.hdr = hdr.map(str::to_owned);
            file.hdr_format = label.map(str::to_owned);
            file
        };
        let p8 = || {
            source(
                Some("dolby_vision"),
                Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
            )
        };
        let p5 = || source(Some("dolby_vision"), Some("Dolby Vision · Profile 5"));

        let everything = RenderCaps::proven(true);
        let no_plain = RenderCaps {
            dv_strippable: true,
            dolby_vision_p5_render: true,
            hdr10_passthrough: false,
            hdr10_max_height: HDR10_MAX_MEASURED_HEIGHT,
            dolby_vision_convert: false,
        };
        let no_rpu = RenderCaps {
            dv_strippable: true,
            dolby_vision_p5_render: false,
            hdr10_passthrough: true,
            hdr10_max_height: HDR10_MAX_MEASURED_HEIGHT,
            dolby_vision_convert: false,
        };
        let nothing = RenderCaps::strip_only(true);

        // (source, node, expected grade, a fragment of the expected reason)
        let cases: [(MediaFile, RenderCaps, OutputGrade, &str); 10] = [
            // An SDR source has no grade to lose and says nothing about it.
            (source(None, None), everything, OutputGrade::Sdr, ""),
            // Plain PQ, by the plain route.
            (
                source(Some("hdr10"), None),
                everything,
                OutputGrade::Hdr10,
                "HDR kept",
            ),
            (
                source(Some("hdr10plus"), None),
                everything,
                OutputGrade::Hdr10,
                "HDR kept",
            ),
            (
                source(Some("hdr10"), None),
                no_plain,
                OutputGrade::Sdr,
                "this node",
            ),
            // The Dolby route and the plain route are proved separately, and
            // each source takes only its own: a node that proved the RPU
            // renderer and not the plain encode still tone-maps plain HDR10.
            (p5(), everything, OutputGrade::Hdr10, "HDR kept"),
            (p5(), no_rpu, OutputGrade::Sdr, "this node"),
            (p5(), nothing, OutputGrade::Sdr, "this node"),
            // A Dolby Vision base layer that is HDR10-compatible decodes to
            // ordinary PQ frames, so it is the PLAIN route, not the RPU one.
            (p8(), no_rpu, OutputGrade::Hdr10, "HDR kept"),
            (p8(), no_plain, OutputGrade::Sdr, "this node"),
            // HLG has no grade that spells it. Refused on every node.
            (
                source(Some("hlg"), None),
                everything,
                OutputGrade::Sdr,
                "HLG has no HDR encode rung",
            ),
        ];
        for (file, node, expected, reason) in cases {
            let (grade, why) = target_grade(&file, &hdr_client(), &node);
            assert_eq!(
                grade, expected,
                "{:?} / {:?} on {node:?}",
                file.hdr, file.hdr_format
            );
            assert!(
                why.contains(reason),
                "{:?} / {:?}: reason {why:?} does not name {reason:?}",
                file.hdr,
                file.hdr_format
            );
        }

        // The client is the third party, and it refuses independently of the
        // other two. Both of its bits are required: `supports_hdr` is "this
        // display shows HDR at all", `supports_hdr10_transcode` is "this
        // decoder takes Main10 PQ", and either one absent means not proven.
        let mut no_pq = hdr_client();
        no_pq.supports_hdr10_transcode = false;
        let (grade, why) = target_grade(&source(Some("hdr10"), None), &no_pq, &everything);
        assert_eq!(grade, OutputGrade::Sdr);
        assert!(why.contains("this client did not prove"), "{why}");

        // An SDR display is refused too, but silently: `evaluate` has already
        // pushed "HDR (…) presentation was not proven by this client", which
        // names the bit that actually refused. A second line here would put
        // two entries in the badge for one refusal and blame the Main10
        // decoder for a decision the display made.
        let mut sdr_display = hdr_client();
        sdr_display.supports_hdr = false;
        let (grade, why) = target_grade(&source(Some("hdr10"), None), &sdr_display, &everything);
        assert_eq!(grade, OutputGrade::Sdr);
        assert_eq!(why, "");
        let plan = decide(&source(Some("hdr10"), None), &sdr_display, &everything);
        assert_eq!(
            plan.reasons
                .iter()
                .filter(|reason| reason.contains("tone-map"))
                .count(),
            1,
            "one refusal, one line: {:?}",
            plan.reasons
        );

        // A client that cannot decode HEVC is refused whatever it proved about
        // PQ: the rung emits HEVC Main10 and nothing else.
        let mut no_hevc = hdr_client();
        no_hevc.video_codecs = vec!["h264".into()];
        let (grade, why) = target_grade(&source(Some("hdr10"), None), &no_hevc, &everything);
        assert_eq!(grade, OutputGrade::Sdr);
        assert!(why.contains("does not decode HEVC"), "{why}");
    }

    /// The grade classification and the renderer choice read the same
    /// function, so the badge and the bytes cannot disagree about which graph
    /// ran.
    ///
    /// The two graphs are not interchangeable in either direction, and both
    /// mistakes are silent. The Dolby graph applies an RPU and, handed plain
    /// PQ frames, emits crushed shadows and clipping above roughly 70% at exit
    /// 0. The plain graph copies pixel values and would put a Profile 5 base
    /// layer — which is not an HDR10 grade in any sense — on the wire tagged
    /// as one.
    #[test]
    fn every_hdr_source_names_exactly_one_renderer() {
        let cases = [
            (None, None, None),
            (Some("hdr10"), None, Some(HdrRoute::Passthrough)),
            (Some("hdr10plus"), None, Some(HdrRoute::Passthrough)),
            (Some("hlg"), None, None),
            (
                Some("dolby_vision"),
                Some("Dolby Vision · Profile 5"),
                Some(HdrRoute::DolbyVisionRpu),
            ),
            (
                Some("dolby_vision"),
                Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
                Some(HdrRoute::Passthrough),
            ),
            (
                Some("dolby_vision"),
                Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
                Some(HdrRoute::Passthrough),
            ),
            // An HLG-compatible base is still HLG, and there is no HLG rung.
            (
                Some("dolby_vision"),
                Some("Dolby Vision · Profile 7 (HLG-compatible)"),
                None,
            ),
            // No compatible base and no RPU this server can read.
            (Some("dolby_vision"), Some("Dolby Vision · Profile 7"), None),
            (Some("dolby_vision"), Some("Dolby Vision · Profile 4"), None),
            (Some("dolby_vision"), Some("Dolby Vision"), None),
            (Some("dolby_vision"), None, None),
        ];
        for (hdr, label, expected) in cases {
            let mut file = file("mkv", "hevc", "aac");
            file.hdr = hdr.map(str::to_owned);
            file.hdr_format = label.map(str::to_owned);
            assert_eq!(hdr_route(&file), expected, "{hdr:?} / {label:?}");
            // And the one route that exists agrees with the RPU predicate the
            // daemon's renderer choice is pinned to.
            assert_eq!(
                hdr_route(&file) == Some(HdrRoute::DolbyVisionRpu),
                dolby_vision_needs_rpu_render(&file),
                "{hdr:?} / {label:?}"
            );
        }
    }

    fn four_k_hevc() -> MediaFile {
        let mut file = file("mp4", "hevc", "aac");
        file.video_profile = Some("Main 10".into());
        file.width = Some(3840);
        file.height = Some(2160);
        file.bit_depth = Some(10);
        file.bitrate = Some(50_000_000);
        file
    }

    fn learned(file: &MediaFile, age_ms: i64) -> caps::LearnedLimit {
        caps::LearnedLimit {
            identity: caps::decode_limit_identity(file),
            label: "4K HEVC Main 10".to_owned(),
            lost: 41,
            secs: 60,
            rate: 41.0,
            at_ms: NOW_MS - age_ms,
        }
    }

    const NOW_MS: i64 = 1_756_400_000_000;
    const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

    fn applied(profile: &DeviceProfile, file: &MediaFile, now_ms: i64) -> Decision {
        let mut profile = profile.clone();
        profile.retain_applicable_learned_limits(now_ms);
        decide(file, &profile, &RenderCaps::proven(true))
    }

    /// A limit the client already applies, applied by the server too.
    ///
    /// The gap this closes: the browser routed every matching title to a
    /// forced transcode after one stuttery session and told nobody, so
    /// `/decision` kept promising a direct play the client had already
    /// decided against — and every reason string shown to that viewer
    /// described a delivery that never happened.
    #[test]
    fn a_learned_client_limit_demotes_the_verdict_in_the_clients_own_words() {
        let file = four_k_hevc();
        let mut profile = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into(), "h264".into()],
            vec!["aac".into()],
            None,
            true,
            false,
        );

        // Without the limit this is an ordinary direct play.
        assert_eq!(
            applied(&profile, &file, NOW_MS).method,
            PlaybackMethod::DirectPlay
        );

        profile.learned_limits = vec![learned(&file, DAY_MS)];
        let after = applied(&profile, &file, NOW_MS);
        assert_eq!(
            after.method,
            PlaybackMethod::Transcode,
            "a decoder that could not keep up with these frames is not helped \
             by the same frames in a different envelope"
        );
        assert!(
            after.reasons.iter().any(|reason| reason
                == "learned client-performance limit for 4K HEVC Main 10: \
                    lost 41 frames in 60s (41/min)"),
            "the reason must be the browser's own sentence: {:?}",
            after.reasons
        );

        // A limit for a different media load does not touch this one. The
        // identity exists because the old codec-and-height key poisoned every
        // 4K HEVC title on the strength of one 90 Mb/s remux.
        let mut other = file.clone();
        other.bitrate = Some(90_000_000);
        assert_eq!(
            applied(&profile, &other, NOW_MS).method,
            PlaybackMethod::DirectPlay,
            "a limit learned at 50 Mb/s must not condemn the 90 Mb/s load"
        );
    }

    /// The server applies the client's policy, not a stricter one of its own.
    ///
    /// The browser stops *applying* an entry after a week and only stops
    /// *keeping* it after a month, so an identity match alone would honour,
    /// for twenty-three days, entries the browser had already decided to
    /// re-measure. Worse, it would break the re-measurement: the browser only
    /// consults its own limits when the server answered `direct_play` or
    /// `remux`, so a server that answers `transcode` first is a server that
    /// makes the weekly re-test — the fix for limits that were "in practice
    /// permanent" — never run again.
    ///
    /// Standing aside at exactly the right moment is therefore not leniency.
    /// It is what hands the decision back to the only party that can
    /// re-measure it.
    #[test]
    fn an_expired_limit_hands_the_decision_back_to_the_client_that_can_retest() {
        let file = four_k_hevc();
        let mut profile = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into(), "h264".into()],
            vec!["aac".into()],
            None,
            true,
            false,
        );

        for (age_days, expected, why) in [
            (0, PlaybackMethod::Transcode, "measured moments ago"),
            (6, PlaybackMethod::Transcode, "inside the re-test window"),
            (7, PlaybackMethod::Transcode, "exactly at the boundary"),
            (
                8,
                PlaybackMethod::DirectPlay,
                "past re-test: the browser would re-measure, so the server steps aside",
            ),
            (
                31,
                PlaybackMethod::DirectPlay,
                "past the TTL: the browser has already thrown this away",
            ),
        ] {
            profile.learned_limits = vec![learned(&file, age_days * DAY_MS)];
            assert_eq!(
                applied(&profile, &file, NOW_MS).method,
                expected,
                "{age_days} days old: {why}"
            );
        }

        // A negative age is a clock disagreement, not a very fresh entry, and
        // an entry with no timestamp is not evidence of freshness either.
        for at_ms in [NOW_MS + DAY_MS, 0, -1] {
            profile.learned_limits = vec![caps::LearnedLimit {
                at_ms,
                ..learned(&file, 0)
            }];
            assert_eq!(
                applied(&profile, &file, NOW_MS).method,
                PlaybackMethod::DirectPlay,
                "at_ms {at_ms}"
            );
        }

        // And a server with no clock applies nothing at all.
        profile.learned_limits = vec![learned(&file, 0)];
        assert_eq!(
            applied(&profile, &file, 0).method,
            PlaybackMethod::DirectPlay
        );
    }

    /// What the demotion costs the picture is part of the sentence.
    ///
    /// The browser appends `HDR10 → SDR` when its own limit is what turns an
    /// HDR delivery into an SDR one, and `docs/PLAYBACK.md` promises the
    /// Reason row names that consequence. A server that dropped it would put
    /// two different explanations of one decision in front of one viewer.
    #[test]
    fn the_reason_names_the_grade_the_demotion_costs() {
        let mut file = four_k_hevc();
        file.hdr = Some("hdr10".into());
        file.hdr_format = Some("HDR10".into());

        // A client that cannot present PQ: this demotion really does cost the
        // grade, so the browser's consequence clause applies.
        let mut sdr_only = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into(), "h264".into()],
            vec!["aac".into()],
            None,
            true,
            false,
        );
        sdr_only.learned_limits = vec![learned(&file, DAY_MS)];
        let lost = applied(&sdr_only, &file, NOW_MS);
        assert!(
            lost.reasons.iter().any(
                |reason| reason.ends_with("lost 41 frames in 60s (41/min); HDR10 \u{2192} SDR")
            ),
            "{:?}",
            lost.reasons
        );

        // A client that presents Main10 PQ keeps HDR10 through the transcode,
        // so nothing was lost and the clause must not appear — reporting a
        // grade loss on a rung that is still HDR10 is the same lie in reverse.
        let mut pq = sdr_only.clone();
        pq.supports_hdr10_transcode = true;
        pq.presents = [(
            "hevc".to_owned(),
            [caps::Transfer::Sdr, caps::Transfer::Pq]
                .into_iter()
                .collect(),
        )]
        .into();
        let kept = applied(&pq, &file, NOW_MS);
        assert_eq!(kept.transcode_grade, OutputGrade::Hdr10);
        assert!(
            kept.reasons
                .iter()
                .any(|reason| reason.ends_with("(41/min)")),
            "{:?}",
            kept.reasons
        );
    }

    /// A planned strip that gets overtaken must not keep describing itself.
    ///
    /// The strip branch says the compatible HDR base was "kept" — true of the
    /// remux it plans. A learned limit then turns that remux into a
    /// transcode, and the sentence becomes a claim about untouched bytes on a
    /// delivery that re-encoded them.
    #[test]
    fn a_strip_overtaken_by_a_demotion_stops_claiming_the_base_was_kept() {
        let mut file = four_k_hevc();
        file.hdr = Some("dolby_vision".into());
        file.hdr_format = Some("Dolby Vision · Profile 8 (HDR10-compatible)".into());

        let mut profile = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into(), "h264".into()],
            vec!["aac".into()],
            None,
            true,
            false,
        );

        let stripped = applied(&profile, &file, NOW_MS);
        assert_eq!(stripped.method, PlaybackMethod::Remux);
        assert!(
            stripped
                .reasons
                .iter()
                .any(|reason| reason.ends_with("compatible HDR base kept")),
            "{:?}",
            stripped.reasons
        );

        profile.learned_limits = vec![learned(&file, DAY_MS)];
        let demoted = applied(&profile, &file, NOW_MS);
        assert_eq!(demoted.method, PlaybackMethod::Transcode);
        assert!(
            !demoted
                .reasons
                .iter()
                .any(|reason| reason.contains("HDR base kept")),
            "a re-encode did not keep anything untouched: {:?}",
            demoted.reasons
        );
        assert!(
            demoted
                .reasons
                .iter()
                .any(|reason| reason.contains("re-encoding from the compatible HDR base")),
            "{:?}",
            demoted.reasons
        );
    }

    /// The reason is reprinted verbatim, so what a client may put in it is
    /// bounded before it is ever printed.
    ///
    /// It reaches the badge, the stats overlay, the admin session list and
    /// the server log. A newline in a log line is how one line becomes two
    /// forged ones, and sixty thousand characters of label is an
    /// authenticated caller choosing how much of every `/decision` response
    /// it occupies.
    #[test]
    fn a_hostile_learned_limit_is_bounded_before_it_is_ever_printed() {
        let caps: caps::DeviceCaps = serde_json::from_str(&format!(
            r#"{{"v":2,"video":[{{"codec":"hevc"}}],"audio":[],"containers":[],
                "learned_limits":[{{"identity":"decode-v2:[]","label":{},
                                   "lost":1,"secs":1,"rate":1,"at_ms":1}}]}}"#,
            serde_json::Value::from(format!("x\n2026 WARN forged\r\n{}", "y".repeat(60_000)))
        ))
        .expect("the document still parses");

        let profile = DeviceProfile::from_caps_v2(&caps);
        let label = &profile.learned_limits[0].label;
        assert!(
            !label.chars().any(char::is_control),
            "a control character in a reprinted label forges log lines: {label:?}"
        );
        assert!(label.chars().count() <= 160, "{}", label.chars().count());

        // And the count of entries is bounded too, not just each one.
        let many: Vec<serde_json::Value> = (0..500)
            .map(|n| serde_json::json!({"identity": format!("decode-v2:[{n}]"), "at_ms": 1}))
            .collect();
        let flood: caps::DeviceCaps = serde_json::from_value(serde_json::json!({
            "v": 2, "video": [{"codec": "hevc"}], "audio": [], "containers": [],
            "learned_limits": many,
        }))
        .expect("the document parses");
        assert_eq!(
            DeviceProfile::from_caps_v2(&flood).learned_limits.len(),
            256
        );
    }

    /// A limit with no identity matches nothing, ever.
    ///
    /// The browser stores these in a map keyed BY the identity, so the
    /// obvious client serialization — the map's values — omits it, and
    /// `LearnedLimit::identity` is `#[serde(default)]` precisely so that
    /// mistake cannot refuse a create. It must therefore also be unable to
    /// match: an empty identity that compared equal to anything would demote
    /// every title on the server on the strength of one malformed entry.
    #[test]
    fn a_learned_limit_with_no_identity_can_never_match() {
        let file = four_k_hevc();
        let mut profile = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into(), "h264".into()],
            vec!["aac".into()],
            None,
            true,
            false,
        );
        profile.learned_limits = vec![caps::LearnedLimit {
            identity: String::new(),
            ..learned(&file, DAY_MS)
        }];
        assert_eq!(
            applied(&profile, &file, NOW_MS).method,
            PlaybackMethod::DirectPlay
        );
    }

    /// A limit with no label, and one the browser recorded at a zero rate.
    ///
    /// Both are shapes a real `localStorage` entry takes, and neither may
    /// produce a sentence with a hole in it — nor a sentence the browser
    /// would not have written. `pruneDecodeLimits` deliberately keeps a
    /// zero-rate entry, so printing "measured unstable original playback"
    /// over it would be the server inventing a wording for a measurement the
    /// browser prints in full.
    #[test]
    fn a_partial_learned_limit_still_produces_a_whole_sentence() {
        let unlabelled = caps::LearnedLimit {
            identity: "decode-v2:[\"hevc\"]".to_owned(),
            label: "   ".to_owned(),
            lost: 41,
            secs: 60,
            rate: 41.0,
            at_ms: NOW_MS,
        };
        assert_eq!(
            unlabelled.reason(None),
            "learned client-performance limit for decode-v2:[\"hevc\"]: \
             lost 41 frames in 60s (41/min)"
        );

        let zero_rate = caps::LearnedLimit {
            label: "HEVC Main 10".to_owned(),
            lost: 0,
            rate: 0.0,
            ..unlabelled.clone()
        };
        assert_eq!(
            zero_rate.reason(None),
            "learned client-performance limit for HEVC Main 10: \
             lost 0 frames in 60s (0/min)",
            "the browser keeps a zero-rate entry and prints it in full"
        );

        let anonymous = caps::LearnedLimit {
            identity: String::new(),
            label: String::new(),
            ..unlabelled
        };
        assert_eq!(
            anonymous.reason(Some("dolby_vision")),
            "learned client-performance limit for this media load: \
             lost 41 frames in 60s (41/min); Dolby Vision \u{2192} SDR",
            "no subject is still not a sentence with a hole in it"
        );
    }

    /// The document the browser actually sends must deserialize.
    ///
    /// This is the one that shipped broken. `capsDocument` puts the rate on
    /// the wire exactly as `lostFrameRate` measured it — one decimal place —
    /// and `serde_json` refuses a floating-point literal for an integer field,
    /// `4.0` included. The refusal is not scoped to the field: it aborts the
    /// whole `DeviceCaps`, and with it the `CreateSession` or decision body
    /// carrying it. So one decode rescue recorded in a browser's local storage
    /// was enough to make that browser's capabilities unreadable for thirty
    /// days.
    ///
    /// It went unseen because `askDecision` catches the 400 and retries as the
    /// flat query, so the viewer got a working, quietly less informed answer.
    /// A create has no such fallback: once the web client sends its caps
    /// there, this is a title that will not play.
    #[test]
    fn a_learned_limit_survives_the_rate_the_browser_measures() {
        // Verbatim shape from `capsDocument`, fraction and all.
        let document: caps::DeviceCaps = serde_json::from_str(
            r#"{"v":2,"video":[{"codec":"hevc"}],
                "learned_limits":[{"identity":"v1|hevc|main10|2160|b40","label":"4K HEVC Main 10",
                                   "lost":13,"secs":19,"rate":41.1,"at":1788000000000}]}"#,
        )
        .expect("the document the browser sends has to deserialize");
        let limit = &document.learned_limits[0];
        assert!((limit.rate - 41.1).abs() < f64::EPSILON);
        assert_eq!(limit.at_ms, 1_788_000_000_000, "`at` is the web's spelling");
        assert!(
            limit.reason(None).contains("41.1/min"),
            "the fraction the client measured is what gets printed: {}",
            limit.reason(None)
        );

        // A whole number is a float on this wire too — `+(4).toFixed(1)` is
        // `4`, so both spellings reach the server from the same code path.
        let whole: caps::DeviceCaps =
            serde_json::from_str(r#"{"v":2,"audio":["aac"],"learned_limits":[{"rate":4}]}"#)
                .expect("an integer rate is still a rate");
        assert!((whole.learned_limits[0].rate - 4.0).abs() < f64::EPSILON);
    }

    /// The browser and the server must key a learned limit identically.
    ///
    /// `tests/playback/decode-limit-identity.json` is the contract, and it is
    /// generated from the browser's own function. `web-policy.test.js` runs
    /// the same rows through `decodeLimitIdentity`, so a change to either
    /// spelling fails in both languages at once.
    ///
    /// A divergence here has no symptom. Nothing errors: the server's key
    /// simply never matches the browser's, no limit is ever applied, and the
    /// viewer keeps stuttering through the exact title they already taught
    /// their browser to avoid.
    #[test]
    fn the_server_keys_a_learned_limit_the_way_the_browser_does() {
        #[derive(serde::Deserialize)]
        struct Source {
            video_codec: Option<String>,
            video_profile: Option<String>,
            width: Option<i64>,
            height: Option<i64>,
            bit_depth: Option<i64>,
            hdr: Option<String>,
            hdr_format: Option<String>,
            bitrate: Option<i64>,
        }
        #[derive(serde::Deserialize)]
        struct Case {
            name: String,
            source: Source,
            identity: String,
        }
        #[derive(serde::Deserialize)]
        struct Fixture {
            cases: Vec<Case>,
        }

        let raw = include_str!("../../../../tests/playback/decode-limit-identity.json");
        let fixture: Fixture = serde_json::from_str(raw).expect("the shared fixture parses");
        assert!(
            fixture.cases.len() >= 10,
            "a fixture this small stops being a contract"
        );

        for case in &fixture.cases {
            let mut media = file("mkv", "hevc", "aac");
            media.video_codec = case.source.video_codec.clone();
            media.video_profile = case.source.video_profile.clone();
            media.width = case.source.width;
            media.height = case.source.height;
            media.bit_depth = case.source.bit_depth;
            media.hdr = case.source.hdr.clone();
            media.hdr_format = case.source.hdr_format.clone();
            media.bitrate = case.source.bitrate;
            assert_eq!(
                caps::decode_limit_identity(&media),
                case.identity,
                "{}",
                case.name
            );
        }

        // The rows are also chosen to prove properties, and a fixture that
        // only regression-pins its own output proves none of them. Each
        // lookup below panics if its row is renamed or dropped, so an
        // assertion cannot end up passing over an empty set — which is
        // exactly what the first version of this test did.
        let by_name = |needle: &str| -> &str {
            fixture
                .cases
                .iter()
                .find(|case| case.name.contains(needle))
                .unwrap_or_else(|| panic!("the fixture has lost its `{needle}` row"))
                .identity
                .as_str()
        };

        // The bucket tolerates jitter inside one 10 Mb/s step and separates
        // the loads either side of it. This is the property that replaced the
        // old `codec@height` key, which condemned every 4K HEVC title on the
        // strength of one 90 Mb/s remux.
        assert_eq!(
            by_name("the 4K Profile 7 remux"),
            by_name("inside one bucket"),
            "one bucket, one key"
        );
        assert_ne!(
            by_name("the 4K Profile 7 remux"),
            by_name("one bucket up"),
            "two buckets, two keys — this is the whole point of the bucket"
        );

        // "Dolby Vision · Profile 7 (HDR10-compatible)" contains both words,
        // and the browser checks Dolby Vision first. Reading them in the
        // other order re-keys the exact label this feature exists for.
        assert!(
            by_name("BOTH dolby vision and hdr10").contains("\"dolby_vision\""),
            "the check order is not a detail: {}",
            by_name("BOTH dolby vision and hdr10")
        );

        // A word boundary, not a substring, and JavaScript's notion of one:
        // `_` is a word character and `中` is not.
        for (row, expected) in [
            ("'advanced' is not DV", "sdr_or_unknown"),
            ("'highlight' is not HLG", "sdr_or_unknown"),
            ("underscores are word characters", "sdr_or_unknown"),
            ("Unicode letter next to the word", "dolby_vision"),
        ] {
            assert!(
                by_name(row).contains(&format!("\"{expected}\"")),
                "{row}: {}",
                by_name(row)
            );
        }

        // Both integer clamps, which exist only because JavaScript cannot
        // represent the values above them.
        assert!(
            by_name("above MAX_SAFE_INTEGER, where JS clamps")
                .contains("9007199254740991,90071992"),
            "{}",
            by_name("above MAX_SAFE_INTEGER, where JS clamps")
        );
        assert!(
            by_name("where the bucket clamps").ends_with(",900719925]"),
            "{}",
            by_name("where the bucket clamps")
        );
    }

    /// Who gets a converted Profile 7 stream, and who does not.
    ///
    /// The whole value of the conversion is the gap between two client
    /// answers: a device that takes Profile 8 but not 7 — which is every
    /// consumer Dolby Vision decoder, because dual-layer never shipped
    /// outside Blu-ray hardware — gets a Profile 7 title as HDR10 today, and
    /// as Dolby Vision after. Every other client is unaffected, and each of
    /// those "unaffected" cases is a separate way to get this wrong.
    #[test]
    fn only_a_client_that_takes_eight_but_not_seven_gets_a_converted_stream() {
        let p7 = || {
            let mut file = file("mkv", "hevc", "aac");
            file.hdr = Some("dolby_vision".to_owned());
            file.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".to_owned());
            // The columns, not only the label: the conversion needs the level
            // and the compatibility id as numbers to build the configuration
            // record its output has to declare.
            file.dolby_vision.profile = Some(7);
            file.dolby_vision.level = Some(6);
            file.dolby_vision.bl_compat_id = Some(1);
            file
        };
        let client = |profiles: Vec<u8>| {
            let mut profile = caps_profile(
                vec!["mkv".into(), "mp4".into()],
                vec!["hevc".into()],
                vec!["aac".into()],
                None,
                true,
                false,
            );
            profile.dolby_vision_profiles = profiles;
            profile
        };
        let node = RenderCaps::proven(true);

        assert!(
            dolby_vision_converts_to_p81(&p7(), &client(vec![5, 8]), &node),
            "Safari and Apple TV: the case this exists for"
        );

        // A client that enumerates 7 takes the stream as it is. Converting
        // would discard an enhancement layer it asked for.
        assert!(!dolby_vision_converts_to_p81(
            &p7(),
            &client(vec![7, 8]),
            &node
        ));
        // A client that takes neither gets the ordinary strip to HDR10.
        assert!(!dolby_vision_converts_to_p81(&p7(), &client(vec![]), &node));
        assert!(!dolby_vision_converts_to_p81(
            &p7(),
            &client(vec![5]),
            &node
        ));

        // An operator can turn it off, and then nobody gets it.
        assert!(!dolby_vision_converts_to_p81(
            &p7(),
            &client(vec![5, 8]),
            &RenderCaps {
                dolby_vision_convert: false,
                ..node
            }
        ));
    }

    /// Every source shape that must not be converted, and why each is its own
    /// refusal rather than one rule.
    #[test]
    fn a_source_without_a_profile_7_compatible_base_is_never_converted() {
        let client = {
            let mut profile = caps_profile(
                vec!["mkv".into()],
                vec!["hevc".into()],
                vec!["aac".into()],
                None,
                true,
                false,
            );
            profile.dolby_vision_profiles = vec![5, 8];
            profile
        };
        let node = RenderCaps::proven(true);
        let source = |hdr: Option<&str>, label: Option<&str>| {
            let mut file = file("mkv", "hevc", "aac");
            file.hdr = hdr.map(str::to_owned);
            file.hdr_format = label.map(str::to_owned);
            file
        };

        for (hdr, label, why) in [
            (None, None, "an SDR title has no Dolby Vision to convert"),
            (Some("hdr10"), Some("HDR10"), "neither does plain HDR10"),
            (
                Some("dolby_vision"),
                Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
                "a Profile 8 title is already what the conversion produces",
            ),
            (
                Some("dolby_vision"),
                Some("Dolby Vision · Profile 5"),
                "Profile 5 has no HDR10-compatible base to keep",
            ),
            (
                Some("dolby_vision"),
                Some("Dolby Vision · Profile 7 (HLG-compatible)"),
                "an HLG base is not an HDR10 one, and there is no HLG rung",
            ),
            (
                Some("dolby_vision"),
                Some("Dolby Vision · Profile 7"),
                "no compatible base named at all: nothing to convert to",
            ),
        ] {
            assert!(
                !dolby_vision_converts_to_p81(&source(hdr, label), &client, &node),
                "{why}"
            );
        }

        // A row whose label says Profile 7 over an HDR10 base and whose
        // columns say nothing is NOT converted, even though every other
        // routing question in this module would answer it from that label.
        //
        // The conversion asks something prose cannot answer: its output has to
        // declare a Dolby Vision configuration record, plurx writes that
        // record itself, and building one needs the level and the
        // compatibility id as numbers. Converting on a label alone would route
        // the session to an index that can never be built — a permanent
        // `vod_index_pending` and a fall through to live recovery on every
        // play. Such a row keeps the strip until a rescan fills the columns
        // in, which is a working delivery rather than a broken one.
        assert!(!dolby_vision_converts_to_p81(
            &source(
                Some("dolby_vision"),
                Some("Dolby Vision · Profile 7 (HDR10-compatible)")
            ),
            &client,
            &node
        ));

        // …and with the columns it converts, so the list above is a set of
        // exclusions rather than a function that always says no.
        let mut described = source(
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
        );
        described.dolby_vision.profile = Some(7);
        described.dolby_vision.level = Some(6);
        described.dolby_vision.bl_compat_id = Some(1);
        assert!(dolby_vision_converts_to_p81(&described, &client, &node));

        // A level column missing on its own is enough to stand the conversion
        // down, because the record cannot be built without it.
        let mut levelless = described.clone();
        levelless.dolby_vision.level = None;
        assert!(!dolby_vision_converts_to_p81(&levelless, &client, &node));

        // …and so is a level the record cannot hold. `DolbyVisionRecord::new`
        // refuses 0 and anything past 0x3f because the field is six bits, so a
        // row outside that range routes to a conversion whose index can never
        // be built — the same permanent `vod_index_pending` a label-only row
        // would produce, from a value that merely looks present. Real levels
        // are 1 to 13; the bounds are asserted rather than the realistic range,
        // because the bound that matters is the record's.
        for level in [0i64, 0x40, 255, i64::MAX] {
            let mut out_of_range = described.clone();
            out_of_range.dolby_vision.level = Some(level);
            assert!(
                !dolby_vision_converts_to_p81(&out_of_range, &client, &node),
                "level {level} cannot be written into a Dolby Vision record"
            );
            assert!(
                crate::fmp4::DolbyVisionRecord::new(
                    8,
                    u8::try_from(level).unwrap_or(0xff),
                    false,
                    true,
                    true,
                    1
                )
                .is_err(),
                "…which is the reason: level {level} is refused by the writer"
            );
        }
        for level in [1i64, 6, 13, 0x3f] {
            let mut in_range = described.clone();
            in_range.dolby_vision.level = Some(level);
            assert!(
                dolby_vision_converts_to_p81(&in_range, &client, &node),
                "level {level} is a level the record can hold"
            );
        }
    }

    /// The decision a converting client actually receives.
    ///
    /// The three flags have to move together or the badge lies: a converted
    /// stream preserves Dolby Vision (the RPUs survive the bitstream filter),
    /// converts it (they are rewritten in the fragments), and delivers
    /// Dolby Vision (which is the whole point — before this, the same client
    /// on the same title was handed the HDR10 base).
    #[test]
    fn a_converting_client_is_told_it_is_getting_dolby_vision() {
        let mut p7 = file("mkv", "hevc", "aac");
        p7.hdr = Some("dolby_vision".into());
        p7.dolby_vision.profile = Some(7);
        p7.dolby_vision.bl_compat_id = Some(1);
        p7.dolby_vision.level = Some(6);

        let client = |profiles: Vec<u8>| {
            let mut profile = caps_profile(
                vec!["mkv".into(), "mp4".into()],
                vec!["hevc".into()],
                vec!["aac".into()],
                None,
                true,
                false,
            );
            profile.dolby_vision_profiles = profiles;
            profile
        };
        let node = RenderCaps::proven(true);

        // Safari and Apple TV: profile 8, not 7.
        let converted = decide(&p7, &client(vec![5, 8]), &node);
        assert!(converted.convert_dolby_vision);
        assert!(
            converted.preserve_dolby_vision,
            "there is nothing to convert in a stream the filter removed"
        );
        assert_eq!(converted.delivered_dynamic_range, "dolby_vision");
        assert_eq!(
            converted.delivered_dolby_vision_profile,
            Some(8),
            "the badge has to be able to say `DV P7 → DV P8`; the range alone \
             cannot, because a preserved P7 answers `dolby_vision` too"
        );
        assert_eq!(
            converted.method,
            PlaybackMethod::Remux,
            "the picture is copied; only the per-frame metadata is rewritten"
        );
        assert!(
            converted
                .reasons
                .iter()
                .any(|reason| reason.contains("Profile 7 converted to Profile 8.1")),
            "{:?}",
            converted.reasons
        );

        // A client that decodes Profile 7 takes the stream untouched:
        // converting would discard an enhancement layer it asked for.
        let native = decide(&p7, &client(vec![7, 8]), &node);
        assert!(!native.convert_dolby_vision);
        assert!(native.preserve_dolby_vision);
        assert_eq!(
            native.delivered_dolby_vision_profile,
            Some(7),
            "the same range as the converted answer, and this is what tells \
             them apart"
        );

        // A client that decodes neither still gets the ordinary strip.
        let stripped = decide(&p7, &client(vec![]), &node);
        assert!(!stripped.convert_dolby_vision);
        assert!(!stripped.preserve_dolby_vision);
        assert_eq!(stripped.delivered_dynamic_range, "hdr10");
        assert_eq!(
            stripped.delivered_dolby_vision_profile, None,
            "a stripped stream carries no Dolby Vision to name"
        );

        // And an operator can turn it off, which puts that client back on the
        // strip rather than on a refusal.
        let off = decide(
            &p7,
            &client(vec![5, 8]),
            &RenderCaps {
                dolby_vision_convert: false,
                ..node
            },
        );
        assert!(!off.convert_dolby_vision);
        assert_eq!(off.delivered_dynamic_range, "hdr10");
        assert_eq!(off.delivered_dolby_vision_profile, None);
    }

    /// A verdict that re-encodes the picture converts nothing.
    ///
    /// The conversion lives inside a copy pipe. Once the video is being
    /// re-encoded there is no copy left for it to sit in, and a decision still
    /// claiming it would put a Dolby Vision badge on a transcode.
    #[test]
    fn a_transcode_never_claims_to_convert() {
        let mut p7 = file("mkv", "hevc", "aac");
        p7.hdr = Some("dolby_vision".into());
        p7.dolby_vision.profile = Some(7);
        p7.dolby_vision.bl_compat_id = Some(1);
        p7.dolby_vision.level = Some(6);
        p7.height = Some(2160);

        // A client that takes profile 8 but caps height below the source:
        // the height check forces a transcode, and the conversion falls away
        // with it.
        let mut profile = caps_profile(
            vec!["mkv".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            Some(1080),
            true,
            false,
        );
        profile.dolby_vision_profiles = vec![5, 8];
        let decision = decide(&p7, &profile, &RenderCaps::proven(true));

        assert_eq!(decision.method, PlaybackMethod::Transcode);
        assert!(!decision.convert_dolby_vision);
        assert!(!decision.preserve_dolby_vision);
        assert_ne!(decision.delivered_dynamic_range, "dolby_vision");
    }

    /// The same exclusions, decided from the stored columns rather than the
    /// label — which is the branch that actually runs.
    ///
    /// M2 populates `dv_profile` and `bl_compat_id` from the container, and
    /// both `dolby_vision_profile` and `has_hdr10_dv_base` prefer them; the
    /// label parse is the fallback for rows scanned before those columns
    /// existed. A suite that only sets label strings therefore leaves the
    /// production path uncovered — widening the compatibility set from `1 | 6`
    /// to `1 | 4 | 6`, which would convert an HLG-based Profile 7 into an
    /// HDR10-labelled 8.1 and get every frame's transfer function wrong,
    /// passes every label-only test there is.
    #[test]
    fn the_conversion_reads_the_stored_columns_not_only_the_label() {
        let client = {
            let mut profile = caps_profile(
                vec!["mkv".into()],
                vec!["hevc".into()],
                vec!["aac".into()],
                None,
                true,
                false,
            );
            profile.dolby_vision_profiles = vec![5, 8];
            profile
        };
        let node = RenderCaps::proven(true);

        // No label at all, so nothing but the columns can answer.
        let source = |dv_profile: i64, compat: i64| {
            let mut file = file("mkv", "hevc", "aac");
            file.hdr = Some("dolby_vision".into());
            file.hdr_format = None;
            file.dolby_vision.profile = Some(dv_profile);
            file.dolby_vision.bl_compat_id = Some(compat);
            file.dolby_vision.level = Some(6);
            file
        };

        // 1 and 6 are the two ids that mean an HDR10 base layer, and an HDR10
        // base layer is what 8.1 is defined as. Both convert.
        for compat in [1, 6] {
            assert!(
                dolby_vision_converts_to_p81(&source(7, compat), &client, &node),
                "compatibility id {compat} is an HDR10 base"
            );
        }

        // Everything else is a different base, and plurx has a rung for none
        // of them: 4 is HLG (whose target would be 8.4), 2 is SDR, 0 is a base
        // no client can watch on its own.
        for (compat, what) in [(4, "HLG"), (2, "SDR"), (0, "none")] {
            assert!(
                !dolby_vision_converts_to_p81(&source(7, compat), &client, &node),
                "a {what} base is not the HDR10 one profile 8.1 promises"
            );
        }

        // And the profile column gates it exactly as the label did.
        assert!(!dolby_vision_converts_to_p81(&source(5, 1), &client, &node));
        assert!(!dolby_vision_converts_to_p81(&source(8, 1), &client, &node));

        // A column and a label that disagree: the column wins, because it came
        // out of the container and the label came out of a scanner's prose.
        let mut contradicted = source(7, 4);
        contradicted.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".into());
        assert!(
            !dolby_vision_converts_to_p81(&contradicted, &client, &node),
            "the stored compatibility id is the fact; the label is the fallback"
        );
    }

    /// The two-entry HEVC ladder that replaces the web's min-of-rungs hack —
    /// and the fallback that decides what an unknown profile gets.
    ///
    /// The hack it replaces (PLAYBACK-CAPS-V2-PLAN §2, edge E5): a browser
    /// that decodes 8-bit HEVC at 4K and Main10 only at 1080p had one number
    /// to send, so it sent 1080 — transcoding every 4K 8-bit title it would
    /// have direct-played. The document says both, and this is where the
    /// saying pays off.
    #[test]
    fn a_per_profile_ceiling_narrows_by_profile_and_falls_back_to_the_narrowest() {
        let caps: caps::DeviceCaps = serde_json::from_str(
            r#"{"v":2,
                "video":[
                  {"codec":"hevc","profiles":["main"],"max_height":2160},
                  {"codec":"hevc","profiles":["main10"],"max_height":1080},
                  {"codec":"h264","max_height":1080}
                ],
                "audio":["aac"],"containers":["mp4"]}"#,
        )
        .expect("the two-entry ladder parses");
        let profile = DeviceProfile::from_caps_v2(&caps);

        // The point of the ladder: 8-bit 4K is admitted…
        assert_eq!(
            profile.codec_height_ceiling("hevc", Some("Main")),
            Some(2160)
        );
        // …and Main10 4K is not, from the same document.
        assert_eq!(
            profile.codec_height_ceiling("hevc", Some("Main 10")),
            Some(1080),
            "ffprobe writes 'Main 10'; the document says 'main10'"
        );
        assert_eq!(
            profile.codec_height_ceiling("hevc", Some("main10")),
            Some(1080)
        );

        // The case that decides whether this is safe: `video_profile` is the
        // raw ffprobe column and is nullable, and ffprobe omits it for plenty
        // of HEVC streams. An unknown profile takes the NARROWEST ceiling the
        // client gave for the codec. Taking the widest would hand this device
        // a 4K Main10 title at 2160 on nothing but a missing column — it
        // plays black, and the file looks fine in the library.
        for unknown in [None, Some("unknown"), Some("Rext")] {
            assert_eq!(
                profile.codec_height_ceiling("hevc", unknown),
                Some(1080),
                "{unknown:?}"
            );
        }

        // A codec with one entry is unaffected: its minimum is its ceiling,
        // profile or no profile.
        assert_eq!(profile.codec_height_ceiling("h264", None), Some(1080));
        assert_eq!(
            profile.codec_height_ceiling("h264", Some("High")),
            Some(1080)
        );
        assert_eq!(profile.codec_height_ceiling("av1", None), None);
    }

    /// A legacy client keeps the ceiling behaviour it has always had.
    ///
    /// `profile_max_heights` is empty for every named profile and for every
    /// `CAPS_Q` translation, so the whole per-profile path has to degenerate
    /// to the old `video_max_heights` lookup. If it does not, this change
    /// re-decides playback for every device on the fleet that never sent a
    /// v2 document.
    #[test]
    fn a_client_with_no_profile_ladder_keeps_the_old_codec_ceiling() {
        let mut profile = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into(), "h264".into()],
            vec!["aac".into()],
            Some(2160),
            true,
            false,
        );
        profile.video_max_heights = [("hevc".to_owned(), 2160), ("h264".to_owned(), 1080)].into();
        assert!(profile.profile_max_heights.is_empty());

        for source_profile in [None, Some("Main 10"), Some("High"), Some("garbage")] {
            assert_eq!(
                profile.codec_height_ceiling("hevc", source_profile),
                Some(2160),
                "{source_profile:?}"
            );
            assert_eq!(
                profile.codec_height_ceiling("h264", source_profile),
                Some(1080),
                "{source_profile:?}"
            );
        }
    }

    /// Every source the HDR10 rung must refuse, and why each one is a
    /// separate refusal rather than one rule.
    #[test]
    fn the_hdr10_rung_is_refused_for_every_source_it_was_not_measured_on() {
        let mut chrome = caps_profile(
            vec!["mp4".into()],
            vec!["h264".into(), "hevc".into()],
            vec!["aac".into()],
            None,
            true,
            false,
        );
        chrome.supports_hdr10_transcode = true;

        // A plain HDR10 source. It carries no RPU, so the Dolby Vision
        // passthrough filter must never see it — that filter emits a broken
        // picture at exit 0 rather than failing. Since M4 it does not have to:
        // a source like this reaches the HDR10 rung by the plain 10-bit route
        // instead, which is what the node's `hdr10_passthrough` proof is for.
        let mut hdr10_source = file("mkv", "hevc", "aac");
        hdr10_source.hdr = Some("hdr10".to_owned());
        assert!(!dolby_vision_needs_rpu_render(&hdr10_source));
        // A browser that cannot decode HEVC at all. It has to re-encode for a
        // reason that has nothing to do with the grade — the case that was
        // silently tone-mapped before M4 — but it must NOT be handed the HDR10
        // rung, whose output is HEVC Main10 and nothing else. `hdr10t=1` is an
        // honest claim from a device with an HDR panel and an H.264-only
        // decoder, and reading it as an HEVC claim is a black screen.
        let mut no_hevc = chrome.clone();
        no_hevc.video_codecs = vec!["h264".into()];
        let d = decide(&hdr10_source, &no_hevc, &RenderCaps::proven(true));
        assert_eq!(d.method, PlaybackMethod::Transcode);
        assert_eq!(d.transcode_grade, OutputGrade::Sdr);
        assert!(
            d.reasons.iter().any(|r| r.contains("does not decode HEVC")),
            "{:?}",
            d.reasons
        );

        // The same source on a client that DOES decode HEVC, forced to
        // re-encode by a height ceiling rather than a codec: this is the case
        // M4 exists for.
        let mut capped = chrome.clone();
        capped.max_height = Some(1080);
        let mut uhd = hdr10_source.clone();
        uhd.width = Some(3840);
        uhd.height = Some(2160);
        let d = decide(&uhd, &capped, &RenderCaps::proven(true));
        assert_eq!(d.method, PlaybackMethod::Transcode);
        assert_eq!(d.transcode_grade, OutputGrade::Hdr10);
        assert_eq!(d.delivered_dynamic_range, "hdr10");
        // And it is still refused on a node that has not proved that encode.
        let unproved = RenderCaps {
            dv_strippable: true,
            dolby_vision_p5_render: true,
            hdr10_passthrough: false,
            hdr10_max_height: HDR10_MAX_MEASURED_HEIGHT,
            dolby_vision_convert: false,
        };
        let d = decide(&uhd, &capped, &unproved);
        assert_eq!(d.transcode_grade, OutputGrade::Sdr);
        assert!(
            d.reasons.iter().any(|r| r.contains("this node")),
            "the demotion has to name which of the three refused: {:?}",
            d.reasons
        );
        // A node that proved the graph but only at a height this delivery can
        // never reach is the same refusal — and it is the one that was
        // missing: a node with no hardware encoder resolves Auto to 720p,
        // where no measured rung exists.
        let short = RenderCaps {
            hdr10_max_height: 720,
            ..RenderCaps::proven(true)
        };
        assert_eq!(
            decide(&uhd, &capped, &short).transcode_grade,
            OutputGrade::Sdr
        );
        // And a source too small to fill the smallest measured rung, on a node
        // that proved everything.
        let mut small = hdr10_source.clone();
        small.width = Some(1280);
        small.height = Some(720);
        assert_eq!(
            decide(&small, &capped, &RenderCaps::proven(true)).transcode_grade,
            OutputGrade::Sdr
        );
        // A client that never proved it presents Main10 PQ is refused too,
        // and the reason names the client rather than the node.
        let mut no_pq = capped.clone();
        no_pq.supports_hdr10_transcode = false;
        let d = decide(&uhd, &no_pq, &RenderCaps::proven(true));
        assert_eq!(d.transcode_grade, OutputGrade::Sdr);
        assert!(
            d.reasons
                .iter()
                .any(|r| r.contains("this client did not prove")),
            "{:?}",
            d.reasons
        );

        // HLG has no rung of its own: re-tagging it as PQ would change the
        // grade rather than keep it, so it tone-maps whatever anyone proved.
        let mut hlg = hdr10_source.clone();
        hlg.hdr = Some("hlg".to_owned());
        assert!(!dolby_vision_needs_rpu_render(&hlg));
        assert_eq!(
            decide(&hlg, &capped, &RenderCaps::proven(true)).transcode_grade,
            OutputGrade::Sdr
        );
        assert!(!dolby_vision_needs_rpu_render(&file("mkv", "hevc", "aac")));

        // Dolby Vision WITH a compatible base: the daemon renders it through
        // zscale or a vendor graph, neither of which reads an RPU. Claiming
        // HDR10 here would be a badge the renderer does not honour.
        let mut p8 = file("mkv", "hevc", "aac");
        p8.hdr = Some("dolby_vision".to_owned());
        p8.hdr_format = Some("Dolby Vision · Profile 8 (HDR10-compatible)".to_owned());
        assert!(!dolby_vision_needs_rpu_render(&p8));
        // With no dovi_rpu this becomes a re-encode. The RPU renderer is still
        // refused — but the base layer decodes to ordinary PQ frames, so since
        // M4 the plain HDR10 rung takes it rather than tone-mapping.
        let d = decide(&p8, &chrome, &RenderCaps::proven(false));
        assert_eq!(d.method, PlaybackMethod::Transcode);
        assert_eq!(d.transcode_grade, OutputGrade::Hdr10);
        assert_eq!(d.delivered_dynamic_range, "hdr10");

        // A Dolby Vision profile the RPU renderer was never built for, with no
        // compatible base to fall back on: nothing can render this as HDR, so
        // it tone-maps however much the client and the node proved.
        let mut p7 = p8.clone();
        p7.hdr_format = Some("Dolby Vision · Profile 7".to_owned());
        assert!(!dolby_vision_needs_rpu_render(&p7));
        let d = decide(&p7, &chrome, &RenderCaps::proven(true));
        assert_eq!(d.transcode_grade, OutputGrade::Sdr);
        // …and one whose profile the scan could not read at all.
        let mut unknown = p8.clone();
        unknown.hdr_format = None;
        assert!(!dolby_vision_needs_rpu_render(&unknown));
        assert_eq!(
            decide(&unknown, &chrome, &RenderCaps::proven(true)).transcode_grade,
            OutputGrade::Sdr
        );

        // Only the one it was measured on.
        let mut p5 = p8;
        p5.hdr_format = Some("Dolby Vision · Profile 5".to_owned());
        assert!(dolby_vision_needs_rpu_render(&p5));
        assert_eq!(
            decide(&p5, &chrome, &RenderCaps::proven(true)).transcode_grade,
            OutputGrade::Hdr10
        );
    }

    /// A grade only exists where something is encoded. A remux and a direct
    /// play carry the source's own bytes, so an HDR10 grade left on either
    /// would be a badge describing an encode that never ran.
    #[test]
    fn a_delivery_that_encodes_nothing_reports_no_grade() {
        let mut p5 = file("mkv", "hevc", "aac");
        p5.hdr = Some("dolby_vision".to_owned());
        p5.hdr_format = Some("Dolby Vision · Profile 5".to_owned());
        let mut client = caps_profile(
            vec!["mkv".into(), "mp4".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            true,
            false,
        );
        client.supports_hdr10_transcode = true;
        client.dolby_vision_profiles = vec![5];

        let direct = decide(&p5, &client, &RenderCaps::proven(true));
        assert_eq!(direct.method, PlaybackMethod::DirectPlay);
        assert_eq!(direct.transcode_grade, OutputGrade::Sdr);
        assert_eq!(direct.delivered_dynamic_range, "dolby_vision");

        // Original never re-encodes video, whatever was asked for.
        let forced = decide_forced(&p5, &client, Force::Original, &RenderCaps::proven(true));
        assert_eq!(forced.transcode_grade, OutputGrade::Sdr);
        // The quality menu's Transcode asks for a smaller stream, not a worse
        // picture: since M4 it negotiates a grade like every other transcode.
        let manual = decide_forced(&p5, &client, Force::Transcode, &RenderCaps::proven(true));
        assert_eq!(manual.method, PlaybackMethod::Transcode);
        assert_eq!(manual.transcode_grade, OutputGrade::Hdr10);
        assert_eq!(manual.delivered_dynamic_range, "hdr10");
        // On a node that never proved the Profile 5 renderer, the same request
        // tone-maps and says whose refusal it was.
        let unproved = RenderCaps {
            dv_strippable: true,
            dolby_vision_p5_render: false,
            hdr10_passthrough: true,
            hdr10_max_height: HDR10_MAX_MEASURED_HEIGHT,
            dolby_vision_convert: false,
        };
        let manual = decide_forced(&p5, &client, Force::Transcode, &unproved);
        assert_eq!(manual.transcode_grade, OutputGrade::Sdr);
        assert_eq!(manual.delivered_dynamic_range, "sdr");
        assert!(manual
            .reasons
            .iter()
            .any(|r| r.contains("this node's ffmpeg")));
    }

    /// The reporter itself, at both grades, on the same file.
    #[test]
    fn the_delivered_range_of_a_transcode_is_its_grade() {
        let mut p5 = file("mkv", "hevc", "aac");
        p5.hdr = Some("dolby_vision".to_owned());
        p5.hdr_format = Some("Dolby Vision · Profile 5".to_owned());
        assert_eq!(
            delivered_dynamic_range(&p5, PlaybackMethod::Transcode, false, OutputGrade::Sdr),
            "sdr"
        );
        assert_eq!(
            delivered_dynamic_range(&p5, PlaybackMethod::Transcode, false, OutputGrade::Hdr10),
            "hdr10"
        );
        // The grade is inert for anything that copies bytes.
        assert_eq!(
            delivered_dynamic_range(&p5, PlaybackMethod::Remux, true, OutputGrade::Hdr10),
            "dolby_vision"
        );
        assert_eq!(
            delivered_dynamic_range(&p5, PlaybackMethod::DirectPlay, false, OutputGrade::Hdr10),
            "hdr10",
            "a stripped Profile 5 copy's over-claim is unchanged by M5"
        );
    }

    #[test]
    fn dolby_vision_profiles_are_negotiated_individually() {
        let mut apple = caps_profile(
            vec!["mp4".into(), "mov".into(), "m4v".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            true,
            false,
        );
        apple.dolby_vision_profiles = vec![5, 8];
        apple.remux_dolby_vision = true;
        let mut android = caps_profile(
            vec![
                "mkv".into(),
                "mp4".into(),
                "webm".into(),
                "mov".into(),
                "ts".into(),
            ],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            true,
            false,
        );
        android.dolby_vision_profiles = vec![5, 8];

        let mut p5 = file("mp4", "hevc", "aac");
        p5.hdr = Some("dolby_vision".to_owned());
        p5.hdr_format = Some("Dolby Vision · Profile 5".to_owned());
        let supported = decide(&p5, &apple, &RenderCaps::proven(true));
        assert_eq!(supported.method, PlaybackMethod::Remux);
        assert!(supported.preserve_dolby_vision);
        assert_eq!(supported.delivered_dynamic_range, "dolby_vision");
        assert!(supported
            .reasons
            .iter()
            .any(|reason| reason.contains("copy-video HLS")));

        let mut p8 = file("mkv", "hevc", "aac");
        p8.hdr = Some("dolby_vision".to_owned());
        p8.hdr_format = Some("Dolby Vision · Profile 8 (HDR10-compatible)".to_owned());
        let apple_p8 = decide(&p8, &apple, &RenderCaps::proven(true));
        assert_eq!(apple_p8.method, PlaybackMethod::Remux);
        assert!(apple_p8.preserve_dolby_vision);
        assert_eq!(apple_p8.delivered_dynamic_range, "dolby_vision");
        let android_p8 = decide(&p8, &android, &RenderCaps::proven(true));
        assert_eq!(android_p8.method, PlaybackMethod::DirectPlay);
        assert!(android_p8.preserve_dolby_vision);
        assert_eq!(android_p8.delivered_dynamic_range, "dolby_vision");

        let mut p7 = file("mp4", "hevc", "aac");
        p7.hdr = Some("dolby_vision".to_owned());
        p7.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".to_owned());
        p7.dolby_vision.profile = Some(7);
        p7.dolby_vision.level = Some(6);
        p7.dolby_vision.bl_compat_id = Some(1);
        // Profile 7 on a client that decodes 8 but not 7. Since M5a this is
        // the conversion rather than the strip: the same client on the same
        // title used to be handed the HDR10 base, and now gets Dolby Vision.
        let converted = decide(&p7, &apple, &RenderCaps::proven(true));
        assert_eq!(converted.method, PlaybackMethod::Remux);
        assert!(converted.convert_dolby_vision);
        assert!(converted.preserve_dolby_vision);
        assert_eq!(converted.delivered_dynamic_range, "dolby_vision");
        assert!(converted
            .reasons
            .iter()
            .all(|reason| !reason.contains("browser")));
        let android_converted = decide(&p7, &android, &RenderCaps::proven(true));
        assert_eq!(android_converted.method, PlaybackMethod::Remux);
        assert!(android_converted.convert_dolby_vision);
        assert_eq!(android_converted.delivered_dynamic_range, "dolby_vision");

        // The pre-M5a answer is still the answer on a node where an operator
        // turned the conversion off. Kept rather than replaced, because it is
        // the behaviour every deployed node had until this milestone and the
        // switch is what an operator uses to get it back.
        let unconverted = RenderCaps {
            dolby_vision_convert: false,
            ..RenderCaps::proven(true)
        };
        let fallback = decide(&p7, &apple, &unconverted);
        assert_eq!(fallback.method, PlaybackMethod::Remux);
        assert!(!fallback.preserve_dolby_vision);
        assert!(!fallback.convert_dolby_vision);
        assert_eq!(fallback.delivered_dynamic_range, "hdr10");
        let android_fallback = decide(&p7, &android, &unconverted);
        assert_eq!(android_fallback.method, PlaybackMethod::Remux);
        assert!(!android_fallback.preserve_dolby_vision);
        assert_eq!(android_fallback.delivered_dynamic_range, "hdr10");

        let sdr = caps_profile(
            vec!["mkv".into(), "mp4".into()],
            vec!["hevc".into(), "h264".into()],
            vec!["aac".into()],
            None,
            false,
            false,
        );
        let sdr_fallback = decide(&p8, &sdr, &RenderCaps::proven(true));
        assert_eq!(sdr_fallback.method, PlaybackMethod::Transcode);
        assert!(!sdr_fallback.preserve_dolby_vision);
        assert_eq!(sdr_fallback.delivered_dynamic_range, "sdr");
    }

    /// A Dolby Vision claim is not allowed to be overruled by `hdr=0`.
    ///
    /// Android computes the two bits from different registries: `hdr=1` needs
    /// `video/hevc` in the RAW `MediaCodecList`, while Dolby Vision is probed
    /// through Media3's decoder selector (the gap PR #370 opened that path
    /// for). A Google TV box can therefore send `dv=1&dvprofile=5&hdr=0` for a
    /// panel and decoder that genuinely play the file — and got an SDR
    /// transcode of it, because `hdr_ok` vetoed the plan even though
    /// `DvHandling` had already answered `None`. Any client with that shape
    /// (Apple, web) would hit the same thing, which is why the guard lives
    /// here rather than in one client's caps builder.
    #[test]
    fn a_dolby_vision_claim_is_not_overruled_by_a_missing_hdr_bit() {
        let mut p5 = file("mp4", "hevc", "aac");
        p5.hdr = Some("dolby_vision".to_owned());
        p5.hdr_format = Some("Dolby Vision · Profile 5".to_owned());

        // dv=1&dvprofile=5, hdr=0 — the box that reported the bug.
        let mut dv_no_hdr_bit = caps_profile(
            vec!["mkv".into(), "mp4".into(), "mov".into(), "ts".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            false, // hdr=0
            false,
        );
        dv_no_hdr_bit.dolby_vision_profiles = vec![5, 8];

        let d = decide(&p5, &dv_no_hdr_bit, &RenderCaps::proven(true));
        assert_ne!(
            d.method,
            PlaybackMethod::Transcode,
            "a client that just claimed DV Profile 5 must not be tone-mapped: {:?}",
            d.reasons
        );
        assert_eq!(d.method, PlaybackMethod::DirectPlay);
        assert!(d.preserve_dolby_vision);
        assert_eq!(d.delivered_dynamic_range, "dolby_vision");
        assert!(
            d.reasons.iter().all(|r| !r.contains("tone-mapping")),
            "{:?}",
            d.reasons
        );

        // Same box, but the delivery envelope it wants is normalized HLS:
        // a copy-video remux, still no re-encode and still Dolby Vision.
        let mut dv_hls = dv_no_hdr_bit.clone();
        dv_hls.remux_dolby_vision = true;
        let remuxed = decide(&p5, &dv_hls, &RenderCaps::proven(true));
        assert_eq!(remuxed.method, PlaybackMethod::Remux);
        assert!(remuxed.preserve_dolby_vision);
        assert_eq!(remuxed.delivered_dynamic_range, "dolby_vision");
    }

    /// The other half of the guard: it excuses a client that claimed THIS
    /// file's Dolby Vision, and nothing else. A genuinely SDR-only client
    /// still gets tone-mapped, so the fix above is not a blanket `hdr_ok`
    /// bypass for every DV source.
    #[test]
    fn a_client_that_never_claimed_dolby_vision_still_tone_maps_it() {
        let mut p5 = file("mp4", "hevc", "aac");
        p5.hdr = Some("dolby_vision".to_owned());
        p5.hdr_format = Some("Dolby Vision · Profile 5".to_owned());

        // No dv bit, no dvprofile, hdr=0 — an SDR panel.
        let sdr_only = caps_profile(
            vec!["mkv".into(), "mp4".into(), "mov".into(), "ts".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            false,
            false,
        );
        let d = decide(&p5, &sdr_only, &RenderCaps::proven(true));
        assert_eq!(d.method, PlaybackMethod::Transcode);
        assert!(!d.preserve_dolby_vision);
        assert_eq!(d.delivered_dynamic_range, "sdr");

        // And a client that probed DV but not THIS profile is equally not
        // excused: Profile 5 has no compatible base to strip to.
        let mut dv_p8_only = sdr_only.clone();
        dv_p8_only.dolby_vision_profiles = vec![8];
        let d = decide(&p5, &dv_p8_only, &RenderCaps::proven(true));
        assert_eq!(d.method, PlaybackMethod::Transcode);
        assert!(!d.preserve_dolby_vision);
        assert_eq!(d.delivered_dynamic_range, "sdr");
    }

    /// Regression pin for the dimension the guard must not touch: a plain
    /// HDR10 source is still judged by `supports_hdr` alone, whatever the
    /// client's Dolby Vision claim says.
    #[test]
    fn plain_hdr10_is_still_decided_by_the_hdr_bit_alone() {
        let mut hdr10 = file("mp4", "hevc", "aac");
        hdr10.hdr = Some("hdr10".to_owned());

        // hdr=1, no DV claim → unchanged: direct play, HDR10 delivered.
        let hdr_no_dv = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            true,
            false,
        );
        let d = decide(&hdr10, &hdr_no_dv, &RenderCaps::proven(true));
        assert_eq!(d.method, PlaybackMethod::DirectPlay);
        assert!(!d.preserve_dolby_vision);
        assert_eq!(d.delivered_dynamic_range, "hdr10");

        // hdr=0 plus a full DV claim does NOT buy an HDR10 file a pass: the
        // excuse is about the file's own dynamic range, not the client's
        // general HDR ambitions.
        let mut dv_no_hdr_bit = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            false,
            false,
        );
        dv_no_hdr_bit.dolby_vision_profiles = vec![5, 8];
        let d = decide(&hdr10, &dv_no_hdr_bit, &RenderCaps::proven(true));
        assert_eq!(d.method, PlaybackMethod::Transcode);
        assert_eq!(d.delivered_dynamic_range, "sdr");
        assert!(
            d.reasons.iter().any(|r| r.contains("tone-mapping")),
            "{:?}",
            d.reasons
        );
    }

    /// The badge's whole reason for existing: a DV disc remux says "DV P7"
    /// while Chrome watches a tone-mapped 1080p SDR encode of it. The
    /// decision already knows which of the three deliveries happened — this
    /// pins that it now says so out loud, per delivery.
    #[test]
    fn a_dolby_vision_plan_reports_the_grade_it_actually_delivers() {
        let mut dv = file("mkv", "hevc", "aac");
        dv.hdr = Some("dolby_vision".to_owned());
        dv.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".to_owned());
        let hdr_client = |dolby: bool| {
            caps_profile(
                vec!["mkv".into(), "mp4".into()],
                vec!["hevc".into(), "h264".into()],
                vec!["aac".into()],
                None,
                true,
                dolby,
            )
        };

        // Safari decodes DV: the file goes over untouched, RPUs and all.
        let safari = decide(&dv, &hdr_client(true), &RenderCaps::proven(true));
        assert_eq!(safari.method, PlaybackMethod::DirectPlay);
        assert_eq!(safari.delivered_dynamic_range, "dolby_vision");

        // Chrome on a server that can strip: the base layer survives, and
        // the base layer is what the compatibility marker names.
        let stripped = decide(&dv, &hdr_client(false), &RenderCaps::proven(true));
        assert_eq!(stripped.method, PlaybackMethod::Remux);
        assert_eq!(
            stripped.delivered_dynamic_range, "hdr10",
            "an HDR10-compatible base delivers HDR10, not Dolby Vision"
        );
        let mut hlg_base = dv.clone();
        hlg_base.hdr_format = Some("Dolby Vision · Profile 8 (HLG-compatible)".to_owned());
        assert_eq!(
            decide(&hlg_base, &hdr_client(false), &RenderCaps::proven(true))
                .delivered_dynamic_range,
            "hlg",
            "and an HLG-compatible base delivers HLG"
        );

        // Chrome on a server that cannot strip: a re-encode, which is H.264
        // 8-bit through the tone-map graph however the source was graded.
        let reencoded = decide(&dv, &hdr_client(false), &RenderCaps::proven(false));
        assert_eq!(reencoded.method, PlaybackMethod::Transcode);
        assert_eq!(reencoded.delivered_dynamic_range, "sdr");
    }

    /// The non-DV half of the truth table. Copied video keeps whatever the
    /// source was graded in; a transcode answers whatever grade the
    /// negotiation reached, and for a client that proved nothing about PQ
    /// that is still SDR.
    ///
    /// The name is left as it was on purpose: it records what this was true of
    /// before M4, and every client in it proves nothing about Main10 PQ, so
    /// every answer below is unchanged.
    #[test]
    fn copied_video_keeps_the_sources_grade_and_a_transcode_never_does() {
        let mut hdr10 = file("mkv", "hevc", "aac"); // MKV → container mismatch
        hdr10.hdr = Some("hdr10".to_owned());
        let hdr_client = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            true,
            false,
        );
        let remuxed = decide(&hdr10, &hdr_client, &RenderCaps::proven(true));
        assert_eq!(remuxed.method, PlaybackMethod::Remux);
        assert_eq!(remuxed.delivered_dynamic_range, "hdr10");

        // Same file, SDR display: the server tone-maps, and says so.
        let sdr_client = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            false,
            false,
        );
        let toned = decide(&hdr10, &sdr_client, &RenderCaps::proven(true));
        assert_eq!(toned.method, PlaybackMethod::Transcode);
        assert_eq!(toned.delivered_dynamic_range, "sdr");

        // An SDR source is SDR wherever it goes — there is no grade to lose.
        let plain = file("mp4", "h264", "aac");
        assert_eq!(
            decide(&plain, default_profile(), &RenderCaps::proven(true)).delivered_dynamic_range,
            "sdr"
        );
        assert_eq!(
            decide(
                &file("mkv", "h264", "ac3"),
                default_profile(),
                &RenderCaps::proven(true)
            )
            .delivered_dynamic_range,
            "sdr"
        );
    }

    /// A manual quality pick is still a delivery, so it still has to answer.
    /// Original honours the strip (base layer kept); Transcode overrides a
    /// perfectly direct-playable DV file, and tone-maps it for a client that
    /// never proved it presents Main10 PQ — which is every client here. Since
    /// M4 that control negotiates a grade like any other transcode rather than
    /// being pinned to SDR; `a_delivery_that_encodes_nothing_reports_no_grade`
    /// covers the client that does prove it.
    #[test]
    fn a_forced_quality_reports_the_grade_that_override_delivers() {
        let mut dv = file("mp4", "hevc", "aac"); // container+audio both fine
        dv.hdr = Some("dolby_vision".to_owned());
        dv.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".to_owned());
        let chrome = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            true,
            false,
        );
        let original = decide_forced(&dv, &chrome, Force::Original, &RenderCaps::proven(true));
        assert_eq!(original.method, PlaybackMethod::Remux);
        assert_eq!(original.delivered_dynamic_range, "hdr10");

        let safari = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            true,
            true,
        );
        assert_eq!(
            decide_forced(&dv, &safari, Force::Original, &RenderCaps::proven(true))
                .delivered_dynamic_range,
            "dolby_vision",
            "Original on a client that decodes DV delivers DV"
        );
        assert_eq!(
            decide_forced(&dv, &safari, Force::Transcode, &RenderCaps::proven(true))
                .delivered_dynamic_range,
            "sdr",
            "and a forced rung tone-maps the same file for a client that never \
             proved it presents Main10 PQ, direct-playable or not"
        );
    }

    #[test]
    fn caps_hdr_flag_gates_tone_mapping() {
        let mut f = file("mp4", "hevc", "aac");
        f.hdr = Some("hdr10".to_owned());
        let sdr = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            false,
            false,
        );
        assert_eq!(
            decide(&f, &sdr, &RenderCaps::proven(true)).method,
            PlaybackMethod::Transcode
        ); // SDR display → tone-map
        let hdr = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            None,
            true,
            false,
        );
        assert_eq!(
            decide(&f, &hdr, &RenderCaps::proven(true)).method,
            PlaybackMethod::DirectPlay
        ); // HDR display → direct
    }

    #[test]
    fn four_k_direct_plays_when_uncapped() {
        let mut f = file("mp4", "h264", "aac");
        f.height = Some(2160);
        // No max_height in caps → a decodable 4K stream direct-plays (browser
        // downscales on a smaller screen).
        let caps = caps_profile(
            vec!["mp4".into()],
            vec!["h264".into()],
            vec!["aac".into()],
            None,
            false,
            false,
        );
        assert_eq!(
            decide(&f, &caps, &RenderCaps::proven(true)).method,
            PlaybackMethod::DirectPlay
        );
    }

    #[test]
    fn runtime_height_ceiling_is_specific_to_the_source_codec() {
        let mut caps = caps_profile(
            vec!["mp4".into()],
            vec!["h264".into(), "hevc".into()],
            vec!["aac".into()],
            None,
            false,
            false,
        );
        caps.video_max_heights.insert("h264".into(), 1080);
        caps.video_max_heights.insert("hevc".into(), 2160);

        let mut h264 = file("mp4", "h264", "aac");
        h264.height = Some(2160);
        let mut hevc = file("mp4", "hevc", "aac");
        hevc.height = Some(2160);

        let rejected = decide(&h264, &caps, &RenderCaps::proven(true));
        assert_eq!(rejected.method, PlaybackMethod::Transcode);
        assert!(rejected
            .reasons
            .iter()
            .any(|reason| reason == "resolution above device maximum"));
        assert_eq!(
            decide(&hevc, &caps, &RenderCaps::proven(true)).method,
            PlaybackMethod::DirectPlay
        );
    }

    #[test]
    fn global_height_ceiling_still_narrows_codec_specific_limit() {
        let mut caps = caps_profile(
            vec!["mp4".into()],
            vec!["hevc".into()],
            vec!["aac".into()],
            Some(1080),
            false,
            false,
        );
        caps.video_max_heights.insert("hevc".into(), 2160);
        let mut hevc = file("mp4", "hevc", "aac");
        hevc.height = Some(2160);

        assert_eq!(
            decide(&hevc, &caps, &RenderCaps::proven(true)).method,
            PlaybackMethod::Transcode
        );
    }

    #[test]
    fn forced_original_never_transcodes_video() {
        // HEVC the browser can't take would auto-transcode; Original forces a
        // copy-video remux instead (client rescues if it truly won't decode).
        let hevc = file("mkv", "hevc", "aac");
        let d = decide_forced(
            &hevc,
            default_profile(),
            Force::Original,
            &RenderCaps::proven(true),
        );
        assert_eq!(d.method, PlaybackMethod::Remux);
        assert!(
            decide(&hevc, default_profile(), &RenderCaps::proven(true)).method
                == PlaybackMethod::Transcode
        );
    }

    #[test]
    fn forced_transcode_overrides_a_direct_playable_file() {
        let mp4 = file("mp4", "h264", "aac");
        assert_eq!(
            decide(&mp4, default_profile(), &RenderCaps::proven(true)).method,
            PlaybackMethod::DirectPlay
        );
        let d = decide_forced(
            &mp4,
            default_profile(),
            Force::Transcode,
            &RenderCaps::proven(true),
        );
        assert_eq!(d.method, PlaybackMethod::Transcode);
    }

    #[test]
    fn forced_auto_matches_plain_decide() {
        let mkv = file("mkv", "h264", "ac3");
        assert_eq!(
            decide_forced(
                &mkv,
                default_profile(),
                Force::Auto,
                &RenderCaps::proven(true)
            )
            .method,
            decide(&mkv, default_profile(), &RenderCaps::proven(true)).method
        );
    }

    #[test]
    fn a_disc_remux_wants_segments_without_any_storage_measurement() {
        let why = prefer_segmented(Some(69_000_000)).expect("probed remux");
        assert!(why.contains("69 Mb/s"), "{why}");
    }

    #[test]
    fn every_probed_remux_has_a_stable_segmented_reason() {
        // Average source rate cannot predict a Wi-Fi, scheduler, or network
        // filesystem gap. Pin both the widened route and its viewer-facing
        // explanation; a partial restoration of the old 40 Mb/s gate fails.
        assert_eq!(
            prefer_segmented(Some(25_000_000)).as_deref(),
            Some(
                "25 Mb/s remux — progressive fMP4 buffers about 2.2 s; HLS allows deeper read-ahead"
            )
        );
        assert!(prefer_segmented(Some(10_000_000)).is_some());
    }

    #[test]
    fn nothing_is_claimed_about_a_file_that_was_never_probed() {
        // No bitrate means no measurement, not a small file. Guessing one from
        // resolution would reroute a library on an inference.
        assert_eq!(prefer_segmented(None), None);
        assert_eq!(prefer_segmented(Some(0)), None);
        assert!(prefer_segmented(Some(20_000_000)).is_some());
        assert!(prefer_segmented(Some(69_000_000)).is_some());
    }
}
