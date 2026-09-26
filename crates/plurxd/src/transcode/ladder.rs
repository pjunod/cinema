use super::*;

pub(super) fn bitrate_for_height(height: i64) -> u32 {
    match height {
        h if h >= 2160 => 20_000,
        h if h >= 1080 => 8_000,
        h if h >= 720 => 4_000,
        h if h >= 480 => 2_000,
        _ => 1_200,
    }
}

/// The measured Dolby-Vision → HDR10 rungs.
///
/// Software retains the original 1080p point. QuickSync additionally admits
/// 2160p after the exact Profile 5 software-decode → tonemapx passthrough →
/// P010 upload → HEVC Main10 graph measured 1.52× realtime at 1080p and 1.26×
/// at 2160p on media1 (2026-08-22). Boot re-proves the device graph before the
/// larger rung is advertised; machines that cannot reproduce it stay at the
/// conservative route.
pub const HDR10_HEIGHT: i64 = 1080;
pub const HDR10_4K_HEIGHT: i64 = 2160;

/// The output-geometry proof attached to an already-resolved encoder route.
pub(super) fn capability_height_for_encoder(
    file: Option<&plurx_core::domain::MediaFile>,
    encoder: Encoder,
) -> i64 {
    if encoder == Encoder::Software {
        return AUTO_SOFTWARE_HEIGHT;
    }
    match file {
        // Profile 5 software decode → tonemapx SDR reshape → QSV H.264
        // measured 1.37× realtime at 2160p on media1 (2026-08-22). The pairing
        // probe proves the node's device init/upload graph before its caller
        // may resolve QSV, so SDR-only displays keep the source geometry too.
        Some(file)
            if encoder == Encoder::Qsv
                && TranscodeManager::needs_dovi_reshape(file) == Ok(true) =>
        {
            MAX_HEIGHT
        }
        // Other hardware HDR graphs retain the older 1080p proof. Do not turn
        // one QSV/Profile-5 measurement into a claim about them.
        Some(file) if file.hdr.is_some() => AUTO_HARDWARE_PROBED_HEIGHT,
        // A known SDR source needs no tone-map graph. Validated hardware H.264
        // can retain its geometry; network evidence is applied separately.
        Some(_) => MAX_HEIGHT,
        None => AUTO_HARDWARE_PROBED_HEIGHT,
    }
}

/// HEVC level 4.0's maximum luma picture size, in samples.
///
/// The output frame has to fit the level [`HDR10_HLS_CODEC`] declares. A
/// 2.39:1 film at 1080 high is 2578x1080 — 2.78 M samples, past this bound —
/// and x265 would encode it at level 5, making the advertised `L120` a lie
/// about the stream. Those sessions take the SDR ladder, which is what they
/// get today.
pub(super) const HDR10_MAX_LUMA_SAMPLES: i64 = 2_228_224;
/// HEVC level 5.0's maximum luma picture size, in samples.
const HDR10_4K_MAX_LUMA_SAMPLES: i64 = 8_912_896;

/// The RFC 6381 identifier for the HDR10 rung's video, **measured** rather
/// than derived.
///
/// Produced by running the production argument list against a real Profile 5
/// sample on jellyfin-ffmpeg7 7.1.4-3 at 1920x1080 / 8000 kb/s, remuxing the
/// result to fMP4, and reading the `hvcC` with the same field extraction
/// `http::hls::hevc_codec_from_init` uses: profile_space 0, tier high,
/// profile_idc 2 (Main 10), compatibility flags 0x4 reversed, level 120
/// (4.0), one non-zero constraint byte 0x90.
///
/// Its shape is what makes the master emit `VIDEO-RANGE=PQ` — that logic
/// keys on an `hvc1`/`hev1`/`dvh1`/`dvhe` prefix and is deliberately not
/// duplicated here.
///
/// Note the **H**igh tier: `should_serve_high_tier_media_playlist` already
/// collapses a High-tier HDR master to its media rendition for AVPlayer, and
/// this rung inherits that unchanged.
const HDR10_HLS_CODEC: &str = "hvc1.2.4.H120.90";
/// Measured from the 2160p QSV output's hvcC: Main10, compatibility 4, High
/// tier, level 150, constraint byte 0x90.
const HDR10_4K_HLS_CODEC: &str = "hvc1.2.4.H150.90";

