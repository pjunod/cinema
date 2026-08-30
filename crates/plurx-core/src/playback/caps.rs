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
    /// A curve this server has not heard of.
    ///
    /// The alternative is that one unrecognised token — a future `hlg10`, a
    /// typo, a v2.1 client — fails the whole document, which fails the whole
    /// create, which is no playback at all. That is exactly backwards for a
    /// design whose rule is that an unknown claim is simply not a claim: this
    /// variant matches nothing, grades nothing, and costs a set entry.
    #[serde(other)]
    Unknown,
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
    ///
    /// Defaulted like every other field here. The web stores these entries in
    /// a map keyed BY the identity (PLAYBACK-CAPS-V2-PLAN §4.6), so the
    /// obvious client serialization — the map's values — omits it. A
    /// diagnostic field must never be the reason a create is refused.
    #[serde(default)]
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
    /// Spelled `at` by the web (§4.6); accepted under both names so the
    /// timestamp does not silently read zero.
    #[serde(default, alias = "at")]
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
    ///   both meant both. A `vmaxheight` entry for a codec absent from
    ///   `vcodec` is dropped rather than carried: the v2 shape has nowhere to
    ///   put a ceiling for a codec the client does not decode, and it cannot
    ///   change a verdict that `video_ok` has already refused on the codec.
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
        // Membership is a set lookup rather than a scan of `video_codecs`.
        // The document arrives from the network with no per-field bound, and
        // a linear scan per entry is quadratic in a body an authenticated
        // caller chooses the size of — seconds of a pinned worker thread for
        // one request.
        let mut seen_codecs: std::collections::HashSet<String> = std::collections::HashSet::new();
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
            if seen_codecs.insert(codec.clone()) {
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

        // The display, when the client described one, is the authority — and
        // it has to be, in both directions. A decoder that emits PQ into an
        // SDR panel shows grey, and a panel that shows HDR through a decoder
        // that cannot is not an HDR path either. Only a document with no
        // `display` block falls back to what its codecs present.
        //
        // This is also what keeps the legacy translation byte-identical:
        // `from_legacy_query` always emits a display block, so `hdr=1` alone
        // decides `supports_hdr` exactly as it did before caps v2 existed —
        // and `hdr10t=1&hdr=0`, which the out-of-tree clients can produce
        // because they compute the two bits from different sources, keeps
        // tone-mapping to SDR rather than newly direct-playing PQ.
        let supports_hdr = match caps.display.as_ref() {
            Some(display) => display.hdr,
            None => presents
                .values()
                .any(|set| set.contains(&Transfer::Pq) || set.contains(&Transfer::Hlg)),
        };
        // Deliberately narrower than the flat `hdr10t` bit it replaces: the
        // claim is "decodes HEVC Main10 and presents it as PQ", and a client
        // that did not list `hevc` at all has not made it. The old code stored
        // `hdr10t=1` from such a client and `target_grade` then refused the
        // rung anyway on the codec check, so the verdict is unchanged; only
        // the reason string moves, and it moves to the accurate one.
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
            learned_limits: caps
                .learned_limits
                .iter()
                .take(MAX_LEARNED_LIMITS)
                .map(LearnedLimit::bounded)
                .collect(),
        }
    }
}

/// The bucket width the browser groups source bitrates into, in bits per
/// second (`DECODE_LIMIT_DEFAULTS.bitrateBucketBps`).
const DECODE_LIMIT_BITRATE_BUCKET_BPS: i64 = 10_000_000;

/// The identity schema the browser stamps into every key it writes
/// (`DECODE_LIMIT_DEFAULTS.schema`). Bumping it in one place and not the
/// other silently stops every learned limit from matching, which looks like
/// the feature quietly not working rather than like a version skew — so the
/// shared fixture pins both spellings together.
const DECODE_LIMIT_SCHEMA: &str = "v2";

/// JavaScript's `Number.MAX_SAFE_INTEGER`, which `positiveInteger` and
/// `bitrateBucket` both clamp to.
///
/// Reproduced because the browser cannot represent an integer above it and
/// therefore writes the clamp into its key. A server that did not clamp would
/// key every such source one apart from the browser — for values no real
/// probe produces, which is exactly the kind of divergence that survives
/// review and then surfaces years later on one weird file.
const JS_MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// The whitespace class JavaScript's `String.prototype.trim` and `\s` use.
///
/// It is *not* Rust's `char::is_whitespace`, and the difference is two
/// codepoints in opposite directions: U+FEFF (zero-width no-break space) is
/// whitespace to JS and not to Rust, and U+0085 (NEL) is whitespace to Rust
/// and not to JS. Both appear in text scraped out of container metadata.
fn js_whitespace(character: char) -> bool {
    character == '\u{feff}' || (character.is_whitespace() && character != '\u{85}')
}

