use super::*;

/// Replace the scanner's best-effort HEVC tier with the exact declaration
/// carried by the HLS initialization segment.
///
/// ffprobe records Main/Main10 and level in the library row but commonly
/// omits the tier. Guessing Main tier then advertises `L150` for a High-tier
/// `hvcC` record (`H150`), and AVPlayer treats the only HDR variant as
/// unsupported. The init segment is the canonical description of the bytes
/// this session actually publishes, including any Dolby Vision stripping.
///
/// A `dvh1`/`dvhe` sample entry is NOT described by the HEVC form. Dolby
/// Vision's HLS identifier is `dvh1.PP.LL` — two-digit profile, two-digit
/// level — and the `hvcC` beside it describes only the base layer, so reading
/// the tier out of it and emitting `dvh1.2.4.L150.90` produces a string no
/// player can resolve. AVPlayer rejected exactly that on a Profile 5 title
/// while the same session's media playlist, which carries no CODECS at all,
/// played. Dolby Vision therefore reads its own `dvcC`/`dvvC` configuration
/// record, which is also the canonical answer when the library row's probe
/// carried no DOVI side data and the advertised codec came through bare.
pub(super) async fn exact_hls_context_before(
    state: &AppState,
    session: &str,
    context: crate::transcode::HlsContext,
    owner: &crate::transcode::MediaResponseOwner,
    deadline: Instant,
) -> Result<crate::transcode::HlsContext, ApiError> {
    let started = Instant::now();
    let inspection_deadline = deadline
        .checked_sub(Duration::from_millis(25))
        .unwrap_or(deadline);
    let inspect_avc_init = owner.avc_master_uses_init();
    let inspected = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        exact_hls_context_at(
            state,
            session,
            context,
            inspect_avc_init,
            inspection_deadline,
        ),
    )
    .await;
    match inspected {
        Ok(Ok(context)) => {
            tracing::info!(
                target: "plurxd::http::hls",
                session = %crate::transcode::session_log_id(session),
                phase = "init_inspection",
                outcome = "ready",
                elapsed_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64,
                "resolved exact HLS initialization context"
            );
            Ok(context)
        }
        Ok(Err(error)) => {
            // The init lookup and parse can cross an owner replacement. A
            // predecessor's refusal must never be published against the
            // successor that reused this capability URL.
            authorize_attempt_status(
                state,
                session,
                owner,
                "master-playlist-init-inspection",
                None,
                deadline,
            )
            .await?;
            tracing::warn!(
                target: "plurxd::http::hls",
                session = %crate::transcode::session_log_id(session),
                phase = "init_inspection",
                outcome = error.log_code(),
                elapsed_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64,
                "HLS initialization inspection refused the playlist"
            );
            Err(error.into_api_error())
        }
        Err(_) => {
            tracing::warn!(
                target: "plurxd::http::hls",
                session = %crate::transcode::session_log_id(session),
                phase = "init_inspection",
                outcome = "response_publication_timeout",
                elapsed_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64,
                "HLS initialization inspection exhausted the response deadline"
            );
            Err(response_publication_timeout())
        }
    }
}

#[derive(Debug)]
pub(super) enum HlsInitInspectionError {
    Response {
        status: StatusCode,
        code: &'static str,
        message: &'static str,
    },
    Api(ApiError),
}

impl HlsInitInspectionError {
    fn invalid() -> Self {
        Self::Response {
            status: StatusCode::BAD_GATEWAY,
            code: "hls_init_invalid",
            message: "the published HLS initialization media is malformed or incomplete",
        }
    }

    fn unsupported() -> Self {
        Self::Response {
            status: StatusCode::BAD_GATEWAY,
            code: "hls_init_unsupported",
            message: "the published HLS initialization media uses an unsupported codec layout",
        }
    }

    fn unavailable() -> Self {
        Self::Response {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "init_inspection_unavailable",
            message: "initialization media could not be inspected; retry shortly",
        }
    }

