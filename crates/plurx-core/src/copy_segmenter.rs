//! The existing GOP-aware copy parser, independent of native publication.
//! A concrete custodian must own the source pipe, sink, and original task join.
use crate::domain::MediaFile;
use crate::fmp4::{
    self, FragmentReader, Init, Published, SegmentCounts, Segmenter, TrackKind, Unit,
};
use crate::transcode::{
    CopyVideoOptions, COPY_FIRST_SEGMENT_SECONDS, COPY_PUBLISH_GATE_SECS, COPY_SEGMENT_MAX_BYTES,
    COPY_SEGMENT_MAX_SECS, COPY_SEGMENT_SECONDS, ROLLING_PRESENTATION_TARGET_SECS,
};
use crate::vod_generation::sanitize_stale_dolby_brand;
use tokio::io::{AsyncRead, AsyncReadExt};
const READ_CHUNK: usize = 256 * 1024;
const MEMORY_WARN_BYTES: usize = 160 * 1024 * 1024;

/// The floor and the two ceilings, as a session sees them.
///
/// A struct rather than three constants read at the point of use, because the
/// tests need to reach a ceiling without a 4K file: the policy is the thing
/// under test, and a 48 MB ceiling is not reachable from a 12-second fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub floor_seconds: u32,
    /// The floor for the first segment only — see
    /// [`plurx_core::fmp4::CutPolicy::first_floor_ticks`].
    pub first_floor_seconds: u32,
    pub max_bytes: usize,
    pub max_seconds: u32,
    /// Fixed covering target advertised before the first rolling response.
    pub target_seconds: u32,
    /// Hold `index.m3u8` until this many seconds of media exist, so playback
    /// starts behind a cushion instead of at the live edge — the whole story
    /// is on [`plurx_core::transcode::COPY_PUBLISH_GATE_SECS`]. Seconds, not
    /// a segment count: a count multiplies by whatever the opening cuts, and
    /// a quiet opening cuts ceiling-length segments. End of stream overrides
    /// the gate: a film shorter than the cushion publishes whole.
    pub publish_gate_secs: u32,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            floor_seconds: COPY_SEGMENT_SECONDS,
            first_floor_seconds: COPY_FIRST_SEGMENT_SECONDS,
            max_bytes: COPY_SEGMENT_MAX_BYTES,
            max_seconds: COPY_SEGMENT_MAX_SECS,
            target_seconds: ROLLING_PRESENTATION_TARGET_SECS,
            publish_gate_secs: COPY_PUBLISH_GATE_SECS,
        }
    }
}

/// How a segmenter session ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The stream was not one this reader could follow. This is a typed shape
    /// fact only: local playlist creation does not prove client visibility.
    /// The actor uses exact response-publication ordering to choose the one
    /// frozen direct-HLS retry before publication or retained failure after
    /// publication.
    Unsupported(String),
    /// The emitted out-of-band HEVC init is not decodable. Retrying through
    /// ffmpeg's legacy HLS muxer would publish the same invalid decoder
    /// configuration, so this failure is terminal even when probe metadata
    /// did not predict that promotion would be needed.
    InvalidHevcConfiguration(String),
    /// The reader consumed the complete pipe and durably published every final
    /// segment plus the exact ENDLIST. Process exit and duration/frontier proof
    /// remain separate actor facts; this outcome alone is not session success.
    Completed(SegmentCounts),
    /// The stream was structurally supported, but the reader could not finish
    /// parsing, merging, or durably publishing it. This never authorizes the
    /// legacy retry; the actor decides discard versus retained failure from
    /// exact attempt-media publication ordering.
    ReaderFailed {
        reason: String,
        counts: SegmentCounts,
    },
    /// Scratch disappeared because lifecycle teardown already owns the
    /// session. This is not producer-failure evidence and must not overwrite
    /// the actor's End/authority/lease verdict.
    Cancelled(SegmentCounts),
}