/// The RFC 6381 `CODECS` value for a *re-encoded* HLS session.
///
/// A transcode's output is described by the argument list that produced it,
/// not by a probe: unlike the copy path there is no source sample entry to
/// read, and unlike an fMP4 session there is no `init.mp4` for
/// `http::hls::exact_hls_context` to open (this muxer writes MPEG-TS). So the
/// string is the grade's, and the grade is the pipeline's.
pub(super) fn transcoded_hls_codecs(grade: OutputGrade, target_height: i64) -> String {
    match grade {
        OutputGrade::Sdr => "avc1.640034,mp4a.40.2".to_owned(),
        OutputGrade::Hdr10 if target_height > HDR10_HEIGHT => {
            format!("{HDR10_4K_HLS_CODEC},mp4a.40.2")
        }
        OutputGrade::Hdr10 => format!("{HDR10_HLS_CODEC},mp4a.40.2"),
    }
}

/// Does this source/encoder pair fit one of the measured HDR10 points?
pub(super) fn hdr10_rung_fits(
    file: &plurx_core::domain::MediaFile,
    target_height: i64,
    encoder: Encoder,
) -> bool {
    let max_samples = match (target_height, encoder) {
        (HDR10_HEIGHT, Encoder::Software | Encoder::Qsv) => HDR10_MAX_LUMA_SAMPLES,
        (HDR10_4K_HEIGHT, Encoder::Qsv) => HDR10_4K_MAX_LUMA_SAMPLES,
        _ => return false,
    };
    match plurx_core::transcode::output_size(file, target_height) {
        // Never upscale into HDR: `output_size` clamps to the source, so a
        // 720p source at the 1080 rung produces a 720p frame, which is not
        // the geometry the codec declaration was measured at.
        Some((w, h)) => h == target_height && w * h <= max_samples,
        None => false,
    }
}

/// The adaptive lower ladder's rungs, bottom to top (ADAPTIVE-QUALITY.md).
/// Hardware Auto may begin at a distinct source-preserving rung above 1080
/// when direct play is impossible; network evidence and stall recovery enter
/// this lower ladder at 1080. Keeping that top rung out of this shared list
/// prevents the decision and offline APIs from offering it before a live
/// session has resolved the node's encoder and pipeline capability.
pub const LADDER_HEIGHTS: [i64; 4] = [360, 480, 720, 1080];

/// Resolve exactly one step below a session's already-normalized height.
/// Heights between published rungs land on the next lower rung; a session
/// already at or below the ladder floor repeats the rung it is on, so the
/// native client's own recovery budget owns the terminal outcome rather than
/// a new server error path.
///
/// The floor is `current`, never `LADDER_HEIGHTS[0]`. An Auto session's
/// resolved rung is not ladder-constrained below 360: `auto_height` follows a
/// sub-360 source, and `auto_height_from_prior` settles at [`MIN_HEIGHT`] on
/// exactly the starved links that stall and reopen. Answering those with 360
/// would step the viewer *up* — onto a rung the server's own stored verdict
/// recorded as starving — and would promise a rung a sub-360 source cannot
/// feed, which is the advertised-vs-delivered defect `auto_height` already
/// fixed once.
pub(super) fn one_rung_below(current: i64) -> i64 {
    LADDER_HEIGHTS
        .iter()
        .rev()
        .copied()
        .find(|height| *height < current)
        .unwrap_or(current)
        // The same lower bound every other height in the system takes. A
        // predecessor recorded at 0 — a remux of an unprobed source — must
        // not normalize into a zero-height transcode.
        .max(MIN_HEIGHT)
}

/// One advertised rung of the ladder.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Rung {
    pub height: i64,
    /// The rung's nominal cost on the wire: video target + audio, in kb/s.
    /// This is the number an adaptation controller compares its estimate to
    /// (its thresholds carry their own headroom factors).
    pub total_kbps: u32,
    /// What the rung may PEAK at over the rate-control window: `-maxrate`
    /// (1.5× the target, PERF-PLAN §4.6) + audio. The number that must
    /// cover the measured burst — media1 measured 9.05 Mb/s on the 1080
    /// rung's 8 Mb/s target — and the one an HLS `BANDWIDTH` attribute
    /// would be required to state.
    pub peak_kbps: u32,
}