    fn pending() -> Self {
        Self::Response {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "startup_timeout",
            message: "initialization media is not ready yet; retry shortly",
        }
    }

    fn from_api_error(error: ApiError) -> Self {
        Self::Api(error)
    }

    fn log_code(&self) -> &'static str {
        match self {
            Self::Response { code, .. } => code,
            Self::Api(_) => "producer_or_session_failure",
        }
    }

    fn into_api_error(self) -> ApiError {
        match self {
            Self::Response {
                status,
                code,
                message,
            } => ApiError::typed(status, code, message),
            Self::Api(error) => error,
        }
    }
}

#[cfg(test)]
pub(super) async fn exact_hls_context(
    state: &AppState,
    session: &str,
    context: crate::transcode::HlsContext,
) -> Result<crate::transcode::HlsContext, HlsInitInspectionError> {
    exact_hls_context_at(
        state,
        session,
        context,
        true,
        Instant::now() + RESPONSE_PUBLICATION_LIFECYCLE_BUDGET,
    )
    .await
}

pub(super) async fn exact_hls_context_at(
    state: &AppState,
    session: &str,
    context: crate::transcode::HlsContext,
    inspect_avc_init: bool,
    deadline: Instant,
) -> Result<crate::transcode::HlsContext, HlsInitInspectionError> {
    let fallback_video = context.codecs.split(',').next().unwrap_or_default();
    let Some(sample_entry) = ["hvc1", "hev1", "dvh1", "dvhe", "avc1"]
        .into_iter()
        .find(|entry| fallback_video.starts_with(entry) && (*entry != "avc1" || inspect_avc_init))
    else {
        return Ok(context);
    };
    // A fenced successor names its init after its ownership epoch, so the
    // object to probe comes from the session, not from a literal. Asking for
    // the wrong name does not fail fast: `segment` waits for a segment that
    // will never be produced, stalling every playlist request for the full
    // production wait before falling back to the scanner's guessed tier.
    let Some(init_object) = state.transcode.session_init_object(session).await else {
        return Err(HlsInitInspectionError::pending());
    };
    // Initialization segments are a few KiB. Bound malformed input so a
    // playlist request can never allocate without limit. VOD init bytes come
    // through their own registry; live bytes retain the internal delivery
    // tracker that keeps this probe out of player throughput telemetry.
    let mut init = Vec::new();
    match state
        .transcode
        .vod_segment_before(session, &init_object, deadline)
        .await
    {
        Some(crate::transcode::VodResponsePublication {
            result: Ok(Some(ready)),
            ..
        }) => {
            if ready.len > INIT_INSPECTION_LIMIT_BYTES {
                return Err(HlsInitInspectionError::invalid());
            }
            init.reserve(ready.len as usize);
            let mut reader = ready.file.take(ready.len);
            let read = reader
                .read_to_end(&mut init)
                .await
                .map_err(|_| HlsInitInspectionError::unavailable())?;
            if read as u64 != ready.len {
                return Err(HlsInitInspectionError::invalid());
            }
        }
        Some(crate::transcode::VodResponsePublication {
            result: Ok(None), ..
        }) => return Err(HlsInitInspectionError::invalid()),
        Some(crate::transcode::VodResponsePublication {
            result: Err(crate::vodserve::VodError::Pending { .. }),
            ..
        }) => return Err(HlsInitInspectionError::pending()),
        Some(crate::transcode::VodResponsePublication {
            result: Err(crate::vodserve::VodError::Busy(_) | crate::vodserve::VodError::Io(_)),
            ..
        }) => return Err(HlsInitInspectionError::unavailable()),
        Some(crate::transcode::VodResponsePublication {
            result: Err(error @ crate::vodserve::VodError::ProducerFailed(_)),
            ..
        })
        | Some(crate::transcode::VodResponsePublication {
            result: Err(error @ crate::vodserve::VodError::Gone(_)),
            ..
        }) => {
            return Err(HlsInitInspectionError::from_api_error(vod_error(
                session, error,
            )));
        }
        None => {
            let opened = match state
                .transcode
                .segment_for_publication_before(session, &init_object, deadline)
                .await
            {
                Ok(crate::transcode::SegmentPublication::Ready(opened)) => *opened,
                Ok(crate::transcode::SegmentPublication::Pending(_)) => {
                    return Err(HlsInitInspectionError::pending());
                }
                Ok(crate::transcode::SegmentPublication::Missing(Some(_))) => {
                    return Err(HlsInitInspectionError::invalid());
                }
                Ok(crate::transcode::SegmentPublication::Missing(None)) => {
                    return Err(HlsInitInspectionError::from_api_error(playlist_error(
                        session,
                        PlaylistError::SessionGone,
                    )));
                }
                Ok(crate::transcode::SegmentPublication::Unavailable(_)) => {
                    return Err(HlsInitInspectionError::unavailable());
                }
                Ok(crate::transcode::SegmentPublication::Failed(error)) => {
                    return Err(HlsInitInspectionError::from_api_error(playlist_error(
                        session,
                        error.error,
                    )));
                }
                Err(crate::transcode::SegmentOpenError::Capacity) => {
                    return Err(HlsInitInspectionError::unavailable());
                }
            };
            // This open is an internal capability check even when the size
            // alone rejects it. Settle the zero-body tracker explicitly so a
            // deliberate oversized refusal is not logged as an abandoned
            // client response.
            let mut delivery = opened.delivery.into_internal_probe();
            if opened.len > INIT_INSPECTION_LIMIT_BYTES {
                delivery.finish_without_body();
                return Err(HlsInitInspectionError::invalid());
            }
            init.reserve(opened.len as usize);
            // No response body exists here — this is the playlist generator
            // reading `hvcC` for itself.
            delivery.expect_at_most(opened.len);
            let started = Instant::now();
            let mut reader = opened.file.take(opened.len);
            match reader.read_to_end(&mut init).await {
                Ok(bytes) => {
                    delivery.note_read(bytes as u64, started.elapsed());
                    delivery.finish();
                    if bytes as u64 != opened.len {
                        return Err(HlsInitInspectionError::invalid());
                    }
                }
                Err(error) => {
                    delivery.fail(&error);
                    return Err(HlsInitInspectionError::unavailable());
                }
            }
        }
    }
    bind_hls_init_context(context, &init, sample_entry)
}