/// A JavaScript regex `\b` word character: ASCII letters, digits, underscore.
///
/// Rust's `char::is_alphanumeric` is the Unicode answer and therefore the
/// wrong one here. Using it makes `"HDR_DV_HLG"` key as Dolby Vision where
/// the browser reads no word boundary at all, and makes `"dv中"` key as
/// nothing where the browser reads one.
fn js_word_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// `\bword\b` as JavaScript matches it, against an already-normalized string.
fn js_word_match(haystack: &str, word: &str) -> bool {
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(word) {
        let start = from + offset;
        let end = start + word.len();
        let before_is_word = haystack[..start]
            .chars()
            .next_back()
            .is_some_and(js_word_char);
        let after_is_word = haystack[end..].chars().next().is_some_and(js_word_char);
        if !before_is_word && !after_is_word {
            return true;
        }
        from = start + word.chars().next().map_or(1, char::len_utf8);
        if from >= haystack.len() {
            break;
        }
    }
    false
}

/// The browser's `decodeLimitIdentity(source)`, in Rust.
///
/// This is a deliberate second implementation of a function that already
/// exists in `web/playback-policy.js`, and the reason is that the browser
/// computes the key from the source it is *looking at* while the server has
/// to compute it from the source row it is *deciding about*. They have to
/// agree byte for byte or nothing matches, and nothing matching is invisible:
/// the limit is simply never applied and the viewer keeps stuttering.
///
/// `tests/playback/decode-limit-identity.json` is the contract between the
/// two. Both this function and the browser's are run against every row in it,
/// so a change to either spelling fails in both languages at once — and the
/// rows deliberately include the shapes where two implementations of one
/// function drift: the label that contains *both* "dolby vision" and "hdr10",
/// non-ASCII case folding, the two whitespace codepoints the languages
/// disagree about, underscores against word boundaries, and both integer
/// clamps.
pub fn decode_limit_identity(file: &crate::domain::MediaFile) -> String {
    fn text(value: Option<&str>, fallback: &str) -> String {
        // `String(value).trim().toLowerCase().replace(/\s+/g, " ")`, in that
        // order, with JavaScript's whitespace class throughout.
        let normalized: String = value
            .map(|value| {
                let trimmed = value.trim_matches(js_whitespace).to_lowercase();
                let mut out = String::with_capacity(trimmed.len());
                let mut in_space = false;
                for character in trimmed.chars() {
                    if js_whitespace(character) {
                        if !in_space {
                            out.push(' ');
                            in_space = true;
                        }
                    } else {
                        out.push(character);
                        in_space = false;
                    }
                }
                out
            })
            .unwrap_or_default();
        if normalized.is_empty() {
            fallback.to_owned()
        } else {
            normalized
        }
    }
    // `positiveInteger`: anything not strictly positive is zero, and anything
    // JavaScript could not have represented exactly is clamped where it
    // clamps.
    fn positive(value: Option<i64>) -> i64 {
        value
            .filter(|value| *value > 0)
            .map(|value| value.min(JS_MAX_SAFE_INTEGER))
            .unwrap_or(0)
    }

    let hdr_format = file.hdr_format.as_deref();
    // `sourceDynamicRange`: the declared range wins; otherwise the rich label
    // is read for the words the browser reads, **in the browser's order**.
    // "Dolby Vision · Profile 7 (HDR10-compatible)" contains both, and it is
    // the exact label this whole feature exists for, so the order is not a
    // detail.
    let dynamic_range = {
        let declared = text(file.hdr.as_deref(), "");
        if !declared.is_empty() {
            declared
        } else {
            let rich = text(hdr_format, "");
            if rich.contains("dolby vision") || js_word_match(&rich, "dv") {
                "dolby_vision".to_owned()
            } else if rich.contains("hdr10") {
                "hdr10".to_owned()
            } else if js_word_match(&rich, "hlg") {
                "hlg".to_owned()
            } else {
                "sdr_or_unknown".to_owned()
            }
        }
    };
    // `bitrateBucket`: `"unknown"` rather than a number when there is no
    // positive bitrate to bucket — a string where the other rows are integers,
    // which is exactly what the browser writes.
    let bucket = match file.bitrate.filter(|bitrate| *bitrate > 0) {
        Some(bitrate) => serde_json::Value::from(
            bitrate
                .min(JS_MAX_SAFE_INTEGER)
                .div_euclid(DECODE_LIMIT_BITRATE_BUCKET_BPS),
        ),
        None => serde_json::Value::from("unknown"),
    };

    let fields = serde_json::Value::Array(vec![
        text(file.video_codec.as_deref(), "unknown").into(),
        text(file.video_profile.as_deref(), "unknown").into(),
        positive(file.width).into(),
        positive(file.height).into(),
        positive(file.bit_depth).into(),
        dynamic_range.into(),
        text(hdr_format, "none").into(),
        bucket,
    ]);
    format!("decode-{DECODE_LIMIT_SCHEMA}:{fields}")
}

/// How many learned limits a client may have applied to it in one decision.
///
/// The browser's own map is bounded by how many distinct media loads one
/// person stutters through in a month; this is a bound on what an
/// authenticated caller may *send*, which is a different question. The body
/// limit already caps the bytes — this caps what gets cloned into a profile
/// and scanned once per decision, and it is far above any real client.
const MAX_LEARNED_LIMITS: usize = 256;