/// The ladder as advertised to a client, top rung first, filtered to what
/// the source can actually feed: rungs above the source height are dropped
/// (a 720p file offers 720p and below — never an upscale), and an unprobed
/// source offers the whole ladder because there is nothing to filter by.
pub fn ladder(source_height: Option<i64>) -> Vec<Rung> {
    LADDER_HEIGHTS
        .iter()
        .rev()
        .filter(|h| source_height.filter(|s| *s > 0).is_none_or(|s| **h <= s))
        .map(|&height| {
            let video = bitrate_for_height(height);
            Rung {
                height,
                total_kbps: video + plurx_core::transcode::AUDIO_BITRATE_KBPS_DEFAULT,
                peak_kbps: video * 3 / 2 + plurx_core::transcode::AUDIO_BITRATE_KBPS_DEFAULT,
            }
        })
        .collect()
}

/// The ladder a session response may advertise: the source's own rungs,
/// further capped by what this node can actually serve for this file
/// ([`TranscodeManager::capability_height_ceiling`]).
///
/// Separate from [`ladder`] because the two answer different questions.
/// `ladder` is "what rungs exist at or below this source"; this is "which of
/// them will the server be held to". The web ABR controller upgrades into any
/// rung the response lists, so the difference is load-bearing.
pub fn advertised_ladder(source_height: Option<i64>, ceiling: i64) -> Vec<Rung> {
    let resolved_ceiling = source_height
        .filter(|height| *height > 0)
        .map_or(ceiling, |height| height.min(ceiling));
    let mut rungs = ladder(Some(resolved_ceiling));
    let lower_ladder_top = LADDER_HEIGHTS[LADDER_HEIGHTS.len() - 1];
    if resolved_ceiling > lower_ladder_top {
        let video = bitrate_for_height(resolved_ceiling);
        rungs.insert(
            0,
            Rung {
                height: resolved_ceiling,
                total_kbps: video + plurx_core::transcode::AUDIO_BITRATE_KBPS_DEFAULT,
                peak_kbps: video * 3 / 2 + plurx_core::transcode::AUDIO_BITRATE_KBPS_DEFAULT,
            },
        );
    }
    rungs
}

/// Apply the node-local prior to the already encoder-capped Auto choice.
/// Absence is an exact identity operation, which is the default-off contract.
///
/// Both signals the prior carries are applied, and the lower rung wins. The
/// starvation verdict used to return early, which made the throughput estimate
/// dead code for any tuple that had ever stalled — including in the downward
/// direction, so a link that later collapsed still started at the rung below
/// its old starvation. `now_ms` is here because the verdict expires
/// ([`plurx_core::domain::NETWORK_PRIOR_STARVED_TTL_MS`]); a tuple whose
/// starvation has aged out is decided by throughput alone, which is what lets
/// a link that has since recovered reach the cap again.
pub(super) fn auto_height_from_prior(
    current: i64,
    source_height: Option<i64>,
    prior: Option<&plurx_core::domain::NetworkPrior>,
    now_ms: i64,
) -> i64 {
    let Some(prior) = prior else {
        return current;
    };
    let mut height = current;

    // A known supply failure is stronger evidence than an EWMA collected
    // during healthy playback. Start below the lowest rung known to starve.
    if let Some(starved) = prior.active_starved_rung(now_ms) {
        let below = LADDER_HEIGHTS
            .iter()
            .rev()
            .copied()
            .find(|height| *height < starved)
            .unwrap_or(MIN_HEIGHT);
        height = height.min(below);
    }

    if let Some(sustained_kbps) = prior.sustained_kbps {
        let peak =
            bitrate_for_height(height) * 3 / 2 + plurx_core::transcode::AUDIO_BITRATE_KBPS_DEFAULT;
        if sustained_kbps < peak {
            height = ladder(source_height)
                .into_iter()
                .find(|rung| rung.height <= height && rung.peak_kbps <= sustained_kbps)
                .map(|rung| rung.height)
                .unwrap_or(MIN_HEIGHT);
        }
    }
    height.max(MIN_HEIGHT)
}

/// Snap an explicitly requested height onto the ladder: nearest rung, ties
/// DOWN (bandwidth is the scarce thing). Two escapes, both deliberate.
/// Heights above the top rung pass through untouched — they are
/// Original-class requests (a forced-subtitle burn under Original carries a
/// 4K source's own 2160), not strays. And the caller must not snap a request
/// for the source's own height for the same reason; that exception needs the
/// file and so lives at the call site.
pub fn snap_height(height: i64) -> i64 {
    let top = LADDER_HEIGHTS[LADDER_HEIGHTS.len() - 1];
    if height > top {
        return height;
    }
    LADDER_HEIGHTS
        .iter()
        .copied()
        .min_by_key(|rung| ((rung - height).abs(), *rung))
        .unwrap_or(top)
}
