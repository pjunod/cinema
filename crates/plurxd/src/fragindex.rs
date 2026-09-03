//! Building a file's fragment index.
//!
//! Companion to [`plurx_core::segplan`] (what an index *is* and what a plan is
//! made from it) — this is *how one gets built*, which means one ffmpeg child,
//! one pass over the file, and no side effects.
//!
//! The shape is [`crate::copyseg::run`] with the segmenter taken out: the same
//! [`FragmentReader`] over the same production-shaped pipe, recording a row per
//! fragment instead of publishing media. Sharing the reader is deliberate —
//! a second implementation of "what is a fragment" is exactly how an index
//! comes to describe a stream the producer never emits.
//!
//! Two rules the caller inherits and must not soften:
//!
//! - **An index build never blocks a playback.** A file with no index keeps
//!   the legacy live presentation for that watch and converts from the next
//!   one (plan §2.2, ledger D4), so no cold play ever waits on this.
//! - **A partial read is not an index.** ffmpeg dying a third of the way
//!   through a file emits a perfectly well-formed prefix, and an index built
//!   from it would place every later boundary in the wrong part of the film.
//!   [`IndexOutcome::Truncated`] is the only honest answer there, and the
//!   caller retries rather than persisting it.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use plurx_core::domain::MediaFile;
use plurx_core::fmp4::{self, FragmentReader, Init, PromotionInputs, TrackKind, Unit};
use plurx_core::segplan::{FragmentIndex, IndexRow, SourceIdentity};
use plurx_core::transcode;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::ffmpeg::ffmpeg_bin;

#[cfg(unix)]
type SourceFd = std::os::fd::RawFd;
#[cfg(not(unix))]
type SourceFd = i32;

/// Matches [`crate::copyseg::READ_CHUNK`]'s reasoning: large enough that a
/// fast copy is not a syscall storm, small enough that the reader parks in one
/// `read` rather than holding a large buffer.
const READ_CHUNK: usize = 256 * 1024;

type IndexProgress = dyn Fn(u64, i64, usize) + Send + Sync;
type SharedIndexProgress = Arc<IndexProgress>;

/// How an index build ended.
#[derive(Debug, Clone, PartialEq)]
pub enum IndexOutcome {
    /// A complete pass over the file.
    Built(Box<FragmentIndex>),
    /// The pipe ended before the file did — a NAS read that went wrong, or a
    /// per-file budget too optimistic for this disk on this day. Recorded as a
    /// [`plurx_core::segplan::FragmentIndexOutcome`] with how far it got and a
    /// backoff, because a pass that repeats a thirty-minute whole-file read
    /// every wrap of the library is how one slow mount starves every other
    /// title in it.
    Truncated { reason: String, rows: usize },
    /// This file cannot be indexed by this path at all, and retrying will not
    /// change that. Recorded as terminal until the file's own identity
    /// changes, so the background job stops asking.
    Unsupported(String),
}

/// Read one index pipe to exhaustion.
///
/// Generic over the source for the same reason [`crate::copyseg::run`] is:
/// everything between the pipe and the index is worth testing and none of it
/// needs a real child process to be worth testing.
/// `dolby_vision` is **every** answer, and deliberately one parameter rather
/// than a flag per outcome. The three are mutually exclusive by construction:
/// a pass either rewrites the record, deletes it, or leaves it alone, and the
/// pass's rows and its served init have to describe the same stream. Separate
/// flags are how those come to disagree, on a pair nothing downstream would
/// notice — the rows would be byte counts for one stream and the served init a
/// description of another.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn index_stream<R: AsyncRead + Unpin>(
    src: R,
    identity: SourceIdentity,
    expected_ms: Option<i64>,
    dolby_vision: DolbyVisionPass,
) -> IndexOutcome {
    index_stream_with_progress(src, identity, expected_ms, dolby_vision, None).await
}

/// What this index pass does about the muxer's Dolby Vision record.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum DolbyVisionPass {
    /// Leave it. Every ordinary copy, and every preserved Dolby Vision copy:
    /// what the muxer wrote already describes the stream.
    #[default]
    Untouched,
    /// Rewrite it to this record. The Profile 7 → 8.1 conversion, whose RPUs
    /// are rewritten after the muxer, so the record ffmpeg copied out of the
    /// source container describes a stream that no longer exists.
    Rewrite(Box<plurx_core::fmp4::DolbyVisionRecord>),
    /// Delete it. The strip on an ffmpeg without `dovi_rpu`: `filter_units`
    /// removed the RPU and enhancement-layer NAL units by type, and the DOVI
    /// side data the muxer wrote the record from survived, so the output
    /// declares Profile 7 with an enhancement layer over a stream that has
    /// neither.
    Remove,
}