fn bind_hls_init_context(
    mut context: crate::transcode::HlsContext,
    init: &[u8],
    sample_entry: &str,
) -> Result<crate::transcode::HlsContext, HlsInitInspectionError> {
    let mut reader = plurx_core::fmp4::FragmentReader::new();
    reader.push(init);
    let parsed = match reader.next_unit() {
        Ok(Some(plurx_core::fmp4::Unit::Init(init))) => init,
        Ok(Some(_)) | Ok(None) | Err(plurx_core::fmp4::Fmp4Error::Malformed(_)) => {
            return Err(HlsInitInspectionError::invalid());
        }
        Err(
            plurx_core::fmp4::Fmp4Error::Unsupported(_)
            | plurx_core::fmp4::Fmp4Error::MultipleHevcSampleEntries { .. },
        ) => return Err(HlsInitInspectionError::unsupported()),
    };
    let required_box = match sample_entry {
        "dvh1" | "dvhe" => init
            .windows(4)
            .any(|window| window == b"dvcC" || window == b"dvvC"),
        // AVC is resolved structurally from the selected video track below;
        // a raw byte occurrence is not evidence that an avcC belongs to it.
        "avc1" => true,
        _ => init.windows(4).any(|window| window == b"hvcC"),
    };
    if !required_box {
        return Err(HlsInitInspectionError::invalid());
    }
    if matches!(sample_entry, "hvc1" | "dvh1") {
        match plurx_core::fmp4::hevc_parameter_sets_complete(&parsed) {
            Ok(true) => {}
            Ok(false) | Err(plurx_core::fmp4::Fmp4Error::Malformed(_)) => {
                return Err(HlsInitInspectionError::invalid());
            }
            Err(
                plurx_core::fmp4::Fmp4Error::Unsupported(_)
                | plurx_core::fmp4::Fmp4Error::MultipleHevcSampleEntries { .. },
            ) => return Err(HlsInitInspectionError::unsupported()),
        }
    }
    if sample_entry != "avc1" {
        match plurx_core::fmp4::validate_hevc_sample_entries(&parsed) {
            Ok(plurx_core::fmp4::HevcSampleEntryLayout::Single) => {}
            Ok(plurx_core::fmp4::HevcSampleEntryLayout::NotHevc)
            | Ok(plurx_core::fmp4::HevcSampleEntryLayout::Multiple { .. }) => {
                return Err(HlsInitInspectionError::unsupported());
            }
            Err(plurx_core::fmp4::Fmp4Error::Malformed(_)) => {
                return Err(HlsInitInspectionError::invalid());
            }
            Err(
                plurx_core::fmp4::Fmp4Error::Unsupported(_)
                | plurx_core::fmp4::Fmp4Error::MultipleHevcSampleEntries { .. },
            ) => return Err(HlsInitInspectionError::unsupported()),
        }
    }
    let derived = if sample_entry == "avc1" {
        match plurx_core::fmp4::avc_rfc6381_codec(&parsed) {
            Ok(Some(codec)) => Some(codec),
            Ok(None) | Err(plurx_core::fmp4::Fmp4Error::Malformed(_)) => {
                return Err(HlsInitInspectionError::invalid());
            }
            Err(
                plurx_core::fmp4::Fmp4Error::Unsupported(_)
                | plurx_core::fmp4::Fmp4Error::MultipleHevcSampleEntries { .. },
            ) => return Err(HlsInitInspectionError::unsupported()),
        }
    } else if matches!(sample_entry, "dvh1" | "dvhe") {
        dolby_vision_codec_from_init(init, sample_entry)
    } else {
        hevc_codec_from_init(init, sample_entry)
    };
    let video = derived.ok_or_else(HlsInitInspectionError::unsupported)?;
    if let Some(facts) = &mut context.codec_facts {
        if sample_entry == "avc1" {
            facts.bind_output_avc_init(video.clone());
        } else if matches!(sample_entry, "hvc1" | "hev1") {
            facts.bind_output_hevc_init(video.clone());
        }
    }
    context.codecs = match context.codecs.split_once(',') {
        Some((_, audio)) if !audio.trim().is_empty() => format!("{video},{}", audio.trim()),
        _ => video,
    };
    Ok(context)
}

