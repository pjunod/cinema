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
    mut context: crate::transcode::HlsContext,
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
    let mut reader = plurx_core::fmp4::FragmentReader::new();
    reader.push(&init);
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
        dolby_vision_codec_from_init(&init, sample_entry)
    } else {
        hevc_codec_from_init(&init, sample_entry)
    };
    let video = derived.ok_or_else(HlsInitInspectionError::unsupported)?;
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