async fn index_stream_with_progress<R: AsyncRead + Unpin>(
    mut src: R,
    identity: SourceIdentity,
    expected_ms: Option<i64>,
    dolby_vision: DolbyVisionPass,
    progress: Option<&IndexProgress>,
) -> IndexOutcome {
    let convert = matches!(dolby_vision, DolbyVisionPass::Rewrite(_));
    // A converting identity's index has to describe the CONVERTED bytes. An
    // index is a list of the producer's own output byte counts and the landing
    // matcher compares them, so an index built from the unconverted stream
    // would not be stale — it would be confidently wrong about media this
    // identity never produces. The conversion shortens every RPU, so every
    // fragment carrying one is a different size.
    let mut converter: Option<crate::dvpipe::Converter> = None;
    let mut reader = FragmentReader::new();
    let mut init: Option<Init> = None;
    let mut init_sha = String::new();
    let mut timescale: u32 = 0;
    let mut rows: Vec<IndexRow> = Vec::new();
    let mut bytes_read = 0_u64;
    let mut covered_ticks = 0_u64;
    // The promotion inputs the whole film's generations will share, taken from
    // the first clean fragment, plus whether every later clean fragment agrees
    // with it. Both are plan §2.2's ruling: capture once, check continuously,
    // and refuse a varying title here rather than mid-playback.
    let mut promotion: Option<PromotionInputs> = None;
    let mut parameter_sets_constant = true;
    let mut buf = vec![0u8; READ_CHUNK];

    loop {
        let read = match src.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) => {
                return IndexOutcome::Truncated {
                    reason: format!("index pipe read: {error}"),
                    rows: rows.len(),
                }
            }
        };
        bytes_read = bytes_read.saturating_add(read as u64);
        reader.push(&buf[..read]);

        loop {
            let unit = match reader.next_unit() {
                Ok(Some(unit)) => unit,
                Ok(None) => break,
                Err(error) => {
                    // Unlike a session, an index has no published playlist to
                    // protect, so there is no point past which a broken stream
                    // becomes something other than "do not index this".
                    return IndexOutcome::Unsupported(format!(
                        "lost the fragment stream after {} fragments: {error}",
                        rows.len()
                    ));
                }
            };
            match unit {
                Unit::Init(parsed) => {
                    let Some(video) = parsed.video() else {
                        return IndexOutcome::Unsupported(
                            "the index pipe's moov has no video track".into(),
                        );
                    };
                    if video.codec.is_none() || video.nal_length_size == 0 {
                        return IndexOutcome::Unsupported(
                            "the video track carries no hvcC/avcC, so no fragment can be \
                             classified"
                                .into(),
                        );
                    }
                    // A video-only pipe declares exactly one track. Anything
                    // else is ffmpeg adding something of its own — a chapter
                    // `text` track is what it was the first time — and its
                    // bytes would land in every fragment's byte count, which
                    // is the number the landing matcher compares.
                    if let Some(extra) = parsed.tracks.iter().find(|t| t.kind != TrackKind::Video) {
                        return IndexOutcome::Unsupported(format!(
                            "the index pipe declared a non-video track (id {}, kind {:?})",
                            extra.id, extra.kind
                        ));
                    }
                    timescale = video.timescale.max(1);
                    init_sha = hex(Sha256::digest(&parsed.bytes));
                    init = Some(parsed);
                }
                Unit::Fragment(fragment) => {
                    let Some(ref init) = init else {
                        return IndexOutcome::Unsupported(
                            "a fragment arrived before the moov".into(),
                        );
                    };
                    let mut fragment = fragment;
                    if convert {
                        if converter.is_none() {
                            match crate::dvpipe::Converter::for_init(init) {
                                Ok(ready) => converter = Some(ready),
                                Err(refused) => {
                                    return IndexOutcome::Unsupported(format!(
                                        "this stream cannot be converted: {refused}"
                                    ))
                                }
                            }
                        }
                        let converter = converter.as_mut().expect("just set");
                        if let Err(refused) = converter.convert(&mut fragment) {
                            return IndexOutcome::Unsupported(format!(
                                "this stream cannot be converted: {refused}"
                            ));
                        }
                    }
                    let fragment = fragment;
                    let Some(video) = init.video() else {
                        return IndexOutcome::Unsupported("the moov lost its video track".into());
                    };
                    let Some(track) = fragment.track(video.id) else {
                        // A video-only pipe emitting a fragment with no video
                        // in it describes nothing the plan can use.
                        return IndexOutcome::Unsupported(
                            "a fragment carried no video track".into(),
                        );
                    };
                    let dts = track.base_decode_time;
                    let duration = fragment.video_duration(init);
                    covered_ticks = covered_ticks.saturating_add(duration);
                    let bytes = u32::try_from(fragment.len()).unwrap_or(u32::MAX);
                    // The landing matcher's quantity. Container overhead is
                    // excluded deliberately: a production generation carries
                    // audio, so its `moof` and `mdat` are tens of kilobytes
                    // larger than this pipe's and vary with the audio track,
                    // while the video samples themselves are copied and come
                    // out byte for byte identical.
                    let video_bytes = u32::try_from(track.byte_len()).unwrap_or(u32::MAX);
                    let class = fmp4::classify(&fragment, init);

                    // Only a clean fragment can begin a segment, so only a
                    // clean fragment can ever be a generation's first — which
                    // makes these the only fragments whose promotion inputs
                    // could ever differ from the stored ones.
                    if class.is_clean() {
                        let here = PromotionInputs::from_fragment(&fragment, init);
                        match promotion {
                            None => promotion = Some(here),
                            Some(ref canonical) => {
                                if &here != canonical {
                                    // A film whose clean starts disagree cannot
                                    // be described by one immutable init. Not a
                                    // failure — a fact, recorded so the title
                                    // keeps the legacy presentation instead of
                                    // being VOD-presented on a promise that
                                    // cannot be kept.
                                    parameter_sets_constant = false;
                                }
                            }
                        }
                    }

                    rows.push(IndexRow {
                        dts,
                        duration,
                        bytes,
                        video_bytes,
                        class,
                    });
                }
                // ffmpeg's random-access index, written at EOF. Seeing it is
                // the strongest evidence the pass was complete.
                Unit::Trailer => {}
            }
        }
        if let Some(progress) = progress {
            let media_ms = if timescale > 0 {
                covered_ticks
                    .saturating_mul(1_000)
                    .saturating_div(u64::from(timescale))
                    .min(i64::MAX as u64) as i64
            } else {
                0
            };
            progress(bytes_read, media_ms, rows.len());
        }
    }

    if rows.is_empty() {
        return IndexOutcome::Unsupported("the index pipe produced no fragments".into());
    }
    // A clean end is one where everything sent was consumed: ffmpeg wrote its
    // `mfra` trailer, or the pipe closed on a fragment boundary. Bytes left in
    // hand mean the child died mid-write.
    if !reader.saw_trailer() && reader.buffered() != 0 {
        return IndexOutcome::Truncated {
            reason: format!("{} bytes left mid-fragment", reader.buffered()),
            rows: rows.len(),
        };
    }
    if let Some(expected_ms) = expected_ms {
        let covered: u64 = rows.iter().map(|row| row.duration).sum();
        let expected = (expected_ms.max(0) as u64).saturating_mul(u64::from(timescale)) / 1000;
        // Two seconds of slack. The probe's duration and the video track's
        // summed sample durations are two measurements of the same film and
        // they routinely disagree by the last frame or two; a container whose
        // header rounds disagrees by more.
        let slack = u64::from(timescale) * 2;
        if covered + slack < expected {
            return IndexOutcome::Truncated {
                reason: format!("covered {covered} of {expected} ticks"),
                rows: rows.len(),
            };
        }
    }

    // A converting pass that rewrote nothing is a pass whose file's stored
    // facts are wrong: the row says Profile 7 and the stream carries no RPUs
    // at all. Indexing it as the converted identity would record that lie in
    // the keyspace, and every session looking that identity up would be served
    // segments cut for a stream that was never converted.
    if convert {
        let report = converter.as_ref().map(|c| c.report()).unwrap_or_default();
        if report.rpus == 0 {
            return IndexOutcome::Unsupported(
                "this source is recorded as Dolby Vision Profile 7 but its stream carries no \
                 RPUs to convert"
                    .into(),
            );
        }
        // After the refusal, not before it. The index pass reads every RPU in
        // the file, so its answer is the whole film's rather than one
        // session's opening fragment's — and it is what the conversion costs a
        // viewer, since MEL carries no picture detail of its own while FEL
        // carries real residual detail. A line logged on the failure path
        // would report a default MEL/FEL answer for a pass that read no RPU at
        // all, which is worse than saying nothing: this is what an operator
        // reading a "why does this look softer" report has to go on.
        tracing::info!(
            rpus = report.rpus,
            source_profile = report.source_profile,
            enhancement_layer = ?report.enhancement_layer,
            "indexed a converted stream: {}",
            report.enhancement_layer.reason()
        );
    }
    let mut promotion = promotion.unwrap_or_default();
    // The record cannot come out of a fragment: what the muxer wrote describes
    // the source, not the converted stream, which is the whole reason plurx
    // supplies its own. It rides in the stored promotion inputs so the served
    // init this pass validates and the served init a later generation promotes
    // are produced by the same function from the same facts.
    match dolby_vision {
        DolbyVisionPass::Untouched => {}
        DolbyVisionPass::Rewrite(record) => promotion.dolby_vision = Some(*record),
        DolbyVisionPass::Remove => promotion.strip_dolby_vision = true,
    }
    let Some(mut served_init) = init.clone() else {
        return IndexOutcome::Unsupported(
            "the index pipe ended without an init to validate".into(),
        );
    };
    if let Err(error) = fmp4::promote_from(&mut served_init, &promotion) {
        return IndexOutcome::Unsupported(format!(
            "the served init could not be built from this pass's promotion inputs: {error}"
        ));
    }
    if let Err(error) = fmp4::validate_hevc_decoder_configuration(&served_init) {
        return IndexOutcome::Unsupported(format!(
            "validating the HEVC decoder configuration: {error}"
        ));
    }

    let mut built = FragmentIndex::new(timescale, rows, init_sha, identity);
    built.promotion = promotion;
    built.parameter_sets_constant = parameter_sets_constant;
    IndexOutcome::Built(Box::new(built))
}