/// How long a client-supplied label may be before it stops being a label.
///
/// It is reprinted verbatim into a reason string that reaches the badge, the
/// stats overlay, the admin session list and the server log. The browser
/// builds these from `mediaLoadLabel`, which is under a hundred characters;
/// a caller that sends sixty thousand is not describing a media load.
const MAX_LEARNED_LIMIT_TEXT: usize = 160;

/// How long the browser keeps a learned limit before discarding it entirely
/// (`DECODE_LIMIT_DEFAULTS.ttlMs`): 30 days.
pub const LEARNED_LIMIT_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1_000;

/// How long the browser applies a learned limit before it insists on
/// re-measuring instead (`DECODE_LIMIT_DEFAULTS.retestMs`): 7 days.
pub const LEARNED_LIMIT_RETEST_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

impl LearnedLimit {
    /// The same entry with its two free-text fields made safe to reprint.
    ///
    /// Control characters go first, then the length. The order matters: this
    /// text ends up in a log line, and a newline inside it is how one log
    /// line becomes two forged ones. The length cap is second because a
    /// truncated label is still a useful label, while an unbounded one is a
    /// client choosing how much of the server's log and every `/decision`
    /// response it gets to occupy.
    fn bounded(&self) -> Self {
        fn clean(value: &str) -> String {
            let cleaned: String = value
                .chars()
                .filter(|character| !character.is_control())
                .take(MAX_LEARNED_LIMIT_TEXT)
                .collect();
            cleaned.trim().to_owned()
        }
        Self {
            identity: clean(&self.identity),
            label: clean(&self.label),
            ..*self
        }
    }

    /// Whether this entry may steer a decision *right now*.
    ///
    /// The browser's own policy, reproduced, and reproducing it is not
    /// optional: `learnedDecodeLimitAction` returns `"retest"` rather than
    /// `"apply"` once an entry is older than `retestMs`, and
    /// `pruneDecodeLimits` drops it outright past `ttlMs` or when its shape is
    /// junk. A server that matched on identity alone would be **stricter than
    /// the client that owns the policy** for the whole 23-day window between
    /// those two ages — honouring limits the browser had already decided to
    /// stop trusting.
    ///
    /// It is also the only thing that keeps the re-test loop alive. The
    /// browser only consults its own limits when the server answered
    /// `direct_play` or `remux`; once the server starts answering `transcode`
    /// first, the weekly one-session re-measure — the fix for limits that
    /// were "in practice permanent" — never runs again unless the server
    /// stands aside exactly when the browser would have. So an expired entry
    /// is not merely ignored here: ignoring it is what hands the decision
    /// back to the client that can actually re-measure it.
    ///
    /// A `now_ms` of zero or less means the caller has no clock, and an
    /// unknown age is not evidence of freshness: nothing applies.
    pub fn applies_at(&self, now_ms: i64) -> bool {
        if now_ms <= 0 || self.at_ms <= 0 {
            return false;
        }
        let age = now_ms - self.at_ms;
        // A negative age is a clock disagreement, not a very fresh entry.
        (0..=LEARNED_LIMIT_RETEST_MS).contains(&age)
    }
}

impl LearnedLimit {
    /// The browser's own sentence for this demotion
    /// (`learnedDecodeLimitView().reason`, `playback-policy.js:969-995`).
    ///
    /// Reproduced rather than paraphrased so the badge, the stats overlay and
    /// the admin's session list all say what the browser would have said on
    /// its own. Two wordings for one decision is how a viewer ends up
    /// reconciling the server against their own client.
    ///
    /// `lost_range` is the grade the ordinary decision would have delivered,
    /// when this demotion is what costs it — the browser appends that
    /// consequence and so must this. `None` when nothing was lost.
    ///
    /// The measurement degrades exactly where the browser's does: the browser
    /// gates on the rate being a finite number, **not** on it being positive,
    /// and `pruneDecodeLimits` deliberately keeps a zero-rate entry. Gating
    /// on `> 0` here would print "measured unstable original playback" over a
    /// measurement the browser prints in full.
    pub fn reason(&self, lost_range: Option<&str>) -> String {
        let label = if self.label.trim().is_empty() {
            // The browser always has a label; a client that sent none is
            // showing its identity instead, which is ugly and still better
            // than a sentence with a hole where the subject goes.
            self.identity.trim()
        } else {
            self.label.trim()
        };
        let label = if label.is_empty() {
            "this media load"
        } else {
            label
        };
        let measurement = format!(
            "lost {} frames in {}s ({}/min)",
            self.lost, self.secs, self.rate
        );
        let consequence = match lost_range {
            Some(range) => format!("; {} \u{2192} SDR", Self::range_label(range)),
            None => String::new(),
        };
        format!("learned client-performance limit for {label}: {measurement}{consequence}")
    }

    /// `dynamicRangeLabel`, for the handful of values that reach it.
    fn range_label(range: &str) -> &str {
        match range {
            "dolby_vision" => "Dolby Vision",
            "hdr10" => "HDR10",
            "hlg" => "HLG",
            other => other,
        }
    }
}
