//! What a client can actually play, as one document.
//!
//! Companion to [`super::decide`] (what the server does with it) — this is
//! *what the client said*, and the one place two wire shapes become one
//! [`DeviceProfile`].
//!
//! The shape this replaces grew a key at a time, and each key brought its own
//! absent-means-what rule (PLAYBACK-CAPS-V2-PLAN §2, edge E5). `dv=1` meant
//! "every Dolby Vision profile" only when `dvprofile` was *absent*; `hdr10t`
//! was a bespoke flag for one rung; `maxheight` was codec-agnostic, so a
//! browser that decodes 8-bit 4K but Main10 only at 1080p had to send the
//! minimum of the two and pay a needless transcode on every 4K 8-bit title.
//!
//! Two rules hold everywhere in here:
//!
//! - **Absent is never a claim.** A field the client did not send means "not
//!   claimed", never "unknown, assume yes". The failure mode of the optimistic
//!   reading is a stream a device cannot decode, which is a black screen; the
//!   failure mode of the pessimistic one is a transcode, which plays.
//! - **The legacy query keeps its exact meaning.** `from_legacy_query` is not
//!   a tidier re-reading of the old flags; it reproduces them, blanket
//!   Dolby Vision claim and all, so the same client gets the same answer
//!   before and after it learns the new shape. The deprecation is enforced by
//!   clients no longer sending the old form, not by the server refusing it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use super::DeviceProfile;

/// A transfer function a client can *present* — decode and put on the
/// attached display as the grade it is, rather than merely accept.
///
/// This is what the `hdr` and `hdr10t` flags were each half of. `Pq` covers
/// HDR10 and HDR10+, which differ only in dynamic metadata the display reads;
/// `Hlg` is a different curve and a different answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transfer {
    Sdr,
    Pq,
    Hlg,
}

/// One decodable video codec, and the ceilings that apply to it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VideoCaps {
    /// Normalized codec name: `h264`, `hevc`, `av1`, `vp9`.
    pub codec: String,
    /// Codec profiles the decoder proved at `max_height` — `main`, `main10`.
    ///
    /// A 10-bit ceiling lower than the 8-bit one is expressed as **two
    /// entries for the same codec** with different heights, which is exactly
    /// what the web's min-of-rungs hack was working around.
    #[serde(default)]
    pub profiles: Vec<String>,
    /// The tallest frame this codec (at these profiles) decodes.
    #[serde(default)]
    pub max_height: Option<i64>,
    #[serde(default)]
    pub max_bitrate_bps: Option<i64>,
    /// Transfer functions this codec's output can be presented as.
    #[serde(default)]
    pub present: Vec<Transfer>,
    /// Dolby Vision profiles this decoder takes, exhaustively. `[]` means
    /// none — there is no blanket claim in this document, by design.
    ///
    /// `7` belongs here only when a decoder enumeration says so (Android's
    /// `MediaCodec` reporting `DolbyVisionProfileDvheDtb`), never from a
    /// display bit and never from a guess: a wrong claim here is a black
    /// screen rather than a downgrade.
    #[serde(default)]
    pub dv_profiles: Vec<u8>,
}

/// Facts about the attached output, as distinct from what decodes.
///
/// A display that shows HDR and a decoder that emits it are different
/// questions, and conflating them is how a PQ stream reaches an SDR panel and
/// renders grey — which plays, so nobody reports it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DisplayCaps {
    #[serde(default)]
    pub hdr: bool,
    #[serde(default)]
    pub dolby_vision: bool,
    #[serde(default)]
    pub max_nits: Option<i64>,
}

/// Which client build sent this. Diagnostic only — it never influences a
/// decision, and it is what makes a fleet-wide "who is still on the old
/// shape" answerable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientInfo {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub build: String,
    #[serde(default)]
    pub ua: String,
}

/// A decode limit a client learned the hard way and now applies itself.
///
/// The web stores these in `localStorage` after a stuttery session and routes
/// every matching title to a forced transcode for the next month — invisibly
/// to the server, which then cannot explain its own answer. Carried verbatim
/// so the server's reason can be the browser's own words.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LearnedLimit {
    /// The client's own identity string for the source shape it gave up on.
    pub identity: String,
    /// A human label for the same, as the client would word it.
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub lost: i64,
    #[serde(default)]
    pub secs: i64,
    #[serde(default)]
    pub rate: i64,
    #[serde(default)]
    pub at_ms: i64,
}