/// Apple's Dolby Vision HLS identifier from the `DOVIDecoderConfigurationRecord`.
///
/// The record is carried by `dvcC` (profiles up to 7) or `dvvC` (profiles 8
/// and 9) inside the sample entry. Its payload begins with two version bytes
/// and then packs the two fields the playlist needs across two more:
///
/// ```text
/// byte 2: dv_profile (7 bits) | dv_level high bit
/// byte 3: dv_level low 5 bits | rpu_present | el_present | bl_present
/// ```
///
/// Apple's form is `dvh1.PP.LL` with both fields zero-padded to two digits —
/// no tier letter, no constraint bytes, and nothing from `hvcC`.
pub(super) fn dolby_vision_codec_from_init(init: &[u8], sample_entry: &str) -> Option<String> {
    let type_at = [b"dvcC", b"dvvC"]
        .into_iter()
        .find_map(|name| init.windows(4).position(|window| window == name))?;
    let box_start = type_at.checked_sub(4)?;
    let box_size = u32::from_be_bytes(init.get(box_start..type_at)?.try_into().ok()?) as usize;
    let box_end = box_start.checked_add(box_size)?;
    // 8-byte header plus the 24-byte record.
    if box_size < 32 || box_end > init.len() {
        return None;
    }
    let payload = init.get(type_at + 4..box_end)?;
    if payload.len() < 4 {
        return None;
    }
    let profile = payload[2] >> 1;
    let level = ((payload[2] & 0x01) << 5) | (payload[3] >> 3);
    // Profile 0 is not assigned and level 0 is not a real declaration; either
    // means the record was not what this parser assumed.
    if profile == 0 || level == 0 {
        return None;
    }
    Some(format!("{sample_entry}.{profile:02}.{level:02}"))
}