/// Turn a failed HEVC-init promotion into the copy reader's policy-bearing
/// outcome.
///
/// A multi-entry `stsd` is fallback-safe only when validation has independently
/// inspected every description and proved its decoder configuration complete.
/// The typed layout prevents a matching parser error string from standing in
/// for that proof. Any missing or incomplete hvcC remains terminal.
pub fn hevc_promotion_failure(
    promotion: fmp4::Fmp4Error,
    validation: Result<fmp4::HevcSampleEntryLayout, fmp4::Fmp4Error>,
) -> Outcome {
    let reason = format!("preparing the HLS init segment: {promotion}");
    if matches!(
        &promotion,
        fmp4::Fmp4Error::MultipleHevcSampleEntries { .. }
    ) && matches!(
        &validation,
        Ok(fmp4::HevcSampleEntryLayout::Multiple { .. })
    ) {
        return Outcome::Unsupported(reason);
    }
    match validation {
        Ok(_) => match promotion {
            fmp4::Fmp4Error::Unsupported(_) | fmp4::Fmp4Error::MultipleHevcSampleEntries { .. } => {
                Outcome::Unsupported(reason)
            }
            fmp4::Fmp4Error::Malformed(_) => Outcome::ReaderFailed {
                reason,
                counts: SegmentCounts::default(),
            },
        },
        Err(configuration) => {
            Outcome::InvalidHevcConfiguration(format!("{reason}; {configuration}"))
        }
    }
}

pub enum PublishError {
    Io(std::io::Error),
    Contract(String),
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Contract(reason) => formatter.write_str(reason),
        }
    }
}

impl From<std::io::Error> for PublishError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub trait CopyPublicationSink {
    async fn torn_down(&self) -> bool;
    async fn write_init(&mut self, init: &Init) -> std::io::Result<()>;
    async fn write_segment(&mut self, published: &Published) -> Result<(), PublishError>;
    async fn write_endlist(&mut self) -> std::io::Result<()>;
    fn started(&self) -> bool;
    async fn read_endlist(&self) -> std::io::Result<String>;
}

fn converted_dolby_vision_record(file: &MediaFile) -> Result<fmp4::DolbyVisionRecord, String> {
    let level = file
        .dolby_vision
        .level
        .and_then(|level| u8::try_from(level).ok())
        .ok_or("the source has no stored Dolby Vision level")?;
    fmp4::DolbyVisionRecord::new(8, level, false, true, true, 1).map_err(|error| error.to_string())
}