/// Everything a client claims it can play.
///
/// Posted as the body of `POST /api/v1/files/:id/decision`, and again as
/// `caps` on session create so the server can re-derive rather than trust an
/// echo.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceCaps {
    /// Document version. `2` is this shape; anything else is refused rather
    /// than guessed at.
    #[serde(default)]
    pub v: u8,
    #[serde(default)]
    pub client: Option<ClientInfo>,
    #[serde(default)]
    pub video: Vec<VideoCaps>,
    #[serde(default)]
    pub audio: Vec<String>,
    #[serde(default)]
    pub containers: Vec<String>,
    #[serde(default)]
    pub transports: Vec<String>,
    /// `hls` when preserved Dolby Vision has to ride the copy-video HLS
    /// envelope rather than a progressive MP4. Apple's AVPlayer can report a
    /// healthy Profile 8 pipeline, advance the raw file's timeline, and still
    /// render black.
    #[serde(default)]
    pub dv_transport: Option<String>,
    #[serde(default)]
    pub display: Option<DisplayCaps>,
    #[serde(default)]
    pub learned_limits: Vec<LearnedLimit>,
    /// An all-profiles Dolby Vision claim, which **only** the legacy
    /// translation sets.
    ///
    /// `dv=1` with no `dvprofile` used to mean "every profile, including the
    /// dual-layer ones no consumer decoder takes". Safari sent exactly that
    /// until 2026-07-31 and got Profile 7 preserved, which only ever changed
    /// the badge — it never rendered. The rule survives here so a client that
    /// has not learned the new shape gets byte-for-byte the answer it got
    /// yesterday, and it is deliberately unspellable in the v2 JSON: a client
    /// that cannot enumerate profiles claims none.
    #[serde(skip)]
    pub legacy_blanket_dolby_vision: bool,
    /// The codec-agnostic ceiling the legacy `maxheight` carried.
    ///
    /// v2 expresses ceilings per codec, but the translation needs somewhere
    /// to put a global one, and a client that sends both should get both
    /// applied — `min`, as `evaluate` has always done.
    #[serde(default)]
    pub max_height: Option<i64>,
}

/// The legacy `CAPS_Q` query, as far as the profile is concerned.
///
/// Lives here rather than in the HTTP layer so both wire shapes reach
/// [`DeviceProfile`] through one function. The daemon fills it from its own
/// query type; nothing else knows the flat form exists.
#[derive(Debug, Clone, Default)]
pub struct LegacyCaps {
    pub containers: Vec<String>,
    pub video_codecs: Vec<String>,
    pub audio_codecs: Vec<String>,
    pub max_height: Option<i64>,
    pub codec_max_heights: HashMap<String, i64>,
    pub hdr: bool,
    pub dv: bool,
    pub dv_profiles: Vec<u8>,
    pub dv_profiles_sent: bool,
    pub dvhls: bool,
    pub hdr10t: bool,
}

impl DeviceCaps {
    /// The version this server understands.
    pub const VERSION: u8 = 2;

    /// Rebuild the v2 document from the flat query a legacy client sends.
    ///
    /// Every mapping here is a restatement of an existing rule, not a
    /// reinterpretation of one:
    ///
    /// - `hdr=1` is a **display** fact, so it lands on `display.hdr` rather
    ///   than on any codec's `present`. It never meant "this decoder emits
    ///   PQ"; that is what `hdr10t` meant, and only for HEVC.
    /// - `hdr10t=1` therefore adds `Pq` to HEVC alone. Absent means not
    ///   proven, which is the field's whole contract.
    /// - `dv=1` with no `dvprofile` sets the blanket flag; with `dvprofile`
    ///   it is ignored, exactly as `stream.rs` has always read it.
    /// - `maxheight` stays a global ceiling and `vmaxheight` a per-codec one,
    ///   because `evaluate` takes the `min` of the two and a client that sent
    ///   both meant both.
    pub fn from_legacy_query(legacy: &LegacyCaps) -> Self {
        let video = legacy
            .video_codecs
            .iter()
            .map(|codec| {
                let codec = codec.trim().to_ascii_lowercase();
                let mut present = vec![Transfer::Sdr];
                if legacy.hdr10t && codec == "hevc" {
                    present.push(Transfer::Pq);
                }
                VideoCaps {
                    max_height: legacy.codec_max_heights.get(&codec).copied(),
                    present,
                    dv_profiles: legacy.dv_profiles.clone(),
                    codec,
                    ..VideoCaps::default()
                }
            })
            .collect();
        Self {
            v: Self::VERSION,
            client: None,
            video,
            audio: legacy.audio_codecs.clone(),
            containers: legacy.containers.clone(),
            transports: Vec::new(),
            dv_transport: legacy.dvhls.then(|| "hls".to_owned()),
            display: Some(DisplayCaps {
                hdr: legacy.hdr,
                dolby_vision: legacy.dv || !legacy.dv_profiles.is_empty(),
                max_nits: None,
            }),
            learned_limits: Vec::new(),
            legacy_blanket_dolby_vision: legacy.dv && !legacy.dv_profiles_sent,
            max_height: legacy.max_height,
        }
    }