/// RFC 6381 identifier from the HEVCDecoderConfigurationRecord in `hvcC`.
pub(super) fn hevc_codec_from_init(init: &[u8], sample_entry: &str) -> Option<String> {
    let type_at = init.windows(4).position(|window| window == b"hvcC")?;
    let box_start = type_at.checked_sub(4)?;
    let box_size = u32::from_be_bytes(init.get(box_start..type_at)?.try_into().ok()?) as usize;
    let box_end = box_start.checked_add(box_size)?;
    if box_size < 21 || box_end > init.len() {
        return None;
    }
    let payload = init.get(type_at + 4..box_end)?;
    if payload.len() < 13 || payload[0] != 1 {
        return None;
    }

    let profile_byte = payload[1];
    let profile_space = match profile_byte >> 6 {
        0 => "",
        1 => "A",
        2 => "B",
        3 => "C",
        _ => return None,
    };
    let profile = profile_byte & 0x1f;
    let tier = if profile_byte & 0x20 == 0 { 'L' } else { 'H' };
    let compatibility = u32::from_be_bytes(payload.get(2..6)?.try_into().ok()?).reverse_bits();
    let level = payload[12];
    let constraints = payload.get(6..12)?;
    let constraints_len = constraints
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |last| last + 1);
    let constraints = constraints[..constraints_len]
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>();

    let mut codec =
        format!("{sample_entry}.{profile_space}{profile}.{compatibility:X}.{tier}{level}");
    if !constraints.is_empty() {
        codec.push('.');
        codec.push_str(&constraints);
    }
    Some(codec)
}

/// Whether an Apple HDR master should collapse to its media rendition.
///
/// This is deliberately narrower than "HDR": Main-tier HDR/Dolby Vision and
/// every session with native text renditions keep the authored multivariant
/// playlist. The workaround applies only to the High-tier codec declaration
/// AVPlayer rejects and only when removing the wrapper loses no rendition.
pub(super) fn should_serve_high_tier_media_playlist(
    file: &MediaFile,
    context: &crate::transcode::HlsContext,
) -> bool {
    let has_native_subtitles = file
        .subtitle_streams
        .iter()
        .any(|track| is_native_text_subtitle(&track.codec));
    if file.hdr.is_none() || has_native_subtitles {
        return false;
    }
    let video = context.codecs.split(',').next().unwrap_or_default();
    let mut fields = video.split('.');
    let sample_entry = fields.next().unwrap_or_default();
    matches!(sample_entry, "hvc1" | "hev1" | "dvh1" | "dvhe")
        && fields.any(|field| {
            field.strip_prefix('H').is_some_and(|level| {
                !level.is_empty() && level.bytes().all(|byte| byte.is_ascii_digit())
            })
        })
}

/// Does this codec advertisement name Dolby Vision?
///
/// `dvh1`/`dvhe` are the Dolby Vision sample-entry spellings; a preserved
/// Profile 5 session carries one in `codecs` with no `SUPPLEMENTAL-CODECS`
/// beside it, which is why "no supplemental" alone is not the question.
fn advertises_dolby_vision(context: &crate::transcode::HlsContext) -> bool {
    let names_dv = |value: &str| value.contains("dvh1") || value.contains("dvhe");
    names_dv(&context.codecs) || context.supplemental_codecs.as_deref().is_some_and(names_dv)
}