pub async fn run_copy_pipe<R: AsyncRead + Unpin, S: CopyPublicationSink>(
    mut src: R,
    sink: &mut S,
    session_id: &str,
    limits: Limits,
    source: &MediaFile,
    video: CopyVideoOptions,
) -> Outcome {
    let strip_dolby_vision_record = video.leaves_a_stale_dolby_vision_record(source);
    let retain_hevc_parameter_sets = video.retains_hevc_parameter_sets();
    let mut reader = FragmentReader::new();
    // Hold the initialization segment until the first video sample arrives.
    // ffmpeg may put HDR10's static SEIs only in that sample; Apple needs the
    // same records in hvcC before it will accept a PQ HLS variant.
    let mut pending_init: Option<(Init, fmp4::CutPolicy)> = None;
    let mut segmenter: Option<Segmenter> = None;
    let mut guarded_dvh1_init: Option<Init> = None;
    let mut converter: Option<crate::vod_generation::conversion::Converter> = None;
    let mut buf = vec![0u8; READ_CHUNK];
    let mut warned_memory = false;

    loop {
        let n = match src.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let counts = segmenter
                    .as_ref()
                    .map_or_else(SegmentCounts::default, Segmenter::counts);
                if sink.torn_down().await {
                    tracing::debug!(session = %session_id, "copy segmenter pipe closed after session teardown: {e}");
                    return Outcome::Cancelled(counts);
                }
                return Outcome::ReaderFailed {
                    reason: format!("reading the fragmented MP4 pipe: {e}"),
                    counts,
                };
            }
        };
        reader.push(&buf[..n]);

        loop {
            let unit = match reader.next_unit() {
                Ok(Some(unit)) => unit,
                Ok(None) => break,
                Err(e) => {
                    // The reader reports stream shape, never publication
                    // policy. A local playlist file is not proof that any
                    // response crossed actor authorization; conversely the
                    // actor may already have published media before this
                    // task observes the file. Preserve structural Unsupported
                    // exactly and let the actor choose Retry or
                    // RetainPublished from its ordered response state.
                    if guarded_dvh1_init.is_some() {
                        return Outcome::InvalidHevcConfiguration(e.to_string());
                    }
                    return match e {
                        fmp4::Fmp4Error::Unsupported(reason) => Outcome::Unsupported(reason),
                        error @ fmp4::Fmp4Error::MultipleHevcSampleEntries { .. } => {
                            Outcome::Unsupported(error.to_string())
                        }
                        fmp4::Fmp4Error::Malformed(reason) => Outcome::ReaderFailed {
                            reason: format!("parsing the fragmented MP4 stream: {reason}"),
                            counts: segmenter
                                .as_ref()
                                .map_or_else(SegmentCounts::default, Segmenter::counts),
                        },
                    };
                }
            };
            match unit {
                Unit::Init(mut init) => {
                    if guarded_dvh1_init.is_some() {
                        return Outcome::InvalidHevcConfiguration(
                            "rolling dvh1 decoder configuration changed after publication".into(),
                        );
                    }
                    let Some(init_video) = init.video() else {
                        return Outcome::Unsupported("the pipe's moov has no video track".into());
                    };
                    if init_video.codec.is_none() || init_video.nal_length_size == 0 {
                        return Outcome::Unsupported(
                            "the video track carries no hvcC/avcC to read keyframes with".into(),
                        );
                    }
                    // A copy session maps exactly one video stream and at most
                    // one audio stream, so anything else in the `moov` is
                    // something ffmpeg added on its own — a chapter `text`
                    // track is what it was the first time, and Safari refused
                    // the stream over it while Chrome played on. Report the
                    // shape exactly; the actor may use the known-playable
                    // legacy muxer only while response publication still
                    // permits a retry, and otherwise retains the admitted
                    // timeline as a typed failure.
                    if let Some(odd) = init.tracks.iter().find(|t| t.kind == TrackKind::Other) {
                        return Outcome::Unsupported(format!(
                            "the pipe declared a track this path never asked for \
                             (id {}, timescale {})",
                            odd.id, odd.timescale
                        ));
                    }
                    let video_timescale = init_video.timescale;
                    // FFmpeg 8 repeats one decoder configuration as a second
                    // sample description (see the fmp4 function). Collapse it
                    // here, before the converter and the record removal read
                    // the init; a genuinely different second description is
                    // left alone and refused below exactly as before.
                    match fmp4::collapse_equivalent_hevc_sample_entries(&mut init) {
                        Ok(0) => {}
                        Ok(removed) => tracing::info!(
                            session = %session_id,
                            removed,
                            "collapsed repeated HEVC sample descriptions in the HLS init segment"
                        ),
                        Err(fmp4::Fmp4Error::Malformed(reason)) => {
                            return Outcome::ReaderFailed {
                                reason: format!("preparing the HLS init segment: {reason}"),
                                counts: SegmentCounts::default(),
                            };
                        }
                        Err(error) => {
                            return Outcome::Unsupported(format!(
                                "preparing the HLS init segment: {error}"
                            ));
                        }
                    }
                    if video.converts_dolby_vision() {
                        match crate::vod_generation::conversion::Converter::for_init(&init) {
                            Ok(ready) => converter = Some(ready),
                            Err(refused) => {
                                return Outcome::InvalidHevcConfiguration(format!(
                                    "this stream cannot be converted: {refused}"
                                ))
                            }
                        }
                    }
                    // This path builds its served init itself rather than
                    // through `fmp4::promote_from`, so the removal the VOD
                    // path gets from the stored promotion inputs has to be
                    // asked for here too. It is the same defect either way:
                    // `filter_units` took the Dolby Vision layers out by NAL
                    // type and the DOVI side data the muxer wrote the record
                    // from is not a NAL unit, so on an ffmpeg without
                    // `dovi_rpu` the init declares Profile 7 with an
                    // enhancement layer over a stream carrying neither.
                    //
                    // The removal fixes the `dby1` file-type brand itself, so
                    // the sanitizer below is a no-op after it fires. It runs
                    // anyway, for the sessions where `strip_dolby_vision_record`
                    // is false and ffmpeg's own `dovi_rpu` filter emptied the
                    // sample entry while movenc had already written the brand.
                    if strip_dolby_vision_record {
                        match fmp4::remove_dolby_vision_record(&mut init) {
                            Ok(true) => tracing::info!(
                                session = %session_id,
                                "removed the stale Dolby Vision record from the HLS init segment"
                            ),
                            Ok(false) => {}
                            // Terminal, deliberately, and not `Unsupported`.
                            //
                            // `Unsupported` is the one classification the actor
                            // may retry — `copy_retry_is_unsupported_only_and_\
                            // successor_classifier_is_recipe_owned` in
                            // `playback_control` pins that, and pins that
                            // `InvalidConfiguration` fails outright instead.
                            // The retry it permits is the legacy muxer, which
                            // renders the same half-strip and writes its own
                            // `init.mp4` with nothing to remove the record from
                            // it. Reporting a failed removal that way would
                            // hand the session to the path this removal exists
                            // to keep it off: the viewer would get the stutter
                            // silently instead of an error.
                            //
                            // This choice of variant is NOT pinned by a test,
                            // and it is worth saying so rather than leaving a
                            // reader to assume it is. Reaching it needs an init
                            // that parses as an init and whose removal then
                            // fails, and every corruption that makes
                            // `shrink_box` refuse — a box size smaller than the
                            // record it contains — is one `FragmentReader`
                            // rejects first. The trigger is correspondingly
                            // unlikely in production: the ancestors are movenc's
                            // own `moov/trak/mdia/minf/stbl/stsd/hvc1`, all
                            // 32-bit sized.
                            Err(error) => {
                                return Outcome::InvalidHevcConfiguration(format!(
                                    "the stale Dolby Vision record could not be removed \
                                     from the init: {error}"
                                ))
                            }
                        }
                    }
                    sanitize_stale_dolby_brand(&mut init);
                    let policy = fmp4::CutPolicy::new(
                        limits.floor_seconds,
                        limits.first_floor_seconds,
                        limits.max_bytes,
                        limits.max_seconds,
                        video_timescale,
                    )
                    .with_strict_max_seconds(limits.target_seconds, video_timescale);
                    pending_init = Some((init, policy));
                }
                Unit::Fragment(mut fragment) => {
                    if let Some(converter) = converter.as_mut() {
                        let before = converter.report().source_profile;
                        if let Err(refused) = converter.convert(&mut fragment) {
                            return Outcome::ReaderFailed {
                                reason: format!("this stream cannot be converted: {refused}"),
                                counts: segmenter
                                    .as_ref()
                                    .map_or_else(SegmentCounts::default, Segmenter::counts),
                            };
                        }
                        let report = converter.report();
                        if segmenter.is_none() && report.rpus == 0 {
                            return Outcome::InvalidHevcConfiguration(
                                "this source is recorded as Dolby Vision Profile 7 but its first \
                                 fragment carries no RPUs to convert"
                                    .into(),
                            );
                        }
                        if before.is_none() && report.source_profile.is_some() {
                            tracing::info!(
                                session = %session_id,
                                source_profile = report.source_profile,
                                enhancement_layer = ?report.enhancement_layer,
                                "converting Dolby Vision to Profile 8.1: {}",
                                report.enhancement_layer.reason()
                            );
                        }
                    }
                    if segmenter.is_none() {
                        let Some((mut init, policy)) = pending_init.take() else {
                            return Outcome::ReaderFailed {
                                reason: "a fragment arrived before the moov".into(),
                                counts: SegmentCounts::default(),
                            };
                        };
                        // A rolling source has no whole-film stability proof.
                        // Repair this muxer omission only with a strict per-sample
                        // guard: matching definitions can be stripped; a later
                        // contradiction terminates before its bytes are buffered.
                        let guard_dvh1 = retain_hevc_parameter_sets
                            && !fmp4::PromotionInputs::from_fragment(&fragment, &init)
                                .dvh1_parameter_sets
                                .is_empty();
                        match fmp4::promote_hevc_parameter_sets(&mut init, &fragment) {
                            Ok(true) => tracing::info!(
                                session = %session_id,
                                "promoted in-band HEVC parameter sets into the HLS init segment"
                            ),
                            Ok(false) => {}
                            Err(error) => {
                                return hevc_promotion_failure(
                                    error,
                                    fmp4::validate_hevc_sample_entries(&init),
                                );
                            }
                        }
                        match fmp4::validate_hevc_sample_entries(&init) {
                            Ok(fmp4::HevcSampleEntryLayout::Multiple { count }) => {
                                return Outcome::Unsupported(format!(
                                    "preparing the HLS init segment: {}",
                                    fmp4::Fmp4Error::MultipleHevcSampleEntries { count }
                                ));
                            }
                            Ok(_) => {}
                            Err(error) => {
                                return Outcome::InvalidHevcConfiguration(format!(
                                    "validating the HEVC decoder configuration: {error}"
                                ));
                            }
                        }
                        match fmp4::promote_hdr10_static_metadata(&mut init, &fragment) {
                            Ok(true) => tracing::info!(
                                session = %session_id,
                                "promoted HDR10 static metadata into the HLS init segment"
                            ),
                            Ok(false) => {}
                            Err(fmp4::Fmp4Error::Unsupported(reason)) => {
                                return Outcome::Unsupported(format!(
                                    "preparing the HLS init segment: {reason}"
                                ));
                            }
                            Err(error @ fmp4::Fmp4Error::MultipleHevcSampleEntries { .. }) => {
                                return Outcome::Unsupported(format!(
                                    "preparing the HLS init segment: {error}"
                                ));
                            }
                            Err(fmp4::Fmp4Error::Malformed(reason)) => {
                                return Outcome::ReaderFailed {
                                    reason: format!("preparing the HLS init segment: {reason}"),
                                    counts: SegmentCounts::default(),
                                };
                            }
                        }
                        if video.converts_dolby_vision() {
                            let record = match converted_dolby_vision_record(source) {
                                Ok(record) => record,
                                Err(reason) => return Outcome::InvalidHevcConfiguration(reason),
                            };
                            if let Err(error) = fmp4::set_dolby_vision_record(&mut init, &record) {
                                return Outcome::InvalidHevcConfiguration(format!(
                                    "writing the converted Dolby Vision record: {error}"
                                ));
                            }
                        }
                        if let Err(error) = fmp4::validate_hevc_decoder_configuration(&init) {
                            return Outcome::InvalidHevcConfiguration(format!(
                                "validating the HEVC decoder configuration: {error}"
                            ));
                        }
                        if guard_dvh1 {
                            if let Err(error) =
                                fmp4::validate_rolling_dvh1_fragment(&init, &fragment)
                            {
                                return Outcome::InvalidHevcConfiguration(error.to_string());
                            }
                            guarded_dvh1_init = Some(init.clone());
                            reader.reject_reinitialization();
                        }
                        if let Err(e) = sink.write_init(&init).await {
                            return if sink.torn_down().await {
                                Outcome::Cancelled(SegmentCounts::default())
                            } else {
                                Outcome::ReaderFailed {
                                    reason: format!("writing init.mp4: {e}"),
                                    counts: SegmentCounts::default(),
                                }
                            };
                        }
                        segmenter =
                            Some(Segmenter::new(init, policy).retaining_hevc_parameter_sets(
                                retain_hevc_parameter_sets && !guard_dvh1,
                            ));
                    }
                    if let Some(init) = guarded_dvh1_init.as_ref() {
                        if let Err(error) = fmp4::validate_rolling_dvh1_fragment(init, &fragment) {
                            return Outcome::InvalidHevcConfiguration(error.to_string());
                        }
                    }
                    let Some(seg) = segmenter.as_mut() else {
                        unreachable!("the first fragment constructs the segmenter");
                    };
                    match seg.push(fragment) {
                        Ok(Some(published)) => {
                            if let Err(e) = sink.write_segment(&published).await {
                                if let PublishError::Contract(reason) = e {
                                    return Outcome::Unsupported(reason);
                                }
                                return if sink.torn_down().await {
                                    tracing::debug!(
                                        session = %session_id,
                                        "session directory went away mid-write; stopping"
                                    );
                                    Outcome::Cancelled(seg.counts())
                                } else {
                                    Outcome::ReaderFailed {
                                        reason: format!("writing {}: {e}", published.name()),
                                        counts: seg.counts(),
                                    }
                                };
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            return match e {
                                fmp4::Fmp4Error::Unsupported(reason) => {
                                    Outcome::Unsupported(reason)
                                }
                                error @ fmp4::Fmp4Error::MultipleHevcSampleEntries { .. } => {
                                    Outcome::Unsupported(error.to_string())
                                }
                                fmp4::Fmp4Error::Malformed(reason) => Outcome::ReaderFailed {
                                    reason: format!("merging a fragmented MP4 segment: {reason}"),
                                    counts: seg.counts(),
                                },
                            };
                        }
                    }
                    if !warned_memory {
                        let held = seg.pending_bytes() + reader.buffered();
                        if held > MEMORY_WARN_BYTES {
                            warned_memory = true;
                            tracing::warn!(
                                session = %session_id, held_bytes = held,
                                "copy segmenter is holding more than a segment's worth of \
                                 bytes; the byte ceiling should have cut before here"
                            );
                        }
                    }
                }
                // ffmpeg's random-access index, written at EOF. It describes
                // a file nobody will ever open; publishing it would append
                // bytes to a segment that is already complete.
                Unit::Trailer => {}
            }
        }
    }

    // A clean end is one where everything sent was consumed: either ffmpeg
    // wrote its `mfra` trailer, or the pipe closed on a fragment boundary. A
    // truncated fragment left in hand means the child was killed mid-write,
    // and an ENDLIST there would tell the player the film ends early.
    let complete = reader.saw_trailer() || reader.buffered() == 0;
    if segmenter.is_none() && pending_init.is_some() {
        return Outcome::ReaderFailed {
            reason: "the pipe ended before its first media fragment".into(),
            counts: SegmentCounts::default(),
        };
    }
    finish(segmenter, sink, session_id, complete).await
}

