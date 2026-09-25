use super::*;

fn audio_track(
    file: &plurx_core::domain::MediaFile,
    selected: Option<i64>,
) -> Option<&plurx_core::domain::AudioStream> {
    selected
        .and_then(|index| file.audio_streams.iter().find(|track| track.index == index))
        .or_else(|| file.audio_streams.iter().find(|track| track.default))
        .or_else(|| file.audio_streams.first())
}

/// RFC 6381 sample type for audio that is being *copied* into the HLS
/// rendition.
///
/// The `_` arm answers `mp4a.40.2` (AAC-LC) for anything unlisted, which
/// **mislabels a genuinely copied FLAC, Opus or DTS track** — it claims AAC
/// for bytes that are not AAC. The Apple client only ever claims codecs the
/// arms above cover, but the web player claims `flac`/`opus` when the browser
/// does, and Safari takes the copy-HLS path — so a FLAC-in-MKV remux reaches
/// this arm today. Recorded rather than fixed
/// (CLIENTS-REMEDIATION-PLAN §8.3) because the value has no consumer at all:
/// the native master carries no `CODECS` attribute (see [`HlsContext`]), so
/// the wrong label is currently written to nothing. It is one match arm, and
/// it wants doing in the same change as whatever starts reading the result —
/// a fix landed now would be untestable through any wire output.
fn copied_audio_codec(file: &plurx_core::domain::MediaFile, selected: Option<i64>) -> &'static str {
    match audio_track(file, selected).map(|track| track.codec.as_str()) {
        Some("ac3" | "ac-3") => "ac-3",
        Some("eac3" | "eac-3" | "ec-3") => "ec-3",
        Some("alac") => "alac",
        Some("mp3") => "mp4a.40.34",
        _ => "mp4a.40.2",
    }
}

/// What a copy with no post-mux converter can actually deliver, given what
/// the decision asked for.
///
/// The takeover muxer and the structural legacy fallback run one ffmpeg and
/// have nowhere to put the RPU rewrite, so a decision that asked for a
/// conversion has to give up its **preservation** here, not just its
/// conversion. A fresh GOP-aware copy does have that rewrite and deliberately
/// bypasses this narrowing for a conversion request.
///
/// The failure that closes: `decide` answers `preserve = true` for every
/// converting client, because the RPUs must survive the bitstream filter for
/// the conversion to have anything to rewrite. Carrying that `true` onto a
/// path with no conversion in it keeps the Profile 7 RPUs *and* the type-63
/// enhancement layer and copies the source's own Profile 7 configuration
/// record through — handing dual-layer Dolby Vision to a client whose caps say
/// it decodes 8 and not 7. That is a black screen, where the strip this
/// produces is the HDR10 base the same client was getting before the
/// conversion existed and can certainly play.
///
/// Dropping the conversion flag matters for a different reason and both are
/// load-bearing: it is what the playlist and the create response's badge are
/// computed from, so leaving it set advertises `dvh1.08.LL` and badges
/// `dolby_vision` over a stream this path stripped to HDR10.
///
/// **The conversion flag is not the whole question, and keying only on it left
/// the guard blind.** It asks "was a conversion requested?" when what this path
/// has to answer is "can what it is about to emit be decoded?" — and those come
/// apart whenever `preserve` arrives without `convert`. Every way that pair can
/// be split ends here: a session built by an older node during a rolling
/// upgrade, an operator with the conversion turned off, a row whose Dolby
/// Vision columns are missing so [`plurx_core::playback::file_can_convert_to_p81`]
/// refuses it, a future caller that sets one field and not the other. In each
/// of them `preserve && !convert` is `true`, and on a path with no RPU rewrite
/// in it that means the source's own configuration record, its RPUs and its
/// type-63 enhancement layer, handed over intact.
///
/// So the second half of the guard asks the **file**, which this path always
/// has and can never be wrong about, rather than a flag that travelled: a
/// dual-layer source is not preserved by a copy that will not convert it. The
/// one client this narrows against its wishes is a client that enumerated a
/// dual-layer profile, and it is narrowed on the recovery path only, to a base
/// layer it can certainly decode — the trade the whole of this function already
/// makes.
///
/// A separate value rather than a mutation, so the caller keeps the decision's
/// answer to log the difference.
pub(super) fn served_copy_options(
    file: &plurx_core::domain::MediaFile,
    options: CopySessionOptions,
) -> CopySessionOptions {
    CopySessionOptions {
        preserve_dolby_vision: options.preserve_dolby_vision
            && !options.convert_dolby_vision
            && !plurx_core::playback::dolby_vision_is_dual_layer(file),
        convert_dolby_vision: false,
        ..options
    }
}