/// Refuse to serve an initialization segment that declares Dolby Vision the
/// playlist does not advertise.
///
/// The invariant is the playlist's own claim: if neither `CODECS` nor
/// `SUPPLEMENTAL-CODECS` names Dolby Vision, an init carrying a `dvcC`/`dvvC`
/// record is describing a stream this session does not serve.
///
/// That pair is exactly what the legacy muxer path produces. A copy that
/// strips Dolby Vision removes the RPU and enhancement-layer NAL units with
/// `filter_units`, which works on NAL types and cannot see the DOVI side data
/// ffmpeg copied out of the source container — so on an ffmpeg without
/// `dovi_rpu` the muxer writes a Profile 7 record with `el_present_flag = 1`
/// over a stream carrying neither layer. The segmenting path removes it in
/// `copyseg`, and the VOD path through the promotion funnel; the legacy path
/// has ffmpeg's own HLS muxer write `init.mp4` straight to disk with no reader
/// in between, so this is where it is caught. Chrome ignores the box;
/// VideoToolbox honours it, and Safari answers 4K10 HEVC so labelled with a
/// software decode on hardware that has a dedicated block for it
/// (`docs/streaming/STUTTER-4K.md` §6).
///
/// Gated on what the playlist says rather than on how the session was built,
/// deliberately. The serve path has no copy options in hand, and the question
/// it can answer is better anyway: the playlist and the init must describe the
/// same stream, whatever produced them. A preserved or converted session
/// advertises Dolby Vision and keeps its record untouched.
///
/// The brand goes with it — `remove_dolby_vision_record` rewrites `dby1` in
/// the same call, because `dby1` over a sample entry with no record is a
/// contradictory init AVPlayer refuses outright rather than merely
/// software-decoding.
pub(super) fn strip_unadvertised_dolby_vision(
    context: &crate::transcode::HlsContext,
    init: &mut Vec<u8>,
) -> bool {
    use plurx_core::fmp4::{FragmentReader, Unit};

    if advertises_dolby_vision(context) {
        return false;
    }
    let mut reader = FragmentReader::new();
    reader.push(init);
    let Ok(Some(Unit::Init(mut parsed))) = reader.next_unit() else {
        return false;
    };
    // Only when the parse round-trips: this rewrites a file a client is about
    // to play, and an init this reader models incompletely must be served as
    // it is rather than as this function's idea of it.
    if parsed.bytes != *init {
        return false;
    }
    match plurx_core::fmp4::remove_dolby_vision_record(&mut parsed) {
        Ok(true) => {
            *init = parsed.bytes;
            true
        }
        _ => false,
    }
}

/// Clear the HEVC High-tier declaration AVPlayer rejects for otherwise
/// hardware-decodable HDR copy sessions.
///
/// Tier changes decoder throughput limits, not the coded picture or decoder
/// tools. Relabeling only the generated initialization record lets the bytes
/// reach VideoToolbox instead of failing AVPlayer's HLS preparation step. This
/// is deliberately limited to the same HDR/no-native-rendition sessions whose
/// master is collapsed above.
pub(super) fn normalize_high_tier_hevc_init(file: &MediaFile, init: &mut [u8]) -> bool {
    if file.hdr.is_none()
        || file
            .subtitle_streams
            .iter()
            .any(|track| is_native_text_subtitle(&track.codec))
    {
        return false;
    }
    let Some(type_at) = init.windows(4).position(|window| window == b"hvcC") else {
        return false;
    };
    let Some(box_start) = type_at.checked_sub(4) else {
        return false;
    };
    let Some(size_bytes) = init.get(box_start..type_at) else {
        return false;
    };
    let Ok(size_bytes) = size_bytes.try_into() else {
        return false;
    };
    let box_size = u32::from_be_bytes(size_bytes) as usize;
    let Some(box_end) = box_start.checked_add(box_size) else {
        return false;
    };
    let Some(payload) = init.get_mut(type_at + 4..box_end) else {
        return false;
    };
    if box_size < 21 || payload.len() < 13 || payload[0] != 1 || payload[1] & 0x20 == 0 {
        return false;
    }
    payload[1] &= !0x20;
    true
}

#[cfg(test)]
mod finite_hevc_init_tests {
    use super::*;
    use plurx_core::transcode::*;