async fn finish<S: CopyPublicationSink>(
    segmenter: Option<Segmenter>,
    sink: &mut S,
    session_id: &str,
    complete: bool,
) -> Outcome {
    let Some(mut seg) = segmenter else {
        return Outcome::ReaderFailed {
            reason: "the pipe ended before its moov arrived".into(),
            counts: SegmentCounts::default(),
        };
    };
    if !complete {
        let counts = seg.counts();
        return Outcome::ReaderFailed {
            reason: "the fragmented MP4 pipe ended inside an incomplete unit".into(),
            counts,
        };
    }
    {
        // `#EXT-X-ENDLIST` says "this is the whole film". Only write it if the
        // last segment actually landed — a playlist terminated one segment
        // short of what was produced tells the player it has everything while
        // the picture stops early, which is worse than a playlist that simply
        // stops growing.
        match seg.finish() {
            Ok(published) => {
                for segment in published {
                    if let Err(e) = sink.write_segment(&segment).await {
                        if let PublishError::Contract(reason) = e {
                            return Outcome::Unsupported(reason);
                        }
                        if sink.torn_down().await {
                            tracing::debug!(
                                session = %session_id,
                                "session directory went away before the final segment; stopping"
                            );
                            return Outcome::Cancelled(seg.counts());
                        }
                        return Outcome::ReaderFailed {
                            reason: format!("writing a final segment: {e}"),
                            counts: seg.counts(),
                        };
                    }
                }
            }
            Err(fmp4::Fmp4Error::Unsupported(reason)) => {
                return Outcome::Unsupported(reason);
            }
            Err(error @ fmp4::Fmp4Error::MultipleHevcSampleEntries { .. }) => {
                return Outcome::Unsupported(error.to_string());
            }
            Err(fmp4::Fmp4Error::Malformed(reason)) => {
                return Outcome::ReaderFailed {
                    reason: format!("merging the final segment: {reason}"),
                    counts: seg.counts(),
                };
            }
        }
        if let Err(e) = sink.write_endlist().await {
            return if sink.torn_down().await {
                Outcome::Cancelled(seg.counts())
            } else {
                Outcome::ReaderFailed {
                    reason: format!("writing the playlist end: {e}"),
                    counts: seg.counts(),
                }
            };
        }
        if !sink.started() {
            return Outcome::ReaderFailed {
                reason: "the reader completed without publishing an ENDLIST".into(),
                counts: seg.counts(),
            };
        }
        let endlist = sink.read_endlist().await;
        match endlist {
            Ok(text) if text.ends_with("#EXT-X-ENDLIST\n") => {}
            Ok(_) => {
                return Outcome::ReaderFailed {
                    reason: "the final playlist did not end with ENDLIST".into(),
                    counts: seg.counts(),
                };
            }
            Err(e) => {
                if sink.torn_down().await {
                    return Outcome::Cancelled(seg.counts());
                }
                return Outcome::ReaderFailed {
                    reason: format!("verifying the final playlist: {e}"),
                    counts: seg.counts(),
                };
            }
        }
    }
    let counts = seg.counts();
    if counts.segments == 0 {
        return Outcome::ReaderFailed {
            reason: "the pipe ended before a single segment".into(),
            counts,
        };
    }
    Outcome::Completed(counts)
}