/// The Dolby Vision configuration record a converted stream must declare.
///
/// Built from the source's stored facts rather than read from the output,
/// because what the output carries is the *source's* record: ffmpeg derives it
/// from its input container rather than from the RPUs (measured,
/// `docs/PLAYBACK-CAPS-V2-M0.md` §8), and the rewrite that makes the RPUs say
/// 8.1 runs on the far side of that muxer. Left alone, the sample entry would
/// declare Profile 7 over samples that are no longer Profile 7.
///
/// What changes from the source's own record and what does not:
///
/// - **profile becomes 8**, which is what the RPUs now say;
/// - **`el_present` becomes false**, because `filter_units=remove_types=63`
///   dropped the enhancement layer and a record still declaring one tells a
///   decoder to expect a layer that is not in the stream;
/// - **the level and the compatibility id are the source's**, unchanged. The
///   level bounds resolution and frame rate, neither of which the conversion
///   touches; the compatibility id says what a non-Dolby-Vision client sees of
///   the base layer, and the base layer is copied byte for byte.
fn converted_dolby_vision_record(
    file: &MediaFile,
) -> Result<plurx_core::fmp4::DolbyVisionRecord, String> {
    let level = file
        .dolby_vision
        .level
        .and_then(|level| u8::try_from(level).ok())
        .ok_or("the source has no stored Dolby Vision level")?;
    let compat = file
        .dolby_vision
        .bl_compat_id
        .and_then(|id| u8::try_from(id).ok())
        .ok_or("the source has no stored base-layer compatibility id")?;
    plurx_core::fmp4::DolbyVisionRecord::new(8, level, false, true, true, compat)
        .map_err(|error| error.to_string())
}

/// The identity a file's index is keyed by, for this build of ffmpeg.
pub fn identity_for(file: &MediaFile, video: transcode::CopyVideoOptions) -> SourceIdentity {
    SourceIdentity::new(
        file.size.max(0) as u64,
        file.mtime,
        plurx_core::segplan::argv_fingerprint(&transcode::copy_video_args(file, video)),
    )
}

/// Every copy-video pipeline a real client can ask this file for, in the order
/// they should be built.
///
/// The indexer used to answer one pipeline — the Dolby-Vision-stripped one —
/// for every file, while `vodserve` and `/decision` build a session's identity
/// from the *session's* `preserve_dolby_vision`. A DV-capable client therefore
/// asked for a byte stream nothing had ever indexed, got `vod_index_pending`,
/// and was rescued by the temporary live-HLS recovery path on every play
/// (PLAYBACK-CAPS-V2-PLAN §4.7, edge E1).
///
/// The stripped identity stays **first**. It is what every non-DV client and
/// every non-DV file uses, and what a forced rebuild resolves to, so a library
/// that is fully indexed today does not re-order its work to adopt this.
///
/// Deduplicated by fingerprint rather than by [`transcode::CopyVideoOptions`]
/// equality, so that an option which happens to render to an argv another
/// option already produced costs nothing. Nothing collapses today — even an
/// ffmpeg with no `dovi_rpu` filter still tags the two differently — but the
/// index keyspace is what a session looks itself up in, and a duplicate there
/// is a whole redundant pass over a 60 GB remux.
///
/// Profile 5 is deliberately in the set even though it has no HDR10 base to
/// strip to. [`plurx_core::playback::decide`] never routes it to a stripping
/// copy, but `decide_forced` with `Force::Original` does, and that copy is
/// indexed today — dropping it would regress a path that works.
///
/// **The Profile 7 → 8.1 conversion is the third identity**, and it is here
/// for the same reason the preserved one is: a converting session builds its
/// identity from its own `CopyVideoOptions`, so an index built only for the
/// other two would leave it looking up something nothing built. It is added
/// only when the *file* could convert — Profile 7 over an HDR10 base — because
/// which clients convert is a per-session question and an index is per-file.
///
/// `convert` is the node's answer, not the file's: an operator who turned the
/// conversion off (`PLURX_DV_CONVERT=0`) has no converting sessions to serve,
/// and indexing for them would spend a third full pass over every Profile 7
/// remux in the library on a stream nothing can ask for.
pub fn video_identities(
    file: &MediaFile,
    probe_json: Option<&str>,
    have_dovi: bool,
    convert: bool,
) -> Vec<transcode::CopyVideoOptions> {
    let preserve_choices: &[bool] = if plurx_core::playback::is_dolby_vision(file) {
        &[false, true]
    } else {
        &[false]
    };
    let mut identities = Vec::with_capacity(preserve_choices.len() + 1);
    let mut seen = std::collections::HashSet::new();
    for preserve in preserve_choices {
        let video = transcode::CopyVideoOptions::from_probe(file, probe_json, have_dovi, *preserve);
        if seen.insert(identity_for(file, video).argv_fingerprint) {
            identities.push(video);
        }
    }
    if convert && plurx_core::playback::file_can_convert_to_p81(file) {
        let video = transcode::CopyVideoOptions::from_probe(file, probe_json, have_dovi, true)
            .with_dolby_vision_conversion(true);
        if seen.insert(identity_for(file, video).argv_fingerprint) {
            identities.push(video);
        }
    }
    identities
}

/// What this pass must do about the muxer's Dolby Vision record.
///
/// A free function, not three lines inside the pipe builder, because the pipe
/// builder spawns ffmpeg and cannot be driven from a test — and this choice is
/// the whole of the fix on both sides of it. Left inline, deleting it changes
/// no test.
fn dolby_vision_pass_for(
    file: &MediaFile,
    video: transcode::CopyVideoOptions,
) -> Result<DolbyVisionPass, String> {
    if video.converts_dolby_vision() {
        // A converting pass produces a stream whose sample entry declares the
        // *source's* record: ffmpeg copies it from the input container, and
        // the rewrite that makes the RPUs say 8.1 runs after that muxer. So
        // plurx builds the right one from the source's own stored facts.
        return converted_dolby_vision_record(file)
            .map(|record| DolbyVisionPass::Rewrite(Box::new(record)))
            .map_err(|reason| {
                format!(
                    "a converting index needs a Dolby Vision record and this file cannot \
                     describe one: {reason}"
                )
            });
    }
    if video.leaves_a_stale_dolby_vision_record(file) {
        // The mirror image, and it lands here for the same reason: what the
        // muxer wrote describes the source, not the stream this pass produces.
        // `filter_units` took the RPU and enhancement-layer NAL units out by
        // type, and the DOVI side data ffmpeg copied from the source container
        // is not a NAL unit, so the output declares Profile 7 with an
        // enhancement layer over a stream carrying neither. Chrome ignores it;
        // VideoToolbox believes it, and Safari answers 4K10 HEVC so labelled
        // with a software decode on hardware that has a block for it.
        return Ok(DolbyVisionPass::Remove);
    }
    Ok(DolbyVisionPass::Untouched)
}

/// One index pass's whole recipe: the argv ffmpeg receives, and what plurx
/// must do afterwards to the Dolby Vision record that argv leaves behind.
///
/// A struct rather than two values threaded side by side, and that is the
/// point. The argv and the record answer are one decision read off one
/// `CopyVideoOptions`; carried separately, a caller can build the argv and
/// drop the answer, and the result is a pass whose bytes and whose served init
/// describe different streams — with nothing to notice, because each half is
/// individually correct. The only way to get the argv is to get both.
struct IndexPass {
    args: Vec<String>,
    dolby_vision: DolbyVisionPass,
    /// The options all three of the above were derived from, carried so the
    /// index key is derived from them too. Passed separately, a caller could
    /// hand the runner a recipe built from one set of options and an identity
    /// computed from another — the rows would be byte counts for one stream
    /// filed under another's key, which is the same class of mismatch as the
    /// argv and the record answer disagreeing.
    video: transcode::CopyVideoOptions,
}

fn index_pass(
    file: &MediaFile,
    video: transcode::CopyVideoOptions,
    input: Option<&str>,
) -> Result<IndexPass, String> {
    Ok(IndexPass {
        args: match input {
            Some(path) => transcode::copy_index_pipe_args_with_input(file, path, video),
            None => transcode::copy_index_pipe_args(file, video),
        },
        dolby_vision: dolby_vision_pass_for(file, video)?,
        video,
    })
}

/// Build a file's index by running the index pipe.
///
/// `budget` bounds the whole pass. An index is background work; a NAS read
/// that has gone pathological should give the slot back rather than hold it
/// until the process restarts.
pub async fn build(
    file: &MediaFile,
    video: transcode::CopyVideoOptions,
    runtime_cache: &Path,
    budget: Duration,
) -> IndexOutcome {
    let pass = match index_pass(file, video, None) {
        Ok(pass) => pass,
        Err(reason) => return IndexOutcome::Unsupported(reason),
    };
    build_with_args(file, pass, None, runtime_cache, budget, None).await
}