    fn source() -> plurx_core::domain::MediaFile {
        plurx_core::domain::MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 5,
            item_id: 1,
            path: std::path::PathBuf::from("/media/profile5.mkv"),
            size: 1,
            mtime: 1,
            duration_ms: Some(1_000),
            container: Some("matroska".into()),
            video_codec: Some("hevc".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: Some("Main".into()),
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: Some(20_000_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    fn plan_context() -> crate::transcode::HlsContext {
        let file = source();
        let probe = serde_json::json!({"streams":[{"index":0,"codec_type":"video",
            "codec_name":"hevc","profile":"Main","width":3840,"height":2160,
            "pix_fmt":"yuv420p","color_transfer":"bt709","color_primaries":"bt709",
            "color_space":"bt709","color_range":"tv","field_order":"progressive",
            "sample_aspect_ratio":"1:1","avg_frame_rate":"24/1","r_frame_rate":"24/1"}]});
        let facts = DecodeFacts::from_ffprobe_json(
            &probe,
            DecodeSourceIdentity::from_sha256("a".repeat(64)).expect("source identity"),
        )
        .expect("facts");
        let caps = DecodeCapabilities::new(
            DecodeCapabilitySnapshotIdentity::new(
                "f".repeat(64),
                "test-node".into(),
                Some("e".repeat(64)),
            )
            .expect("snapshot"),
            vec![],
            vec![SoftwareDecoder {
                codec: "hevc".into(),
                implementation: Some("hevc".into()),
            }],
        )
        .expect("caps");
        let identity = MacosProcessingIdentity::new(
            "1".repeat(64),
            "2".repeat(64),
            "3".repeat(64),
            "4".repeat(64),
            "test".into(),
            "arm64".into(),
            "Apple test".into(),
        )
        .expect("processing identity");
        let context = MacosProcessingContext::new(
            false,
            identity,
            MacosProcessingAvailability::Unavailable,
            MacosProcessingAvailability::Unavailable,
        )
        .with_graph(
            MacosProcessingGraph::HevcSdrHost,
            MacosProcessingAvailability::Available,
        )
        .with_hevc_output_enabled(true);
        let options = TranscodeOptions {
            target_height: 1080,
            output_codec: Some(VideoCodec::Hevc),
            effective_rate_control: EffectiveRateControl::Vbr,
            ..Default::default()
        };
        let mut media = TranscodeMediaOptions::from_options(&file, &options);
        media.input_has_audio = true;
        let plan = resolve_transcode(
            &TranscodeRequest::new(Encoder::VideoToolbox, media),
            &facts,
            &caps,
            &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None)
                .with_macos_processing(context),
            &AttemptRestrictions::none(),
        )
        .expect("finite HEVC plan");
        assert_eq!(plan.output_contract().output_codec(), "hevc");
        let codec_facts =
            crate::transcode::FrozenHlsCodecFacts::encoded(&plan).with_sdr_master_codecs(true);
        assert_eq!(
            codec_facts.sdr_master_codecs(),
            None,
            "the family is not a qualified profile claim"
        );
        crate::transcode::HlsContext {
            bandwidth: None,
            file_id: file.id,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: crate::transcode::transcoded_hls_codecs_for_plan(&plan),
            codec_facts: Some(codec_facts),
            supplemental_codecs: None,
            frame_rate: Some(24.0),
        }
    }

    // Exact ftyp+moov from the signed 46f31 finite producer's served hvc1 init.
    // SHA256 c46d6fea4130a3b189a3f009449945bf8eb9d31d5615011650bd27db5ca42db2; the media payload and session capability are excluded.
    fn produced_init() -> Vec<u8> {
        hex::decode(concat!("0000001c6674797069736f350000020069736f3569736f366d703431000005406d6f6f760000006c6d76686400000000",
"0000000000000000000003e8000000000001000001000000000000000000000000010000000000000000000000000000",
"000100000000000000000000000000004000000000000000000000000000000000000000000000000000000000000002",
"000002637472616b0000005c746b68640000000300000000000000000000000100000000000000000000000000000000",
"000000000000000000010000000000000000000000000000000100000000000000000000000000004000000007800000",
"04380000000001ff6d646961000000206d646864000000000000000000000000000000180000000055c400000000002d",
"68646c72000000000000000076696465000000000000000000000000566964656f48616e646c657200000001aa6d696e",
"6600000014766d68640000000100000000000000000000002464696e660000001c647265660000000000000001000000",
"0c75726c20000000010000016a7374626c0000011e7374736400000000000000010000010e6876633100000000000000",
"01000000000000000000000000000000000780043800480000004800000000000000011f4c61766336322e32382e3130",
"3320686576635f766964656f746f6f6c626f780018ffff0000007768766343010160000000b0000000000078f000fcfd",
"f8f800000f03a00001001840010c01ffff016000000300b00000030000030078170240a10001002a4201010160000003",
"00b00000030000030078a003c0801107cb8817b916452ffcb9fc4feb016a02020201a2000100074401c072f053240000",
"000a6669656c010000000013636f6c726e636c7800010001000100000000107061737000000001000000010000001462",
"74727400000000007a1200007a1200000000107374747300000000000000000000001073747363000000000000000000",
"0000147374737a000000000000000000000000000000107374636f0000000000000000000001bf7472616b0000005c74",
"6b6864000000030000000000000000000000020000000000000000000000000000000000000001010000000001000000",
"0000000000000000000000000100000000000000000000000000004000000000000000000000000000015b6d64696100",
"0000206d6468640000000000000000000000000000bb800000000055c400000000002d68646c72000000000000000073",
"6f756e000000000000000000000000536f756e6448616e646c657200000001066d696e6600000010736d686400000000",
"000000000000002464696e660000001c6472656600000000000000010000000c75726c2000000001000000ca7374626c",
"0000007e7374736400000000000000010000006e6d703461000000000000000100000000000000000002001000000000",
"bb8000000000003665736473000000000380808025000200048080801740150000000002710000027100058080800511",
"9056e5000680808001020000001462747274000000000002710000027100000000107374747300000000000000000000",
"0010737473630000000000000000000000147374737a000000000000000000000000000000107374636f000000000000",
"0000000000486d7665780000002074726578000000000000000100000001000000000000000000000000000000207472",
"657800000000000000020000000100000000000000000000000000000062756474610000005a6d657461000000000000",
"002168646c7200000000000000006d6469726170706c0000000000000000000000002d696c737400000025a9746f6f00",
"00001d6461746100000001000000004c61766636322e31322e313033")).expect("retained producer init")
    }

    #[test]
    fn finite_hevc_plan_inspects_served_init_and_freezes_actual_codec() {
        let context = plan_context();
        assert_eq!(context.codecs, "hvc1,mp4a.40.2");
        let init = produced_init();
        let expected = hevc_codec_from_init(&init, "hvc1").expect("actual HEVC profile");
        assert!(expected.starts_with("hvc1.1."));
        let bound =
            bind_hls_init_context(context, &init, "hvc1").expect("structurally complete init");
        assert_eq!(bound.codecs, format!("{expected},mp4a.40.2"));
        let frozen = bound.codec_facts.expect("frozen output facts");
        assert_eq!(frozen.sdr_master_codecs(), Some(bound.codecs));
        let serialized = serde_json::to_value(frozen).expect("origin");
        assert!(serialized.to_string().contains("OutputInit"));
    }

    #[test]
    fn finite_hevc_plan_refuses_incomplete_parameter_sets() {
        let mut init = produced_init();
        let hvcc = init
            .windows(4)
            .position(|bytes| bytes == b"hvcC")
            .expect("record");
        // First array starts after the 23-byte configuration header. Clearing
        // array_completeness must retain the existing hvc1 refusal.
        init[hvcc + 4 + 23] &= 0x7f;
        assert!(bind_hls_init_context(plan_context(), &init, "hvc1").is_err());
    }
}