pub(super) fn copied_hls_codecs(
    file: &plurx_core::domain::MediaFile,
    audio_index: Option<i64>,
    options: CopySessionOptions,
    probe_json: Option<&str>,
) -> (String, Option<String>) {
    let mut supplemental = None;
    let video = match file.video_codec.as_deref() {
        // A converting copy is described by what it PRODUCES, not by what the
        // source's own ffprobe record says. The source is Profile 7, so every
        // arm below would answer `dvh1.07.06` — advertising the one profile
        // this client's caps said it cannot decode, over media whose sample
        // entry is `hvc1` and whose configuration record plurx wrote as
        // profile 8. Two lies at once, to the only client class that ever
        // reaches this branch.
        Some("hevc" | "h265") if options.convert_dolby_vision => {
            let level = file.dolby_vision.level.unwrap_or(6);
            // The compatibility id says what a non-Dolby-Vision client sees of
            // the base layer, and 8.1 is the HDR10 spelling — which is what
            // `file_can_convert_to_p81` required to route here at all.
            supplemental = Some(format!("dvh1.08.{level:02}/db1p"));
            // …and the base entry stays `hvc1`, because Profile 8.1 is a
            // backward-compatible enhancement of HDR10 and Apple's contract
            // requires a compatible stream to keep it. The profile is declared
            // by SUPPLEMENTAL-CODECS above, which is a codec string rather
            // than a sample-entry fourcc.
            hevc_hls_codec(probe_json).unwrap_or_else(|| "hvc1".to_owned())
        }
        Some("hevc" | "h265")
            if options.preserve_dolby_vision && file.hdr.as_deref() == Some("dolby_vision") =>
        {
            match dolby_vision_hls_config(probe_json) {
                Some(config) if config.profile == 8 && config.compatibility_id == Some(1) => {
                    supplemental = Some(format!("{}/db1p", config.codec));
                    hevc_hls_codec(probe_json).unwrap_or_else(|| "hvc1".to_owned())
                }
                Some(config) if config.profile == 8 && config.compatibility_id == Some(4) => {
                    supplemental = Some(format!("{}/db4h", config.codec));
                    hevc_hls_codec(probe_json).unwrap_or_else(|| "hvc1".to_owned())
                }
                Some(config) => config.codec,
                None => "dvh1".to_owned(),
            }
        }
        Some("hevc" | "h265") => hevc_hls_codec(probe_json).unwrap_or_else(|| "hvc1".to_owned()),
        Some("h264" | "avc") => {
            avc_hls_codec(probe_json).unwrap_or_else(|| "avc1.640034".to_owned())
        }
        _ => "avc1.640034".to_owned(),
    };
    let audio = if options.transcode_audio {
        "mp4a.40.2"
    } else {
        copied_audio_codec(file, audio_index)
    };
    (format!("{video},{audio}"), supplemental)
}

/// AVPlayer accepts a media playlist without codec metadata, but a
/// multivariant playlist has to describe Dolby Vision with its RFC 6381
/// profile and level. A bare `dvh1` makes an otherwise playable copied stream
/// fail during asset preparation. The scanner keeps ffprobe's DOVI
/// configuration record verbatim, so use the exact values that are also
/// carried by the remuxed sample entry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DolbyVisionHlsConfig {
    codec: String,
    profile: u64,
    compatibility_id: Option<u64>,
}

fn video_probe_stream(probe_json: Option<&str>) -> Option<serde_json::Value> {
    let probe: serde_json::Value = serde_json::from_str(probe_json?).ok()?;
    probe
        .get("streams")?
        .as_array()?
        .iter()
        .find(|stream| stream.get("codec_type").and_then(|value| value.as_str()) == Some("video"))
        .cloned()
}

fn dolby_vision_hls_config(probe_json: Option<&str>) -> Option<DolbyVisionHlsConfig> {
    let video = video_probe_stream(probe_json)?;
    let dovi = video
        .get("side_data_list")?
        .as_array()?
        .iter()
        .find(|side| {
            side.get("side_data_type")
                .and_then(|value| value.as_str())
                .is_some_and(|kind| kind.contains("DOVI") || kind.contains("Dolby Vision"))
        })?;
    let profile = dovi.get("dv_profile")?.as_u64()?;
    let level = dovi.get("dv_level")?.as_u64()?;
    Some(DolbyVisionHlsConfig {
        codec: format!("dvh1.{profile:02}.{level:02}"),
        profile,
        compatibility_id: dovi
            .get("dv_bl_signal_compatibility_id")
            .and_then(serde_json::Value::as_u64),
    })
}

/// RFC 6381 HEVC identifier for the HDR10/HLG base layer used by a
/// backward-compatible Dolby Vision stream. The sources plurx copies are
/// Main/Main10, and ffprobe's numeric `level` is already the value RFC 6381
/// places after the tier letter (for example 150 for HEVC level 5.0).
fn hevc_hls_codec(probe_json: Option<&str>) -> Option<String> {
    let video = video_probe_stream(probe_json)?;
    let (profile, compatibility) = match video.get("profile")?.as_str()? {
        // RFC 6381 writes HEVC compatibility flags in reverse bit order.
        // Apple's HLS appendix consequently spells these `1.6` and `2.4`,
        // not the raw ffprobe/HEVC bit masks `60000000` and `20000000`.
        "Main" => (1, "6"),
        "Main 10" => (2, "4"),
        _ => return None,
    };
    let level = video.get("level")?.as_u64()?;
    let tier = if video.get("tier").and_then(serde_json::Value::as_str) == Some("High") {
        'H'
    } else {
        'L'
    };
    Some(format!("hvc1.{profile}.{compatibility}.{tier}{level}.B0"))
}

fn avc_hls_codec(probe_json: Option<&str>) -> Option<String> {
    let video = video_probe_stream(probe_json)?;
    let profile = match video.get("profile")?.as_str()? {
        "Baseline" | "Constrained Baseline" => 66_u8,
        "Main" => 77,
        "High" => 100,
        _ => return None,
    };
    let level = video.get("level")?.as_u64()?;
    let level = u8::try_from(level).ok()?;
    Some(format!("avc1.{profile:02x}00{level:02x}"))
}