/// Build from the exact file descriptor whose complete digest was observed.
/// The parent retains ownership; the child receives a duplicate as fd 3.
#[cfg(unix)]
#[allow(dead_code)]
pub async fn build_from_attested_file(
    file: &MediaFile,
    source: &std::fs::File,
    video: transcode::CopyVideoOptions,
    runtime_cache: &Path,
    budget: Duration,
) -> IndexOutcome {
    use std::os::fd::AsRawFd;

    let pass = match index_pass(file, video, Some("/dev/fd/3")) {
        Ok(pass) => pass,
        Err(reason) => return IndexOutcome::Unsupported(reason),
    };
    build_with_args(
        file,
        pass,
        Some(source.as_raw_fd()),
        runtime_cache,
        budget,
        None,
    )
    .await
}

#[cfg(unix)]
pub async fn build_from_attested_file_with_progress<F>(
    file: &MediaFile,
    source: &std::fs::File,
    video: transcode::CopyVideoOptions,
    runtime_cache: &Path,
    budget: Duration,
    progress: F,
) -> IndexOutcome
where
    F: Fn(u64, i64, usize) + Send + Sync + 'static,
{
    use std::os::fd::AsRawFd;

    let pass = match index_pass(file, video, Some("/dev/fd/3")) {
        Ok(pass) => pass,
        Err(reason) => return IndexOutcome::Unsupported(reason),
    };
    build_with_args(
        file,
        pass,
        Some(source.as_raw_fd()),
        runtime_cache,
        budget,
        Some(Arc::new(progress)),
    )
    .await
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub async fn build_from_attested_file(
    file: &MediaFile,
    _source: &std::fs::File,
    video: transcode::CopyVideoOptions,
    runtime_cache: &Path,
    budget: Duration,
) -> IndexOutcome {
    build(file, video, runtime_cache, budget).await
}

#[cfg(not(unix))]
pub async fn build_from_attested_file_with_progress<F>(
    file: &MediaFile,
    _source: &std::fs::File,
    video: transcode::CopyVideoOptions,
    runtime_cache: &Path,
    budget: Duration,
    progress: F,
) -> IndexOutcome
where
    F: Fn(u64, i64, usize) + Send + Sync + 'static,
{
    let pass = match index_pass(file, video, None) {
        Ok(pass) => pass,
        Err(reason) => return IndexOutcome::Unsupported(reason),
    };
    build_with_args(
        file,
        pass,
        None,
        runtime_cache,
        budget,
        Some(Arc::new(progress)),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn build_with_args(
    file: &MediaFile,
    pass: IndexPass,
    source_fd: Option<SourceFd>,
    runtime_cache: &Path,
    budget: Duration,
    progress: Option<SharedIndexProgress>,
) -> IndexOutcome {
    let IndexPass {
        args,
        dolby_vision,
        video,
    } = pass;
    let identity = identity_for(file, video);
    // The probe's duration, carried in so a short read is caught. Passed in
    // milliseconds and converted against the pipe's own timescale inside the
    // reader, because the timescale is not known until the moov arrives.
    let expected_ms = file.duration_ms.filter(|ms| *ms > 0);
    let started = Instant::now();

    let mut command = tokio::process::Command::new(ffmpeg_bin());
    crate::transcode::configure_ffmpeg_runtime(&mut command, runtime_cache);
    #[cfg(unix)]
    if let Some(source_fd) = source_fd {
        unsafe {
            command.pre_exec(move || {
                let duplicate = libc::fcntl(source_fd, libc::F_DUPFD_CLOEXEC, 10);
                if duplicate == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(duplicate, 3) == -1 {
                    libc::close(duplicate);
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(duplicate);
                let flags = libc::fcntl(3, libc::F_GETFD);
                if flags == -1 || libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut child = match command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return IndexOutcome::Truncated {
                reason: format!("spawning the index pipe: {error}"),
                rows: 0,
            }
        }
    };
    let Some(stdout) = child.stdout.take() else {
        return IndexOutcome::Truncated {
            reason: "the index pipe started without a stdout".into(),
            rows: 0,
        };
    };

    // stderr must be drained on its own task or ffmpeg blocks on a full pipe
    // and the whole build deadlocks — the same discipline `spawn_ffmpeg_pipe`
    // keeps for a live session.
    if let Some(stderr) = child.stderr.take() {
        let path = file.path.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(file = %path.display(), "index pipe: {line}");
            }
        });
    }

    let outcome = match tokio::time::timeout(
        budget,
        index_stream_with_progress(
            stdout,
            identity,
            expected_ms,
            dolby_vision,
            progress.as_deref(),
        ),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => IndexOutcome::Truncated {
            reason: format!("exceeded the {}s index budget", budget.as_secs()),
            rows: 0,
        },
    };
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    let outcome = match (outcome, status) {
        (IndexOutcome::Built(_), Ok(Ok(status))) if !status.success() => IndexOutcome::Truncated {
            reason: format!("index pipe exited with {status}"),
            rows: 0,
        },
        (outcome, Ok(Ok(_))) => outcome,
        (IndexOutcome::Built(_), Ok(Err(error))) => IndexOutcome::Truncated {
            reason: format!("waiting for the index pipe: {error}"),
            rows: 0,
        },
        (IndexOutcome::Built(_), Err(_)) => {
            let _ = child.start_kill();
            IndexOutcome::Truncated {
                reason: "index pipe did not exit after closing stdout".to_owned(),
                rows: 0,
            }
        }
        (outcome, _) => {
            let _ = child.start_kill();
            outcome
        }
    };
    if let IndexOutcome::Built(ref index) = outcome {
        tracing::info!(
            file_id = file.id,
            fragments = index.rows.len(),
            timescale = index.timescale,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "built a fragment index"
        );
    }
    outcome
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::segplan::{self, SEGPLAN_VERSION};
    use plurx_core::testfixtures;

    /// The record a converting pass supplies — and, being `Some`, the way a
    /// caller says that this pass converts at all.
    fn converting_record() -> plurx_core::fmp4::DolbyVisionRecord {
        plurx_core::fmp4::DolbyVisionRecord::new(8, 6, false, true, true, 1).expect("record")
    }

    fn identity() -> SourceIdentity {
        SourceIdentity::new(1, 1, "fingerprint")
    }

    fn hevc_file(hdr: Option<&str>, hdr_format: Option<&str>) -> MediaFile {
        MediaFile {
            id: 77,
            item_id: 1,
            path: std::path::PathBuf::from("/library/film.mkv"),
            size: 60_000_000_000,
            mtime: 1_700_000_000_000,
            duration_ms: Some(7_200_000),
            container: Some("mkv".into()),
            video_codec: Some("hevc".into()),
            video_profile: Some("Main 10".into()),
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            hdr: hdr.map(str::to_owned),
            hdr_format: hdr_format.map(str::to_owned),
            bitrate: Some(60_000_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    fn fingerprints_of(file: &MediaFile, have_dovi: bool) -> Vec<String> {
        video_identities(file, None, have_dovi, false)
            .into_iter()
            .map(|video| identity_for(file, video).argv_fingerprint)
            .collect()
    }

    #[test]
    fn a_plain_file_has_one_pipeline_to_index() {
        let file = hevc_file(None, None);
        assert_eq!(video_identities(&file, None, true, false).len(), 1);
        assert!(
            !video_identities(&file, None, true, false)[0].preserves_dolby_vision(),
            "the stripped identity stays first, so a fully indexed library \
             does not re-order its work to adopt the identity set"
        );
    }

    #[test]
    fn a_dolby_vision_file_is_indexed_for_both_the_strip_and_the_envelope() {
        let file = hevc_file(
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
        );
        let videos = video_identities(&file, None, true, false);
        assert_eq!(videos.len(), 2);
        assert!(!videos[0].preserves_dolby_vision());
        assert!(videos[1].preserves_dolby_vision());

        let held = fingerprints_of(&file, true);
        assert_ne!(
            held[0], held[1],
            "the preserved envelope and the strip are different byte streams, \
             which is why one index could never answer for both"
        );
    }

    /// The converting identity is added only for a file that can actually be
    /// converted, and only on a node that will.
    ///
    /// Both halves are load-bearing in opposite directions. Dropping the file
    /// check would index a third pipeline for every Dolby Vision title in the
    /// library — a full extra pass over a 60 GB remux each — for a stream no
    /// session can ask for. Dropping the node check would do the same on a
    /// node where an operator turned the conversion off.
    #[test]
    fn the_converting_identity_is_indexed_only_when_it_can_be_served() {
        let mut p7 = hevc_file(
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
        );
        p7.dolby_vision.profile = Some(7);
        p7.dolby_vision.level = Some(6);
        p7.dolby_vision.bl_compat_id = Some(1);

        let converting = video_identities(&p7, None, true, true);
        assert_eq!(converting.len(), 3, "strip, preserve, convert");
        assert!(converting[2].converts_dolby_vision());
        assert!(
            converting[0..2].iter().all(|v| !v.converts_dolby_vision()),
            "the stripped identity stays first so a fully indexed library does \
             not re-order its work to adopt this"
        );

        // Three different byte streams, so three different indexes. Sharing
        // one would hand a session a playlist whose cut points describe media
        // it never produces.
        let fingerprints: std::collections::HashSet<_> = converting
            .iter()
            .map(|video| identity_for(&p7, *video).argv_fingerprint)
            .collect();
        assert_eq!(fingerprints.len(), 3);

        // The node switch stands it down.
        assert_eq!(video_identities(&p7, None, true, false).len(), 2);

        // …and so does a file the conversion cannot describe. A Profile 8
        // source is already what the conversion produces; a row with no
        // columns cannot have its configuration record built at all.
        let p8 = {
            let mut file = p7.clone();
            file.dolby_vision.profile = Some(8);
            file
        };
        assert_eq!(video_identities(&p8, None, true, true).len(), 2);
        let label_only = hevc_file(
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
        );
        assert_eq!(video_identities(&label_only, None, true, true).len(), 2);
    }

    /// The converted stream's configuration record, from the source's facts.
    ///
    /// What its output carries is the source's own record — ffmpeg copies the
    /// one the input container had — so every field here is either changed
    /// deliberately or carried deliberately, and getting either wrong is a
    /// stream that describes itself incorrectly with nothing to catch it.
    #[test]
    fn the_converted_record_says_profile_eight_with_no_enhancement_layer() {
        let mut file = hevc_file(Some("dolby_vision"), None);
        file.dolby_vision.profile = Some(7);
        file.dolby_vision.level = Some(9);
        file.dolby_vision.bl_compat_id = Some(6);

        let record = converted_dolby_vision_record(&file).expect("describable");
        assert_eq!(record.profile, 8, "the RPUs now say 8.1");
        assert!(
            !record.el_present,
            "the copy's bitstream filter dropped the enhancement layer; a \
             record still declaring one tells a decoder to expect what is not \
             there"
        );
        assert!(record.rpu_present, "the RPUs are the point");
        assert!(record.bl_present);
        assert_eq!(
            record.level, 9,
            "the level bounds resolution and frame rate, neither of which the \
             conversion touches"
        );
        assert_eq!(
            record.bl_signal_compatibility_id, 6,
            "the base layer is copied byte for byte, so what a non-DV client \
             sees of it is unchanged"
        );

        // A row that cannot describe one refuses rather than inventing values.
        let mut levelless = file.clone();
        levelless.dolby_vision.level = None;
        assert!(converted_dolby_vision_record(&levelless).is_err());
    }

    #[test]
    fn profile_five_keeps_its_stripping_identity_too() {
        // `decide` never routes a Profile 5 source to a stripping copy -- it
        // has no HDR10 base -- but `decide_forced(Force::Original)` does, and
        // that copy is indexed today. The plan's M1 acceptance check expects
        // one row here; dropping the second would regress a live path.
        let file = hevc_file(Some("dolby_vision"), Some("Dolby Vision · Profile 5"));
        assert_eq!(video_identities(&file, None, true, false).len(), 2);
    }

    #[test]
    fn an_ffmpeg_that_cannot_strip_still_offers_two_identities() {
        // Worth pinning because it is the opposite of what it looks like:
        // without the `dovi_rpu` filter nothing removes the RPU, so it would
        // be easy to assume the two pipelines collapse into one. They do not.
        // The sample entry tag is the same `hvc1` either way for a source with
        // an HDR10-compatible base, but `-strict unofficial` and the bitstream
        // filter chain both still differ, and those decide the emitted bytes.
        let file = hevc_file(
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
        );
        let held = fingerprints_of(&file, false);
        assert_eq!(held.len(), 2);
        assert_ne!(held[0], held[1]);
    }

    /// A stripping pass on an ffmpeg without `dovi_rpu` asks for the record to
    /// be removed, and the ask reaches the stored promotion inputs.
    ///
    /// The wire between "this filter chain leaves a record describing a stream
    /// that no longer exists" and "no served init carries it". Without it the
    /// removal happens nowhere: the muxer writes a Profile 7 record with an
    /// enhancement layer over a stream with neither, VideoToolbox believes it,
    /// and Safari software-decodes 4K10 HEVC on hardware built to do it in
    /// silicon.
    #[test]
    fn a_strip_without_dovi_rpu_asks_the_promotion_to_remove_the_record() {
        let dv = hevc_file(
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
        );
        let plain = hevc_file(None, None);

        let strip_no_bsf = transcode::CopyVideoOptions::new(false, false);
        assert!(
            strip_no_bsf.leaves_a_stale_dolby_vision_record(&dv),
            "the one branch that half-strips"
        );

        // Every neighbouring answer is a whole one and needs no help.
        assert!(
            !transcode::CopyVideoOptions::new(true, false).leaves_a_stale_dolby_vision_record(&dv),
            "dovi_rpu removes the side data, so the muxer writes no record"
        );
        assert!(
            !transcode::CopyVideoOptions::new(false, true).leaves_a_stale_dolby_vision_record(&dv),
            "a preserved stream's record is true"
        );
        assert!(
            !transcode::CopyVideoOptions::new(false, true)
                .with_dolby_vision_conversion(true)
                .leaves_a_stale_dolby_vision_record(&dv),
            "a conversion rewrites the record rather than removing it"
        );
        assert!(
            !strip_no_bsf.leaves_a_stale_dolby_vision_record(&plain),
            "a source with no Dolby Vision has no record to be stale"
        );

        // …and the pass the index pipe actually selects from it. This is the
        // half that was untested: with the selection inline in the pipe
        // builder, which spawns ffmpeg and cannot be driven from a test, the
        // whole fix could be deleted and the suite stayed green.
        assert_eq!(
            dolby_vision_pass_for(&dv, strip_no_bsf).expect("a strip needs no record"),
            DolbyVisionPass::Remove
        );
        assert_eq!(
            dolby_vision_pass_for(&dv, transcode::CopyVideoOptions::new(true, false))
                .expect("dovi_rpu"),
            DolbyVisionPass::Untouched
        );
        assert_eq!(
            dolby_vision_pass_for(&dv, transcode::CopyVideoOptions::new(false, true))
                .expect("preserve"),
            DolbyVisionPass::Untouched
        );
        assert_eq!(
            dolby_vision_pass_for(&plain, strip_no_bsf).expect("not Dolby Vision"),
            DolbyVisionPass::Untouched
        );

        // A conversion rewrites rather than removes — and asks for a record
        // this file can describe.
        let mut p7 = dv.clone();
        p7.dolby_vision.profile = Some(7);
        p7.dolby_vision.level = Some(6);
        p7.dolby_vision.bl_compat_id = Some(1);
        let converting =
            transcode::CopyVideoOptions::new(false, true).with_dolby_vision_conversion(true);
        assert!(matches!(
            dolby_vision_pass_for(&p7, converting).expect("describable"),
            DolbyVisionPass::Rewrite(_)
        ));
    }

    /// A probe describing the minimal 23-byte hvcC — the WEB-DL Matroska
    /// shape whose parameter sets live only in band, and the only one
    /// `hevc_parameter_set_promotion_required` fires on.
    const MINIMAL_HVCC_PROBE: &str = r#"{"streams":[{"codec_type":"video","extradata_size":23}]}"#;

    /// The argv and the record answer are one recipe, for every identity the
    /// indexer actually enumerates.
    ///
    /// This is the invariant, and it is asserted against the rendered argv
    /// rather than restated: a pass removes the record exactly when its own
    /// filter chain strips the Dolby Vision NAL units without asking ffmpeg to
    /// drop the side data too. Carrying the two halves separately is how they
    /// come apart — each is individually correct, and together they describe
    /// two different streams with nothing to notice.
    #[test]
    fn every_index_pass_agrees_with_the_argv_it_carries() {
        let mut removing = 0;
        let mut promoting = 0;
        // Each row carries the columns its label describes. Stamping Profile 7
        // over all of them made the "Profile 5" row a duplicate of the Profile
        // 7 one — `file_can_convert_to_p81` reads the columns, not the label —
        // so a real Profile 5 file, which has no compatible base and therefore
        // one identity fewer, was never enumerated.
        for (hdr, label, profile, compat) in [
            (
                Some("dolby_vision"),
                Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
                Some(7i64),
                Some(1i64),
            ),
            (
                Some("dolby_vision"),
                Some("Dolby Vision · Profile 5"),
                Some(5),
                Some(0),
            ),
            (Some("hdr10"), Some("HDR10"), None, None),
            (None, None, None, None),
        ] {
            let mut file = hevc_file(hdr, label);
            file.dolby_vision.profile = profile;
            file.dolby_vision.level = profile.map(|_| 6i64);
            file.dolby_vision.bl_compat_id = compat;
            for have_dovi in [false, true] {
                for convert in [false, true] {
                    // `probe_json` both ways. `Some` turns on parameter-set
                    // promotion, which is the branch of `copy_video_args` that
                    // hand-rolls its filter list instead of calling
                    // `hevc_copy_bsf_for_copy` — the one place the two
                    // builders could most plausibly disagree.
                    for probe in [None, Some(MINIMAL_HVCC_PROBE)] {
                        for video in video_identities(&file, probe, have_dovi, convert) {
                            let pass = index_pass(&file, video, None).expect("a describable pass");
                            let filter = pass
                                .args
                                .windows(2)
                                .find(|pair| pair[0] == "-bsf:v")
                                .map(|pair| pair[1].clone())
                                .unwrap_or_default();
                            let leaves_a_record =
                                filter.contains("62-63") && !filter.contains("dovi_rpu");
                            assert_eq!(
                                pass.dolby_vision == DolbyVisionPass::Remove,
                                leaves_a_record,
                                "hdr={hdr:?} label={label:?} dovi={have_dovi} convert={convert} \
                                 promote={} rendered {filter}",
                                video.promotes_parameter_sets()
                            );
                            if leaves_a_record {
                                removing += 1;
                            }
                            if video.promotes_parameter_sets() {
                                promoting += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(
            removing > 0,
            "no enumerated identity reached the removing branch, so the \
             equality above held vacuously"
        );
        assert!(
            promoting > 0,
            "no enumerated identity reached the parameter-set-promotion \
             branch, which is the filter list the two builders could disagree in"
        );
    }

    /// A `Remove` pass reaches the stored promotion inputs, and an ordinary
    /// one does not.
    ///
    /// The wire the fix is, end to end within this module: the predicate above
    /// says the compensation is needed, and this says the ask survives to the
    /// place every served init — live and regenerated — is promoted from.
    /// Without it the whole fix can be deleted from `build_with_args` and the
    /// suite stays green.
    #[tokio::test]
    async fn a_removing_pass_stores_the_ask_in_the_promotion_inputs() {
        testfixtures::require_ffmpeg();
        let bytes = index_pipe_bytes("closed-gop");

        let removing = index_stream(
            std::io::Cursor::new(bytes.clone()),
            identity(),
            None,
            DolbyVisionPass::Remove,
        )
        .await;
        let IndexOutcome::Built(removing) = removing else {
            panic!("{removing:?}");
        };
        assert!(
            removing.promotion.strip_dolby_vision,
            "a removing pass must record the ask, or nothing removes the record"
        );
        assert!(
            removing.promotion.dolby_vision.is_none(),
            "and must not also ask for a rewrite, which promotion refuses"
        );

        let ordinary = index_stream(
            std::io::Cursor::new(bytes),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await;
        let IndexOutcome::Built(ordinary) = ordinary else {
            panic!("{ordinary:?}");
        };
        assert!(
            !ordinary.promotion.strip_dolby_vision,
            "every other pass leaves the muxer's record alone"
        );
    }

    /// The record the caller supplies reaches the stored promotion inputs, and
    /// therefore the served init every later generation is promoted to.
    ///
    /// This is the wire between "plurx builds a Dolby Vision record because
    /// ffmpeg cannot" and "every served init carries it". Without it the
    /// record is built, discarded, and the converted stream tells every client
    /// it is plain HDR10 — the whole milestone delivering nothing, with no
    /// error anywhere.
    #[tokio::test]
    async fn the_supplied_dolby_vision_record_reaches_the_stored_promotion() {
        testfixtures::require_ffmpeg();
        let bytes = index_pipe_bytes("closed-gop");

        let plain = index_stream(
            std::io::Cursor::new(bytes.clone()),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await;
        let IndexOutcome::Built(plain) = plain else {
            panic!("{plain:?}");
        };
        assert!(
            plain.promotion.dolby_vision.is_none(),
            "an ordinary pass supplies none"
        );

        let record =
            plurx_core::fmp4::DolbyVisionRecord::new(8, 6, false, true, true, 1).expect("record");
        // Supplying the record *is* asking for the conversion, so the stream
        // has to be one there is something to convert in.
        let converted = index_stream(
            std::io::Cursor::new(testfixtures::with_dolby_vision_rpus(&bytes)),
            identity(),
            None,
            DolbyVisionPass::Rewrite(Box::new(record.clone())),
        )
        .await;
        let IndexOutcome::Built(converted) = converted else {
            panic!("{converted:?}");
        };
        assert_eq!(
            converted.promotion.dolby_vision,
            Some(record),
            "the record is stored, so a regenerated init is promoted from it too"
        );
        assert!(!converted.promotion.is_empty());
    }

    /// The conversion runs inside the index pass, and the rows describe the
    /// converted stream.
    ///
    /// This is the wire the whole converting identity hangs from. The index a
    /// converting session lands against has to have been built from converted
    /// fragments: an 8.1 RPU is smaller than the Profile 7 one it replaces, so
    /// every `video_bytes` differs, and an index built without the conversion
    /// would describe a stream no converting session ever produces — the
    /// landing would miss on every fragment and the session would fail with
    /// nothing pointing at why.
    #[tokio::test]
    async fn a_converting_pass_indexes_the_converted_stream() {
        testfixtures::require_ffmpeg();
        let plain_bytes = index_pipe_bytes("closed-gop");
        let dv_bytes = testfixtures::with_dolby_vision_rpus(&plain_bytes);

        let unconverted = index_stream(
            std::io::Cursor::new(dv_bytes.clone()),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await;
        let IndexOutcome::Built(unconverted) = unconverted else {
            panic!("a Dolby Vision stream indexes without converting: {unconverted:?}");
        };

        let converted = index_stream(
            std::io::Cursor::new(dv_bytes),
            identity(),
            None,
            DolbyVisionPass::Rewrite(Box::new(converting_record())),
        )
        .await;
        let IndexOutcome::Built(converted) = converted else {
            panic!("the converting pass must build: {converted:?}");
        };

        assert_eq!(
            converted.rows.len(),
            unconverted.rows.len(),
            "the conversion rewrites metadata, it does not add or drop frames"
        );
        for (with, without) in converted.rows.iter().zip(unconverted.rows.iter()) {
            assert_eq!(with.dts, without.dts, "no timestamp is reconstructed");
            assert_eq!(with.duration, without.duration);
            assert!(
                with.video_bytes < without.video_bytes,
                "every fragment shrinks by what the 8.1 RPUs no longer carry"
            );
        }
    }

    /// A converting pass over a stream with no RPUs in it refuses.
    ///
    /// The row said Profile 7 and the stream carries nothing to convert, so
    /// one of the two is lying. Indexing it under the converted identity would
    /// record that lie in the keyspace, and every session that looked the
    /// identity up would be served segments cut for a stream that was never
    /// converted.
    #[tokio::test]
    async fn a_converting_pass_over_a_stream_with_no_rpus_is_not_an_index() {
        testfixtures::require_ffmpeg();
        let bytes = index_pipe_bytes("closed-gop");
        let outcome = index_stream(
            std::io::Cursor::new(bytes),
            identity(),
            None,
            DolbyVisionPass::Rewrite(Box::new(converting_record())),
        )
        .await;
        let IndexOutcome::Unsupported(reason) = outcome else {
            panic!("a stream with no RPUs cannot be indexed as converted: {outcome:?}");
        };
        assert!(reason.contains("no RPUs to convert"), "{reason}");
    }

    /// The index pipe over a real fixture, read the way the daemon reads it.
    async fn index_fixture(kind: &str) -> IndexOutcome {
        let bytes = index_pipe_bytes(kind);
        index_stream(
            std::io::Cursor::new(bytes),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await
    }

    /// The video-only pipe's own output, cached beside the fixture.
    pub(super) fn index_pipe_bytes(kind: &str) -> Vec<u8> {
        let source = testfixtures::source(kind);
        let mut command = std::process::Command::new(testfixtures::ffmpeg());
        command
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&source)
            .args([
                "-map_chapters",
                "-1",
                "-map",
                "0:v:0?",
                "-an",
                "-sn",
                "-c:v",
                "copy",
            ]);
        if kind != "h264" && kind != "vp9" {
            command
                .args(["-tag:v", "hvc1"])
                .args(["-bsf:v", "filter_units=remove_types=32-34"]);
        }
        command
            .args(["-avoid_negative_ts", "make_zero"])
            .args([
                "-movflags",
                "frag_keyframe+empty_moov+default_base_moof+delay_moov",
            ])
            .args(["-use_editlist", "0", "-f", "mp4", "pipe:1"]);
        testfixtures::run(&mut command)
    }

    fn replace_hvcc_array_type(bytes: &mut [u8], from: u8, to: u8) {
        let kind_at = bytes
            .windows(4)
            .position(|window| window == b"hvcC")
            .expect("hvcC box");
        let payload = kind_at + 4;
        let arrays = usize::from(bytes[payload + 22]);
        let mut pos = payload + 23;
        for _ in 0..arrays {
            let array_kind = bytes[pos] & 0x3f;
            let count = u16::from_be_bytes([bytes[pos + 1], bytes[pos + 2]]) as usize;
            if array_kind == from {
                bytes[pos] = (bytes[pos] & 0xc0) | to;
                return;
            }
            pos += 3;
            for _ in 0..count {
                let len = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as usize;
                pos += 2 + len;
            }
        }
        panic!("hvcC carried no type-{from} array");
    }

    #[tokio::test]
    async fn a_real_pipe_reports_its_parameter_sets_constant() {
        // The scan-time check plan §2.2's ruling asks for. Every clean
        // fragment of a single-pass encode carries the same parameter sets, so
        // the film is VOD-presentable.
        let IndexOutcome::Built(index) = index_fixture("closed-gop").await else {
            panic!("the closed-gop fixture must index");
        };
        assert!(
            index.parameter_sets_constant,
            "a single-pass encode's clean starts must agree"
        );
        // And this corpus carries nothing promotable -- the test pipe strips
        // types 32-34 the way `copy_video_args` does for ordinary HEVC, so
        // promotion is a no-op and there is nothing to capture. Asserted
        // rather than assumed, because a corpus that silently started
        // carrying them would make the test above pass for a different reason.
        assert!(
            index.promotion.is_empty(),
            "ordinary HEVC carries its parameter sets in hvcC, not in band"
        );
    }

    #[tokio::test]
    async fn emitted_hvc1_refuses_an_incomplete_decoder_configuration_without_a_probe_hint() {
        let mut bytes = index_pipe_bytes("closed-gop");
        // Keep the hvcC structurally valid but turn its PPS array into a
        // duplicate SPS array. The ordinary fixture pipe has already removed
        // in-band sets, so there is no hidden PPS from which to "succeed".
        replace_hvcc_array_type(&mut bytes, 34, 33);
        let outcome = index_stream(
            std::io::Cursor::new(bytes),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await;
        let IndexOutcome::Unsupported(reason) = outcome else {
            panic!("an incomplete emitted hvcC must not be indexed: {outcome:?}");
        };
        assert!(reason.contains("complete VPS/SPS/PPS"), "{reason}");
    }

    #[tokio::test]
    async fn a_film_whose_clean_starts_disagree_is_not_vod_presentable() {
        // Built by hand, because no fixture can produce it: the corpus is
        // single-invocation encodes, which are structurally incapable of
        // per-IDR parameter-set variation. The check still has to work.
        let bytes = index_pipe_bytes("closed-gop");
        let IndexOutcome::Built(index) = index_stream(
            std::io::Cursor::new(bytes),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await
        else {
            panic!("must index");
        };
        // A constant film, as the fixture is.
        assert!(index.parameter_sets_constant);

        // Now the same walk with one start disagreeing. `PromotionInputs`
        // compares by value, so this is the exact comparison `index_stream`
        // makes.
        use plurx_core::fmp4::PromotionInputs;
        let canonical = PromotionInputs {
            strip_dolby_vision: false,
            dolby_vision: None,
            parameter_sets: vec![vec![0x40, 0x01, 0x0c]],
            hdr10_sei: Vec::new(),
        };
        let differing = PromotionInputs {
            strip_dolby_vision: false,
            dolby_vision: None,
            parameter_sets: vec![vec![0x40, 0x01, 0x0d]],
            hdr10_sei: Vec::new(),
        };
        assert_ne!(
            canonical, differing,
            "the comparison that decides VOD-presentability must see the \
             difference between two parameter sets that differ by one byte"
        );
    }

    #[tokio::test]
    async fn a_real_pipe_indexes_every_fragment_it_carries() {
        let IndexOutcome::Built(index) = index_fixture("closed-gop").await else {
            panic!("the closed-gop fixture must index");
        };
        assert!(index.rows.len() > 3, "got {} rows", index.rows.len());
        assert!(index.timescale > 0);
        assert_eq!(index.version, SEGPLAN_VERSION);
        assert_eq!(index.init_sha256.len(), 64);
        assert!(
            index
                .rows
                .iter()
                .all(|row| row.bytes > 0 && row.duration > 0),
            "every row describes real bytes and real time"
        );
        // Contiguous: each fragment starts where the last one ended, which is
        // what lets a plan entry's start be an index row's DTS.
        for pair in index.rows.windows(2) {
            assert_eq!(pair[0].dts + pair[0].duration, pair[1].dts);
        }
    }

    #[tokio::test]
    async fn the_index_is_deterministic() {
        let first = index_fixture("closed-gop").await;
        let second = index_fixture("closed-gop").await;
        assert_eq!(first, second, "two passes must describe the same file");
    }

    #[tokio::test]
    async fn an_open_gop_fixture_is_classified_dirty_and_a_closed_one_clean() {
        let IndexOutcome::Built(open) = index_fixture("open-gop").await else {
            panic!("open-gop must index");
        };
        let IndexOutcome::Built(closed) = index_fixture("closed-gop").await else {
            panic!("closed-gop must index");
        };
        assert!(
            closed.rows.iter().filter(|row| row.clean()).count() > closed.rows.len() / 2,
            "a closed GOP is clean nearly everywhere"
        );
        assert!(
            open.rows.iter().filter(|row| !row.clean()).count() > open.rows.len() / 2,
            "an open GOP with leading pictures is dirty nearly everywhere"
        );
    }

    #[tokio::test]
    async fn a_truncated_pipe_is_not_an_index() {
        // The failure that matters: ffmpeg dies partway and emits a perfectly
        // well-formed prefix. An index built from it places every later
        // boundary in the wrong part of the film.
        let bytes = index_pipe_bytes("closed-gop");
        let half = bytes.len() / 2;
        let outcome = index_stream(
            std::io::Cursor::new(bytes[..half].to_vec()),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await;
        assert!(
            matches!(outcome, IndexOutcome::Truncated { .. }),
            "a half-read pipe must not produce an index, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_pipe_that_stops_short_of_the_probed_duration_is_truncated() {
        let bytes = index_pipe_bytes("closed-gop");
        let IndexOutcome::Built(full) = index_stream(
            std::io::Cursor::new(bytes.clone()),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await
        else {
            panic!("the full pipe indexes");
        };
        let _covered: u64 = full.rows.iter().map(|row| row.duration).sum();
        // Claim the file is a minute longer than it is: the coverage check is
        // the only thing standing between a short read and a wrong index.
        let outcome = index_stream(
            std::io::Cursor::new(bytes),
            identity(),
            Some(60_000 + 12_000),
            DolbyVisionPass::Untouched,
        )
        .await;
        assert!(
            matches!(outcome, IndexOutcome::Truncated { .. }),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn an_index_builds_a_plan_whose_entries_land_on_index_rows() {
        let IndexOutcome::Built(index) = index_fixture("closed-gop").await else {
            panic!("closed-gop must index");
        };
        let policy = fmp4::CutPolicy::new(2, 1, 64 * 1024 * 1024, 15, index.timescale);
        let tracks = segplan::TrackDurations {
            video_ms: 12_000,
            audio_ms: 12_000,
            audio_bits_per_second: 256_000,
        };
        let plan = segplan::plan_copy(&index, &policy, &tracks);
        assert!(!plan.is_empty());
        for entry in plan
            .entries
            .iter()
            .filter(|entry| entry.kind == segplan::PlanEntryKind::Video)
        {
            assert!(
                index.rows.iter().any(|row| row.dts == entry.start_ticks),
                "plan entry {} starts at {} ticks, which is not a fragment boundary",
                entry.index,
                entry.start_ticks
            );
        }
    }

    #[tokio::test]
    async fn a_plan_from_a_persisted_index_builds_well_under_a_tenth_of_a_second() {
        let IndexOutcome::Built(index) = index_fixture("closed-gop").await else {
            panic!("closed-gop must index");
        };
        // Blow the corpus up to feature length so the measurement means
        // something: 12 s of fixture is not a 2-hour film.
        let mut rows = Vec::new();
        let mut dts = 0;
        for _ in 0..600 {
            for row in &index.rows {
                rows.push(IndexRow { dts, ..row.clone() });
                dts += row.duration;
            }
        }
        let big = FragmentIndex::new(index.timescale, rows, "sha", identity());
        let policy = fmp4::CutPolicy::new(6, 2, 64 * 1024 * 1024, 15, big.timescale);
        let tracks = segplan::TrackDurations {
            video_ms: 7_200_000,
            audio_ms: 7_200_000,
            audio_bits_per_second: 256_000,
        };
        let started = Instant::now();
        let plan = segplan::plan_copy(&big, &policy, &tracks);
        let elapsed = started.elapsed();
        assert!(!plan.is_empty());
        assert!(
            elapsed < Duration::from_millis(100),
            "planning {} fragments took {elapsed:?}; the plan is built on every \
             create and must not be felt",
            big.rows.len()
        );
    }

    #[tokio::test]
    async fn a_landing_in_a_real_index_resolves_uniquely() {
        let IndexOutcome::Built(index) = index_fixture("closed-gop").await else {
            panic!("closed-gop must index");
        };
        assert!(index.rows.len() >= 5, "need a few fragments to land in");
        for start in 0..index.rows.len().saturating_sub(3) {
            let observed: Vec<u32> = index.rows[start..start + 3]
                .iter()
                .map(|row| row.video_bytes)
                .collect();
            assert_eq!(
                segplan::match_landing(&index, &observed),
                Ok(start),
                "a real fixture's fragment byte counts must place a landing"
            );
        }
    }
}

#[cfg(test)]
mod equality_tests {
    use super::*;
    use plurx_core::testfixtures;

    /// The equality the whole landing mechanism rests on, asserted by
    /// `cargo test` rather than by a script that ran once.
    ///
    /// M0 §12.2 measured that the video-only index pipe and the full
    /// production pipe agree fragment for fragment. Not on everything: the
    /// wire length differs by tens of kilobytes because the production pipe
    /// carries audio. What matches is the video sample bytes, and that is what
    /// `segplan::match_landing` compares — so if this test ever fails, a
    /// repositioned producer can no longer be placed and every far seek turns
    /// into a typed producer failure.
    #[tokio::test]
    async fn the_index_records_the_quantity_a_production_generation_reproduces() {
        for kind in ["closed-gop", "open-gop", "clean-cra", "h264"] {
            let IndexOutcome::Built(index) = index_stream(
                std::io::Cursor::new(super::tests::index_pipe_bytes(kind)),
                SourceIdentity::new(1, 1, "fingerprint"),
                None,
                DolbyVisionPass::Untouched,
            )
            .await
            else {
                panic!("{kind}: the index pipe must index");
            };

            // The production pipe, audio and all — what a repositioned
            // generation really emits. It cannot go through `index_stream`,
            // which refuses a non-video track on purpose, so it is read
            // directly here.
            let (produced_video, produced_wire) = production_fragments(kind);

            assert_eq!(
                index.rows.len(),
                produced_video.len(),
                "{kind}: fragment counts must agree"
            );
            let video: Vec<u32> = index.rows.iter().map(|row| row.video_bytes).collect();
            assert_eq!(
                video, produced_video,
                "{kind}: video sample bytes are what the landing matcher \
                 compares, so they must be identical across audio branches"
            );

            let wire: Vec<u32> = index.rows.iter().map(|row| row.bytes).collect();
            assert_ne!(
                wire, produced_wire,
                "{kind}: if the wire lengths ever DID agree, the fixture stopped \
                 carrying audio and this test stopped proving anything"
            );
        }
    }

    /// The production pipe's per-fragment (video sample bytes, wire length).
    fn production_fragments(kind: &str) -> (Vec<u32>, Vec<u32>) {
        let bytes = testfixtures::pipe(kind);
        let mut reader = FragmentReader::new();
        reader.push(&bytes);
        let mut init: Option<Init> = None;
        let mut video = Vec::new();
        let mut wire = Vec::new();
        while let Ok(Some(unit)) = reader.next_unit() {
            match unit {
                Unit::Init(parsed) => init = Some(parsed),
                Unit::Fragment(fragment) => {
                    let init = init.as_ref().expect("moov before fragments");
                    let track_id = init.video().expect("a video track").id;
                    let track = fragment.track(track_id).expect("video in the fragment");
                    video.push(u32::try_from(track.byte_len()).unwrap_or(u32::MAX));
                    wire.push(u32::try_from(fragment.len()).unwrap_or(u32::MAX));
                }
                Unit::Trailer => {}
            }
        }
        (video, wire)
    }
}