    /// True when this document claims nothing at all — the shape a client
    /// that sent only a named profile produces, which must keep taking the
    /// named-profile path rather than being read as "decodes nothing".
    pub fn is_empty(&self) -> bool {
        self.video.is_empty() && self.audio.is_empty() && self.containers.is_empty()
    }
}

impl DeviceProfile {
    /// One capabilities document, one device profile.
    ///
    /// The point of routing both wire shapes through here is that they cannot
    /// answer differently: a client that upgrades from the flat query to the
    /// v2 document must get the same verdict for the same hardware, and the
    /// only way to be sure of that is for there to be one translation.
    ///
    /// Empty collections keep the defaults the flat form applied, because a
    /// client that names no containers is one that has not been taught to,
    /// not one that plays nothing.
    pub fn from_caps_v2(caps: &DeviceCaps) -> Self {
        let containers = if caps.containers.is_empty() {
            vec!["mp4".into(), "webm".into(), "mov".into()]
        } else {
            caps.containers.clone()
        };
        let audio_codecs = if caps.audio.is_empty() {
            vec!["aac".into(), "mp3".into()]
        } else {
            caps.audio.clone()
        };
        let mut video_codecs: Vec<String> = Vec::new();
        let mut video_max_heights: HashMap<String, i64> = HashMap::new();
        let mut profile_max_heights: BTreeMap<(String, String), i64> = BTreeMap::new();
        let mut presents: BTreeMap<String, BTreeSet<Transfer>> = BTreeMap::new();
        let mut dolby_vision_profiles: Vec<u8> = Vec::new();
        let mut max_bitrate: Option<i64> = None;

        for entry in &caps.video {
            let codec = entry.codec.trim().to_ascii_lowercase();
            if codec.is_empty() {
                continue;
            }
            if !video_codecs.contains(&codec) {
                video_codecs.push(codec.clone());
            }
            if let Some(height) = entry.max_height.filter(|height| *height > 0) {
                // Several entries for one codec are a ceiling ladder, not a
                // contradiction: the codec's own ceiling is the tallest any
                // of its entries reached, and the narrower ones are recorded
                // per profile below.
                let codec_ceiling = video_max_heights.entry(codec.clone()).or_insert(height);
                *codec_ceiling = (*codec_ceiling).max(height);
                for profile in &entry.profiles {
                    let profile = profile.trim().to_ascii_lowercase();
                    if profile.is_empty() {
                        continue;
                    }
                    let slot = profile_max_heights
                        .entry((codec.clone(), profile))
                        .or_insert(height);
                    *slot = (*slot).max(height);
                }
            }
            if let Some(bitrate) = entry.max_bitrate_bps.filter(|bitrate| *bitrate > 0) {
                max_bitrate =
                    Some(max_bitrate.map_or(bitrate, |current: i64| current.max(bitrate)));
            }
            let slot = presents.entry(codec).or_default();
            for transfer in &entry.present {
                slot.insert(*transfer);
            }
            for profile in &entry.dv_profiles {
                if !dolby_vision_profiles.contains(profile) {
                    dolby_vision_profiles.push(*profile);
                }
            }
        }
        if video_codecs.is_empty() {
            video_codecs.push("h264".into());
        }
        dolby_vision_profiles.sort_unstable();

        let display = caps.display.clone().unwrap_or_default();
        // The display bit OR a codec that presents an HDR curve. Either is a
        // client saying it can show HDR at all; `supports_hdr10_transcode`
        // below is the narrower claim about Main10 PQ specifically.
        let presents_hdr = presents
            .values()
            .any(|set| set.contains(&Transfer::Pq) || set.contains(&Transfer::Hlg));
        let supports_hdr = display.hdr || presents_hdr;
        let supports_hdr10_transcode = presents
            .get("hevc")
            .is_some_and(|set| set.contains(&Transfer::Pq));

        DeviceProfile {
            name: "client-caps".to_owned(),
            description: "runtime-probed client capabilities".to_owned(),
            containers,
            video_codecs,
            audio_codecs,
            max_height: caps.max_height,
            video_max_heights,
            max_bitrate,
            supports_hdr,
            supports_dolby_vision: caps.legacy_blanket_dolby_vision,
            dolby_vision_profiles,
            supports_hdr10_transcode,
            remux_dolby_vision: caps.dv_transport.as_deref() == Some("hls"),
            presents,
            profile_max_heights,
            learned_limits: caps.learned_limits.clone(),
        }
    }
}
