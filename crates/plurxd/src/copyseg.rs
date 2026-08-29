//! The GOP-aware copy segmenter: plurx cuts the segments, not ffmpeg.
//!
//! ffmpeg writes one continuous fragmented MP4 down a pipe (one fragment per
//! GOP, `crate::transcode` spawns it with [`plurx_core::transcode::copy_pipe_args`]);
//! this task reads that stream, and publishes a segment boundary only in front
//! of a fragment whose first frame is a true random-access point. Everything
//! it writes — `init.mp4`, `segNNNNN.m4s`, `index.m3u8` — has the same names,
//! the same shape and the same tmp-then-rename semantics ffmpeg's HLS muxer
//! gave it, so the serving layer, the segment index, the ahead-window suspend
//! and the GC all carry on unchanged. The playlist is the interface — which
//! is what lets the publish gate hold it back until a cushion of media
//! exists ([`plurx_core::transcode::COPY_PUBLISH_GATE_SECS`]): until the
//! playlist is out, the rest of the daemon agrees nothing has happened.
//!
//! Why bother: every boundary on an open-GOP copy costs exactly one dropped
//! frame, because a player treats a segment's first keyframe as a
//! random-access point and the HEVC spec says to discard the leading pictures
//! there (docs/STUTTER-4K.md §5.3ter). The cutting is the only part of a copy
//! that plurx can change, so the cutting is what changes.
//!
//! The decision logic is all in [`plurx_core::fmp4`] and is pure. The growing
//! session I/O is retained behind the `live-hls-recovery` feature so an
//! unavailable VOD prerequisite can use the proven compatibility path while
//! the cluster finishes its index backfill.

#[cfg(any(test, feature = "live-hls-recovery"))]
use std::path::{Path, PathBuf};

use plurx_core::fmp4::Init;
#[cfg(any(test, feature = "live-hls-recovery"))]
use plurx_core::fmp4::{
    self, FragmentReader, Published, SegmentCounts, Segmenter, TrackKind, Unit,
};
#[cfg(any(test, feature = "live-hls-recovery"))]
use plurx_core::transcode::{
    COPY_FIRST_SEGMENT_SECONDS, COPY_PUBLISH_GATE_SECS, COPY_SEGMENT_MAX_BYTES,
    COPY_SEGMENT_MAX_SECS, COPY_SEGMENT_SECONDS,
};
#[cfg(any(test, feature = "live-hls-recovery"))]
use tokio::io::{AsyncRead, AsyncReadExt};

/// How much pipe is read at a time. Big enough that a 12 MB/s copy is not a
/// syscall storm, small enough that a SIGSTOPped ffmpeg leaves the reader
/// parked in one `read` rather than holding a large buffer.
#[cfg(any(test, feature = "live-hls-recovery"))]
const READ_CHUNK: usize = 256 * 1024;

/// A reader holding more than this has stopped making sense: the byte ceiling
/// bounds a published segment, so the only bytes in hand should be one pending
/// segment plus one fragment in flight. Exceeding it is logged once — it does
/// not stop the session, because a stream that is merely unusual is still a
/// stream someone is watching.
///
/// What a healthy session actually costs, since the number should be honest:
/// the pending fragments (up to the floor's worth of media — ~52 MB at the
/// 69 Mb/s reference bitrate) plus, for the instant the merge runs, a second
/// copy of the same bytes in the merged buffer. So peak is roughly 2× a
/// segment, ~105 MB, and this threshold sits above that on purpose: it is
/// meant to catch a policy that has stopped cutting, not to complain about
/// the copy every merge makes.
#[cfg(any(test, feature = "live-hls-recovery"))]
const MEMORY_WARN_BYTES: usize = 160 * 1024 * 1024;

/// The floor and the two ceilings, as a session sees them.
///
/// A struct rather than three constants read at the point of use, because the
/// tests need to reach a ceiling without a 4K file: the policy is the thing
/// under test, and a 48 MB ceiling is not reachable from a 12-second fixture.
#[cfg(any(test, feature = "live-hls-recovery"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub floor_seconds: u32,
    /// The floor for the first segment only — see
    /// [`plurx_core::fmp4::CutPolicy::first_floor_ticks`].
    pub first_floor_seconds: u32,
    pub max_bytes: usize,
    pub max_seconds: u32,
    /// Hold `index.m3u8` until this many seconds of media exist, so playback
    /// starts behind a cushion instead of at the live edge — the whole story
    /// is on [`plurx_core::transcode::COPY_PUBLISH_GATE_SECS`]. Seconds, not
    /// a segment count: a count multiplies by whatever the opening cuts, and
    /// a quiet opening cuts ceiling-length segments. End of stream overrides
    /// the gate: a film shorter than the cushion publishes whole.
    pub publish_gate_secs: u32,
}

#[cfg(any(test, feature = "live-hls-recovery"))]
impl Default for Limits {
    fn default() -> Limits {
        Limits {
            floor_seconds: COPY_SEGMENT_SECONDS,
            first_floor_seconds: COPY_FIRST_SEGMENT_SECONDS,
            max_bytes: COPY_SEGMENT_MAX_BYTES,
            max_seconds: COPY_SEGMENT_MAX_SECS,
            publish_gate_secs: COPY_PUBLISH_GATE_SECS,
        }
    }
}

/// How a segmenter session ended.
#[cfg(any(test, feature = "live-hls-recovery"))]
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

/// Everything a session writes, and the playlist it keeps in step.
///
/// The playlist is rewritten whole on every segment rather than appended to,
/// because a reader must never see it half-written: the file is built in
/// memory, written to `index.m3u8.tmp` and renamed over. At 6 s a segment a
/// three-hour film is 1800 lines — rewriting 50 KB per segment costs nothing
/// next to the 50 MB of segment that triggered it.
///
/// The first write is gated: segments land on disk from the start, but
/// `index.m3u8` is withheld until `gate_secs` of media exist, so the first
/// playlist a player loads already holds a cushion and playback starts behind
/// the live edge rather than at it
/// ([`plurx_core::transcode::COPY_PUBLISH_GATE_SECS`]). While the gate is
/// closed nothing downstream knows the segments exist — the segment index,
/// the ahead-window suspend and the GC all read the playlist — and nothing
/// can be served from them, because a client only learns names from the
/// playlist too.
#[cfg(any(test, feature = "live-hls-recovery"))]
struct SessionDir {
    dir: PathBuf,
    /// The `#EXTINF`/URI pairs, without the header. The header is regenerated
    /// on every write because `TARGETDURATION` grows with the longest segment
    /// published so far.
    entries: String,
    /// Media written so far, summed from every published segment's real
    /// duration — what the publish gate measures.
    published_secs: f64,
    /// Withhold `index.m3u8` until this many seconds of media exist.
    gate_secs: u32,
    /// `ceil` of the longest `EXTINF` so far. Never decreases.
    target_duration: u32,
    /// The playlist is on disk — the moment a player could be holding this
    /// timeline, and so the moment the legacy fallback stops being safe.
    started: bool,
}

#[cfg(any(test, feature = "live-hls-recovery"))]
impl SessionDir {
    fn new(dir: PathBuf, gate_secs: u32) -> SessionDir {
        SessionDir {
            dir,
            entries: String::new(),
            published_secs: 0.0,
            gate_secs,
            target_duration: 0,
            started: false,
        }
    }

    fn playlist(&self, end: bool) -> String {
        let mut out = fmp4::playlist_header(self.target_duration.max(1));
        out.push_str(&self.entries);
        if end {
            out.push_str("#EXT-X-ENDLIST\n");
        }
        out
    }

    /// tmp + rename, the semantics the whole daemon assumes: a segment or a
    /// playlist is either absent or complete, never partial.
    async fn publish_file(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        let tmp = self.dir.join(format!("{name}.tmp"));
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, self.dir.join(name)).await
    }

    async fn write_init(&mut self, init: &Init) -> std::io::Result<()> {
        self.publish_file("init.mp4", &init.bytes).await?;
        // No playlist yet: one with no segment in it is a promise the session
        // cannot keep if ffmpeg dies in the next second. The actor's first-
        // media admission observes exactly this file, so it lands only when
        // the publish gate opens.
        Ok(())
    }

    async fn write_segment(&mut self, published: &Published) -> std::io::Result<()> {
        let name = published.name();
        self.publish_file(&name, &published.segment.bytes).await?;
        self.entries
            .push_str(&fmp4::playlist_entry(published.seconds, &name));
        self.published_secs += published.seconds;
        // Grows, never shrinks. A client that read a smaller number and then
        // met a longer segment would be entitled to complain; one that read a
        // number far larger than any real segment waits that long between
        // playlist fetches, which is the stall this replaced.
        let need = published.seconds.ceil().max(1.0) as u32;
        self.target_duration = self.target_duration.max(need);
        // The publish gate. The segment is on disk; whether the world learns
        // of it is a separate decision, taken once: until `gate_secs` of
        // media exist there is no playlist at all, and the first one a player
        // loads already lists the whole cushion.
        if !self.started && self.published_secs < f64::from(self.gate_secs) {
            return Ok(());
        }
        self.started = true;
        let text = self.playlist(false);
        self.publish_file("index.m3u8", text.as_bytes()).await
    }

    async fn write_endlist(&mut self) -> std::io::Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }
        // End of stream opens the gate no matter how few segments exist: a
        // film shorter than the cushion still has to play, and a complete
        // playlist with its ENDLIST is a *better* first read than a live one
        // — the client sees VOD from the start.
        self.started = true;
        let text = self.playlist(true);
        self.publish_file("index.m3u8", text.as_bytes()).await
    }
}

/// Did this write fail because the session was torn down under us?
///
/// Every teardown path removes the session directory (`Session::discard_dir`),
/// and the reader can still be finalizing when it does — a viewer who presses
/// stop, or a player that supersedes its own session, races the last segment
/// out of existence. That is a session ending normally, not a fault, and
/// logging it at ERROR taught the log to cry wolf on the most ordinary event
/// there is. Observed within an hour of the first deploy.
#[cfg(any(test, feature = "live-hls-recovery"))]
fn session_gone(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
}

/// A missing object is lifecycle cancellation only when teardown removed the
/// session directory itself. A missing temp/final file inside a live directory
/// is a producer write failure and must stay visible to the actor.
#[cfg(any(test, feature = "live-hls-recovery"))]
async fn session_directory_gone(dir: &Path) -> bool {
    matches!(
        tokio::fs::metadata(dir).await,
        Err(ref error) if session_gone(error)
    )
}

/// Read `src` to exhaustion, publishing segments into `dir`.
///
/// Generic over the source so the tests can drive a whole session from a byte
/// slice: everything this does between the pipe and the disk is worth testing,
/// and none of it needs a real child process to be worth testing.
#[cfg(any(test, feature = "live-hls-recovery"))]
pub async fn run<R: AsyncRead + Unpin>(
    mut src: R,
    dir: PathBuf,
    session_id: &str,
    limits: Limits,
) -> Outcome {
    match tokio::fs::metadata(&dir).await {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Outcome::ReaderFailed {
                reason: "the copy session scratch path is not a directory".into(),
                counts: SegmentCounts::default(),
            };
        }
        Err(error) => {
            return Outcome::ReaderFailed {
                reason: format!("opening the copy session directory: {error}"),
                counts: SegmentCounts::default(),
            };
        }
    }
    let mut reader = FragmentReader::new();
    let mut out = SessionDir::new(dir, limits.publish_gate_secs);
    // Hold the initialization segment until the first video sample arrives.
    // ffmpeg may put HDR10's static SEIs only in that sample; Apple needs the
    // same records in hvcC before it will accept a PQ HLS variant.
    let mut pending_init: Option<(Init, fmp4::CutPolicy)> = None;
    let mut segmenter: Option<Segmenter> = None;
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
                if session_directory_gone(&out.dir).await {
                    tracing::debug!(session = %crate::transcode::session_log_id(session_id), "copy segmenter pipe closed after session teardown: {e}");
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
                    return match e {
                        fmp4::Fmp4Error::Unsupported(reason) => Outcome::Unsupported(reason),
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
                    let Some(video) = init.video() else {
                        return Outcome::Unsupported("the pipe's moov has no video track".into());
                    };
                    if video.codec.is_none() || video.nal_length_size == 0 {
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
                    let video_timescale = video.timescale;
                    sanitize_stale_dolby_brand(&mut init);
                    let policy = fmp4::CutPolicy::new(
                        limits.floor_seconds,
                        limits.first_floor_seconds,
                        limits.max_bytes,
                        limits.max_seconds,
                        video_timescale,
                    );
                    pending_init = Some((init, policy));
                }
                Unit::Fragment(fragment) => {
                    if segmenter.is_none() {
                        let Some((mut init, policy)) = pending_init.take() else {
                            return Outcome::ReaderFailed {
                                reason: "a fragment arrived before the moov".into(),
                                counts: SegmentCounts::default(),
                            };
                        };
                        match fmp4::promote_hevc_parameter_sets(&mut init, &fragment) {
                            Ok(true) => tracing::info!(
                                session = %crate::transcode::session_log_id(session_id),
                                "promoted in-band HEVC parameter sets into the HLS init segment"
                            ),
                            Ok(false) => {}
                            Err(e) => {
                                let reason = format!("preparing the HLS init segment: {e}");
                                return match fmp4::validate_hevc_decoder_configuration(&init) {
                                    Ok(()) => match e {
                                        fmp4::Fmp4Error::Unsupported(_) => {
                                            Outcome::Unsupported(reason)
                                        }
                                        fmp4::Fmp4Error::Malformed(_) => Outcome::ReaderFailed {
                                            reason,
                                            counts: SegmentCounts::default(),
                                        },
                                    },
                                    Err(configuration) => Outcome::InvalidHevcConfiguration(
                                        format!("{reason}; {configuration}"),
                                    ),
                                };
                            }
                        }
                        match fmp4::promote_hdr10_static_metadata(&mut init, &fragment) {
                            Ok(true) => tracing::info!(
                                session = %crate::transcode::session_log_id(session_id),
                                "promoted HDR10 static metadata into the HLS init segment"
                            ),
                            Ok(false) => {}
                            Err(fmp4::Fmp4Error::Unsupported(reason)) => {
                                return Outcome::Unsupported(format!(
                                    "preparing the HLS init segment: {reason}"
                                ));
                            }
                            Err(fmp4::Fmp4Error::Malformed(reason)) => {
                                return Outcome::ReaderFailed {
                                    reason: format!("preparing the HLS init segment: {reason}"),
                                    counts: SegmentCounts::default(),
                                };
                            }
                        }
                        if let Err(error) = fmp4::validate_hevc_decoder_configuration(&init) {
                            return Outcome::InvalidHevcConfiguration(format!(
                                "validating the HEVC decoder configuration: {error}"
                            ));
                        }
                        if let Err(e) = out.write_init(&init).await {
                            return if session_directory_gone(&out.dir).await {
                                Outcome::Cancelled(SegmentCounts::default())
                            } else {
                                Outcome::ReaderFailed {
                                    reason: format!("writing init.mp4: {e}"),
                                    counts: SegmentCounts::default(),
                                }
                            };
                        }
                        segmenter = Some(Segmenter::new(init, policy));
                    }
                    let Some(seg) = segmenter.as_mut() else {
                        unreachable!("the first fragment constructs the segmenter");
                    };
                    match seg.push(fragment) {
                        Ok(Some(published)) => {
                            if let Err(e) = out.write_segment(&published).await {
                                return if session_directory_gone(&out.dir).await {
                                    tracing::debug!(
                                        session = %crate::transcode::session_log_id(session_id),
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
                                session = %crate::transcode::session_log_id(session_id), held_bytes = held,
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
    finish(segmenter, &mut out, session_id, complete).await
}

/// Remove a Dolby Vision file-type claim after ffmpeg has removed the Dolby
/// configuration and RPUs from the video sample entry.
///
/// ffmpeg 7.1's `dovi_rpu=strip=1` correctly removes `dvcC`/`dvvC` and the RPU
/// NALs, but movenc has already copied `dby1` into `ftyp` by the time the
/// bitstream filter's output parameters reach it. AVPlayer treats that as a
/// contradictory initialization segment and fails the item immediately. The
/// base layer is ordinary `hvc1` HDR10, so replace only the stale file-type
/// brand with the ISO fragmented-MP4 brand already used by this muxer. A real
/// Dolby Vision sample entry keeps its brand untouched.
pub(crate) fn sanitize_stale_dolby_brand(init: &mut Init) -> bool {
    if init.video().is_none_or(|video| video.dolby_vision_config) {
        return false;
    }

    let bytes = &mut init.bytes;
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    let size = u32::from_be_bytes(bytes[0..4].try_into().expect("four-byte ftyp size")) as usize;
    if size < 16 || size > bytes.len() || !(size - 16).is_multiple_of(4) {
        return false;
    }

    let mut changed = false;
    for offset in std::iter::once(8).chain((16..size).step_by(4)) {
        if &bytes[offset..offset + 4] == b"dby1" {
            bytes[offset..offset + 4].copy_from_slice(b"iso6");
            changed = true;
        }
    }
    changed
}

#[cfg(any(test, feature = "live-hls-recovery"))]
async fn finish(
    segmenter: Option<Segmenter>,
    out: &mut SessionDir,
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
                    if let Err(e) = out.write_segment(&segment).await {
                        if session_directory_gone(&out.dir).await {
                            tracing::debug!(
                                session = %crate::transcode::session_log_id(session_id),
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
            Err(fmp4::Fmp4Error::Malformed(reason)) => {
                return Outcome::ReaderFailed {
                    reason: format!("merging the final segment: {reason}"),
                    counts: seg.counts(),
                };
            }
        }
        if let Err(e) = out.write_endlist().await {
            return if session_directory_gone(&out.dir).await {
                Outcome::Cancelled(seg.counts())
            } else {
                Outcome::ReaderFailed {
                    reason: format!("writing the playlist end: {e}"),
                    counts: seg.counts(),
                }
            };
        }
        if !out.started {
            return Outcome::ReaderFailed {
                reason: "the reader completed without publishing an ENDLIST".into(),
                counts: seg.counts(),
            };
        }
        let endlist = tokio::fs::read_to_string(out.dir.join("index.m3u8")).await;
        match endlist {
            Ok(text) if text.ends_with("#EXT-X-ENDLIST\n") => {}
            Ok(_) => {
                return Outcome::ReaderFailed {
                    reason: "the final playlist did not end with ENDLIST".into(),
                    counts: seg.counts(),
                };
            }
            Err(e) => {
                if session_directory_gone(&out.dir).await {
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

/// The end-of-session line `scripts/perf-report` greps.
///
/// One line, one session, every number the cut policy produced — including the
/// ones that are bad news. A ceiling cut still costs the leading picture, and
/// a residual nobody counts is a residual nobody fixes.
#[cfg(any(test, feature = "live-hls-recovery"))]
pub fn summary(counts: &SegmentCounts) -> String {
    format!(
        "copy segmenter: segments {} · clean cuts {} · ceiling cuts {} · \
         fragments {} · unparseable {} · repaired joins {}",
        counts.segments,
        counts.clean_cuts,
        counts.ceiling_cuts,
        counts.fragments,
        counts.unparseable,
        counts.tfdt_adjustments,
    )
}

/// Whether the segmenter can be attempted for a source at all.
///
/// The classifier reads HEVC and H.264 keyframes; anything else would be
/// `Unparseable` on every fragment, which works (nothing is ever cut in front
/// of an unparseable fragment) but would spend every session running the byte
/// ceiling. Better to not start.
pub fn supports(video_codec: Option<&str>) -> bool {
    matches!(video_codec, Some("hevc" | "h265" | "h264" | "avc"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::fmp4::{CutReason, Track, VideoCodec};
    use plurx_core::testfixtures::pipe;
    use std::path::Path;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::ReadBuf;

    struct BrokenPipe;

    impl AsyncRead for BrokenPipe {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(std::io::Error::other("fixture read failure")))
        }
    }

    struct TeardownPipe {
        dir: PathBuf,
    }

    impl AsyncRead for TeardownPipe {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let _ = std::fs::remove_dir_all(&self.dir);
            Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "fixture teardown",
            )))
        }
    }

    /// Long enough to reach a ceiling from a 12 s fixture. The floor is 3 s
    /// rather than the shipped 6 s for the same reason: the property under
    /// test is which keyframe a cut lands on, not what the constant is. The
    /// publish gate is 0 — playlist from the first segment, the pre-gate
    /// behaviour — because these tests observe cut placement, not
    /// publication; the gate has its own tests.
    fn brisk() -> Limits {
        Limits {
            floor_seconds: 3,
            first_floor_seconds: 1,
            max_bytes: 48_000_000,
            max_seconds: 15,
            publish_gate_secs: 0,
        }
    }

    async fn session(kind: &str, limits: Limits) -> (tempfile::TempDir, Outcome) {
        let dir = crate::test_tempdir().expect("tempdir");
        let feed = pipe(kind);
        let outcome = run(&feed[..], dir.path().to_path_buf(), "test", limits).await;
        (dir, outcome)
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

    fn playlist(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("index.m3u8")).unwrap_or_default()
    }

    fn segment_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("session dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn init_with_dolby_brand(dolby_vision_config: bool) -> Init {
        // The exact `ftyp` shape jellyfin-ffmpeg 7.1 writes after
        // dovi_rpu=strip=1: ordinary ISO brands plus the stale dby1 claim.
        let bytes = Vec::from(&b"\0\0\0\x20ftypiso5\0\0\x02\0iso5iso6dby1mp41"[..]);
        assert_eq!(bytes.len(), 32);
        Init {
            bytes,
            tracks: vec![Track {
                id: 1,
                kind: TrackKind::Video,
                timescale: 24_000,
                codec: Some(VideoCodec::Hevc),
                dolby_vision_config,
                nal_length_size: 4,
                default_sample_duration: 0,
                default_sample_size: 0,
                default_sample_flags: 0,
            }],
        }
    }

    #[test]
    fn stripped_dolby_vision_init_drops_the_stale_dby1_brand() {
        let mut init = init_with_dolby_brand(false);
        assert!(sanitize_stale_dolby_brand(&mut init));
        assert!(!init.bytes.windows(4).any(|fourcc| fourcc == b"dby1"));
        assert_eq!(&init.bytes[24..28], b"iso6");
    }

    #[test]
    fn preserved_dolby_vision_init_keeps_its_dby1_brand() {
        let mut init = init_with_dolby_brand(true);
        assert!(!sanitize_stale_dolby_brand(&mut init));
        assert!(init.bytes.windows(4).any(|fourcc| fourcc == b"dby1"));
    }

    #[tokio::test]
    async fn emitted_hvc1_refuses_an_incomplete_decoder_configuration_without_a_probe_hint() {
        let mut feed = pipe("closed-gop");
        // The fixture pipe has removed in-band parameter sets. Leave hvcC
        // structurally valid but turn its PPS into a duplicate SPS so no
        // hidden sample data can complete the configuration.
        replace_hvcc_array_type(&mut feed, 34, 33);
        let dir = crate::test_tempdir().expect("tempdir");
        let outcome = run(&feed[..], dir.path().to_path_buf(), "test", brisk()).await;
        let Outcome::InvalidHevcConfiguration(reason) = outcome else {
            panic!("an incomplete emitted hvcC was not terminal: {outcome:?}");
        };
        assert!(reason.contains("complete VPS/SPS/PPS"), "{reason}");
        assert!(!dir.path().join("init.mp4").exists());
        assert!(!dir.path().join("index.m3u8").exists());
    }

    /// The whole point, end to end: given a source that offers clean cut
    /// points, every boundary lands on one.
    #[tokio::test]
    async fn a_session_on_a_source_with_clean_points_publishes_only_clean_cuts() {
        let (dir, outcome) = session("clean-cra", brisk()).await;
        let Outcome::Completed(counts) = outcome else {
            panic!("the segmenter did not run: {outcome:?}");
        };
        assert!(counts.segments >= 2, "{counts:?}");
        assert_eq!(
            counts.ceiling_cuts, 0,
            "a ceiling cut with clean points available"
        );
        assert_eq!(counts.unparseable, 0);
        // Every cut but the last (which ended because the film did) is clean.
        assert_eq!(counts.clean_cuts, counts.segments - 1);

        let text = playlist(dir.path());
        assert!(text.ends_with("#EXT-X-ENDLIST\n"), "{text}");
        assert_eq!(
            text.matches(".m4s\n").count() as u64,
            counts.segments,
            "the playlist and the counts disagree"
        );
        for i in 0..counts.segments {
            assert!(dir.path().join(format!("seg{i:05}.m4s")).exists());
        }
        assert!(dir.path().join("init.mp4").exists());
    }

    /// And the honest half: a source with no clean point still makes progress,
    /// and the counts say what it cost.
    #[tokio::test]
    async fn a_source_with_no_clean_points_cuts_at_the_ceiling_and_says_so() {
        let limits = Limits {
            floor_seconds: 3,
            first_floor_seconds: 1,
            max_bytes: 300_000,
            max_seconds: 15,
            publish_gate_secs: 0,
        };
        let (dir, outcome) = session("open-gop", limits).await;
        let Outcome::Completed(counts) = outcome else {
            panic!("the segmenter did not run: {outcome:?}");
        };
        assert!(counts.ceiling_cuts >= 1, "{counts:?}");
        assert_eq!(counts.clean_cuts, 0, "there was no clean point to find");
        let line = summary(&counts);
        assert!(
            line.contains(&format!("ceiling cuts {}", counts.ceiling_cuts)),
            "the log line hides the cuts that still cost a frame: {line}"
        );
        assert!(playlist(dir.path()).ends_with("#EXT-X-ENDLIST\n"));
    }

    /// The playlist is the interface to everything downstream — the session's
    /// segment index, the ahead-window suspend, the GC, perf-report. Feed it
    /// to the parser the producer uses on real cached parts.
    #[tokio::test]
    async fn the_playlist_matches_the_template_and_produce_can_parse_it() {
        let (dir, outcome) = session("clean-cra", brisk()).await;
        let Outcome::Completed(counts) = outcome else {
            panic!("{outcome:?}");
        };
        let text = playlist(dir.path());

        for tag in [
            "#EXTM3U\n",
            "#EXT-X-VERSION:7\n",
            "#EXT-X-MEDIA-SEQUENCE:0\n",
            "#EXT-X-PLAYLIST-TYPE:EVENT\n",
            "#EXT-X-MAP:URI=\"init.mp4\"\n",
        ] {
            assert!(text.contains(tag), "playlist is missing {tag}: {text}");
        }
        assert!(
            !text.contains("INDEPENDENT-SEGMENTS"),
            "a claim a ceiling cut makes false"
        );

        // TARGETDURATION is the client's playlist-reload interval on a live
        // EVENT playlist (RFC 8216 §6.3.4), so it has to be the real ceiling
        // of what was published — not a constant far above it, which is how a
        // player ends up waiting fifteen seconds to learn a second segment
        // exists and stalls at the end of the first.
        let longest = text
            .lines()
            .filter_map(|l| l.strip_prefix("#EXTINF:"))
            .filter_map(|l| l.split(',').next()?.parse::<f64>().ok())
            .fold(0.0f64, f64::max);
        let declared: u32 = text
            .lines()
            .find_map(|l| l.strip_prefix("#EXT-X-TARGETDURATION:"))
            .and_then(|v| v.trim().parse().ok())
            .expect("a target duration");
        assert_eq!(
            declared,
            longest.ceil().max(1.0) as u32,
            "TARGETDURATION {declared} against a longest segment of {longest:.3}s"
        );

        let part = crate::produce::Part::from_playlist(&text);
        assert_eq!(part.segments.len() as u64, counts.segments);
        assert_eq!(part.durations_ms.len() as u64, counts.segments);
        let total: i64 = part.durations_ms.iter().sum();
        assert!(
            (11_900..=12_100).contains(&total),
            "produce read {total}ms out of a 12 s fixture"
        );
    }

    /// The startup stall, as a test. A player's runway when it loads a live
    /// playlist is the first segment and nothing else; its next chance to see
    /// a second one is one TARGETDURATION away. So the first segment must be
    /// short and the tag must be honest, or the picture stops the moment
    /// segment zero runs out — which is exactly what Safari did, 5631 ms at
    /// 9.2 s, on a server 55 seconds ahead.
    #[tokio::test]
    async fn the_first_segment_is_short_and_the_playlist_says_so() {
        let (dir, outcome) = session("clean-cra", Limits::default()).await;
        let Outcome::Completed(counts) = outcome else {
            panic!("{outcome:?}");
        };
        assert!(counts.segments >= 2, "{counts:?}");
        let text = playlist(dir.path());
        let extinf: Vec<f64> = text
            .lines()
            .filter_map(|l| l.strip_prefix("#EXTINF:"))
            .filter_map(|l| l.split(',').next()?.parse::<f64>().ok())
            .collect();
        let declared: u32 = text
            .lines()
            .find_map(|l| l.strip_prefix("#EXT-X-TARGETDURATION:"))
            .and_then(|v| v.trim().parse().ok())
            .expect("a target duration");

        // Short enough that the first reload arrives before it runs out.
        assert!(
            extinf[0] < declared as f64,
            "the first segment is {}s against a {declared}s reload interval — \
             the player runs dry before it can learn segment 1 exists",
            extinf[0]
        );
        // And short in absolute terms: this is the whole startup buffer.
        assert!(extinf[0] <= 4.0, "first segment {}s", extinf[0]);
        // The shipped floor still governs everything after it.
        for (i, d) in extinf.iter().enumerate().skip(1).take(extinf.len() - 2) {
            assert!(
                *d >= COPY_SEGMENT_SECONDS as f64,
                "segment {i} is {d}s, under the {COPY_SEGMENT_SECONDS}s floor"
            );
        }
    }

    /// A killed session — the pipe stops mid-fragment. What is on disk has to
    /// be exactly what was complete: no `.tmp` residue for the serving layer
    /// to trip over, and no ENDLIST claiming a film that ends early.
    #[tokio::test]
    async fn a_killed_session_leaves_no_tmp_files_and_no_endlist() {
        let dir = crate::test_tempdir().expect("tempdir");
        let feed = pipe("clean-cra");
        // Two thirds of the stream, which lands inside a fragment.
        let cut = feed.len() * 2 / 3;
        let outcome = run(&feed[..cut], dir.path().to_path_buf(), "test", brisk()).await;
        let Outcome::ReaderFailed { counts, .. } = outcome else {
            panic!("{outcome:?}");
        };
        assert!(counts.segments >= 1);

        let names = segment_files(dir.path());
        assert!(
            !names.iter().any(|n| n.ends_with(".tmp")),
            "left a partial file behind: {names:?}"
        );
        let text = playlist(dir.path());
        assert!(!text.contains("ENDLIST"), "claimed the film ended: {text}");
        assert_eq!(text.matches(".m4s\n").count() as u64, counts.segments);
    }

    /// The publish gate: segments land on disk from the start, but the
    /// playlist does not exist until the cushion does. A session killed
    /// before the gate fills leaves segment files and NO playlist — the
    /// player was never told anything, which is the property the whole gate
    /// stands on (a client that can't see the live edge can't start at it).
    #[tokio::test]
    async fn the_publish_gate_holds_the_playlist_while_segments_land() {
        let dir = crate::test_tempdir().expect("tempdir");
        let feed = pipe("clean-cra");
        // Mid-fragment, so nothing mistakes this for end of stream — EOF is
        // the one thing allowed to open the gate early.
        let cut = feed.len() * 2 / 3;
        let mut limits = brisk();
        limits.publish_gate_secs = 999;
        let outcome = run(&feed[..cut], dir.path().to_path_buf(), "test", limits).await;
        let Outcome::ReaderFailed { counts, .. } = outcome else {
            panic!("{outcome:?}");
        };
        assert!(counts.segments >= 1, "{counts:?}");
        assert!(
            !dir.path().join("index.m3u8").exists(),
            "the gate published a playlist {} segments into a 999 s hold",
            counts.segments
        );
        // The work itself was not held back — only the announcement.
        assert!(dir.path().join("init.mp4").exists());
        for i in 0..counts.segments {
            assert!(dir.path().join(format!("seg{i:05}.m4s")).exists());
        }
    }

    /// And the gate opening mid-stream: once the cushion of media exists, the
    /// first playlist published lists all of it, and every later segment
    /// republishes as before. Same truncated feed as above, a gate the
    /// fixture can actually fill.
    #[tokio::test]
    async fn the_gate_opens_mid_stream_and_the_first_playlist_lists_the_cushion() {
        let dir = crate::test_tempdir().expect("tempdir");
        let feed = pipe("clean-cra");
        let cut = feed.len() * 2 / 3;
        let mut limits = brisk();
        limits.publish_gate_secs = 4;
        let outcome = run(&feed[..cut], dir.path().to_path_buf(), "test", limits).await;
        let Outcome::ReaderFailed { counts, .. } = outcome else {
            panic!("{outcome:?}");
        };
        assert!(
            counts.segments >= 2,
            "the fixture no longer cuts two segments by two thirds in — \
             re-derive this test's cut point: {counts:?}"
        );
        let text = playlist(dir.path());
        assert_eq!(
            text.matches(".m4s\n").count() as u64,
            counts.segments,
            "once open, the playlist must keep step with every segment: {text}"
        );
        let listed: f64 = text
            .lines()
            .filter_map(|l| l.strip_prefix("#EXTINF:"))
            .filter_map(|l| l.split(',').next()?.parse::<f64>().ok())
            .sum();
        assert!(
            listed >= 4.0,
            "the gate opened at {listed:.3}s of media against a 4 s line"
        );
        assert!(!text.contains("ENDLIST"), "{text}");
    }

    /// End of stream overrides the gate: a film shorter than the cushion
    /// publishes whole — ENDLIST and all — the moment it finishes. The first
    /// playlist a player loads is simply VOD.
    #[tokio::test]
    async fn a_film_shorter_than_the_gate_still_publishes_whole_at_eof() {
        let mut limits = brisk();
        limits.publish_gate_secs = 999;
        let (dir, outcome) = session("clean-cra", limits).await;
        let Outcome::Completed(counts) = outcome else {
            panic!("{outcome:?}");
        };
        assert!(counts.segments >= 2, "{counts:?}");
        let text = playlist(dir.path());
        assert!(
            text.ends_with("#EXT-X-ENDLIST\n"),
            "a finished film must say so however few segments it has: {text}"
        );
        assert_eq!(text.matches(".m4s\n").count() as u64, counts.segments);
    }

    /// EOF may be where the reader first proves a structural incompatibility:
    /// final tail splitting rejects a sample longer than the HLS ceiling. A
    /// local playlist can already exist by then, but only the actor knows
    /// whether any response was admitted, so the reader must preserve the
    /// typed Unsupported fact rather than inventing publication policy.
    #[tokio::test]
    async fn an_unsupported_final_tail_stays_typed_after_local_playlist_creation() {
        let mut limits = brisk();
        limits.max_seconds = 0;
        let (dir, outcome) = session("clean-cra", limits).await;
        let Outcome::Unsupported(reason) = outcome else {
            panic!("the EOF structural failure lost its typed outcome: {outcome:?}");
        };
        assert!(reason.contains("sample longer than"), "{reason}");
        assert!(
            dir.path().join("index.m3u8").exists(),
            "the fixture must prove local playlist creation cannot decide fallback"
        );
    }

    /// Malformed bytes are a failed reader, not evidence that the legal stream
    /// shape is unsupported and eligible for the one legacy retry.
    #[tokio::test]
    async fn an_unparseable_moov_is_reader_failure_not_fallback() {
        let dir = crate::test_tempdir().expect("tempdir");
        let mut feed = pipe("open-gop");
        // Corrupt the moov's first child box size. The box walk then runs past
        // its parent, which is the malformed case, not the truncated one.
        let head = 28 + 8; // past ftyp and the moov header
        feed[head..head + 4].copy_from_slice(&0xffff_ffffu32.to_be_bytes());
        let outcome = run(&feed[..], dir.path().to_path_buf(), "test", brisk()).await;
        assert!(
            matches!(outcome, Outcome::ReaderFailed { .. }),
            "a broken moov minted a fallback request: {outcome:?}"
        );
        assert!(!dir.path().join("index.m3u8").exists());
    }

    /// A chapter track in the init is the shape that broke Safari. Even with
    /// `-map_chapters -1` shutting it off at the source, a stream that somehow
    /// arrives carrying one must go to the legacy muxer rather than to a
    /// player: that output is known to be playable, and this one is known not
    /// to be.
    #[tokio::test]
    async fn a_track_this_path_never_asked_for_takes_the_fallback() {
        let src = plurx_core::testfixtures::source_with_chapters();
        // The pipe command WITHOUT the chapter strip — what shipped, and what
        // Safari refused.
        let out = std::process::Command::new(plurx_core::testfixtures::ffmpeg())
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&src)
            .args(["-map", "0:v:0", "-map", "0:a:0?", "-sn", "-c:v", "copy"])
            .args([
                "-tag:v",
                "hvc1",
                "-bsf:v",
                "filter_units=remove_types=32-34",
            ])
            .args([
                "-c:a",
                "aac",
                "-b:a",
                "256k",
                "-avoid_negative_ts",
                "make_zero",
            ])
            .args([
                "-movflags",
                "frag_keyframe+empty_moov+default_base_moof+delay_moov",
            ])
            .args(["-f", "mp4", "pipe:1"])
            .output()
            .expect("ffmpeg");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let dir = crate::test_tempdir().expect("tempdir");
        let outcome = run(&out.stdout[..], dir.path().to_path_buf(), "test", brisk()).await;
        match outcome {
            Outcome::Unsupported(reason) => {
                assert!(reason.contains("never asked for"), "{reason}");
            }
            other => panic!("a chapter track was served to a player: {other:?}"),
        }
        assert!(!dir.path().join("index.m3u8").exists());
    }

    /// Nothing at all down the pipe is producer/read failure, not a structural
    /// capability decision from parsed media.
    #[tokio::test]
    async fn an_empty_pipe_is_reader_failure_not_fallback() {
        let dir = crate::test_tempdir().expect("tempdir");
        let outcome = run(&[][..], dir.path().to_path_buf(), "test", brisk()).await;
        assert!(
            matches!(outcome, Outcome::ReaderFailed { .. }),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_supported_reader_failure_is_not_a_fallback_request() {
        let dir = crate::test_tempdir().expect("tempdir");
        let outcome = run(BrokenPipe, dir.path().to_path_buf(), "test", brisk()).await;
        let Outcome::ReaderFailed { reason, counts } = outcome else {
            panic!("a pipe read error was not classified as reader failure: {outcome:?}");
        };
        assert!(reason.contains("fixture read failure"), "{reason}");
        assert_eq!(counts, SegmentCounts::default());
    }

    #[tokio::test]
    async fn missing_scratch_at_start_is_reader_failure_not_cancellation() {
        let feed = pipe("clean-cra");
        let dir = crate::test_tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        std::fs::remove_dir_all(&path).expect("remove fixture session directory");
        let outcome = run(&feed[..], path, "test", brisk()).await;
        assert!(
            matches!(outcome, Outcome::ReaderFailed { .. }),
            "a directory that never existed masqueraded as teardown: {outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_file_at_the_scratch_path_is_reader_failure_not_cancellation() {
        let feed = pipe("clean-cra");
        let parent = crate::test_tempdir().expect("tempdir");
        let path = parent.path().join("not-a-directory");
        std::fs::write(&path, b"fixture").expect("write fixture file");
        let outcome = run(&feed[..], path, "test", brisk()).await;
        let Outcome::ReaderFailed { reason, .. } = outcome else {
            panic!("a non-directory scratch path became cancellation: {outcome:?}");
        };
        assert!(reason.contains("not a directory"), "{reason}");
    }

    #[tokio::test]
    async fn scratch_removed_after_reader_start_is_cancellation() {
        let dir = crate::test_tempdir().expect("tempdir");
        let outcome = run(
            TeardownPipe {
                dir: dir.path().to_path_buf(),
            },
            dir.path().to_path_buf(),
            "test",
            brisk(),
        )
        .await;
        assert!(
            matches!(outcome, Outcome::Cancelled(_)),
            "session teardown became producer failure: {outcome:?}"
        );
    }

    /// The pipe arrives in whatever pieces the kernel felt like, and a
    /// SIGSTOPped ffmpeg can leave minutes between them. Feeding the same
    /// bytes through a source that hands over a trickle at a time has to
    /// produce the identical session.
    #[tokio::test]
    async fn a_trickling_pipe_produces_the_same_session() {
        let (whole, whole_outcome) = session("clean-cra", brisk()).await;
        let dir = crate::test_tempdir().expect("tempdir");
        let feed = pipe("clean-cra");
        let trickle = tokio::io::BufReader::with_capacity(1, &feed[..]);
        let outcome = run(trickle, dir.path().to_path_buf(), "test", brisk()).await;
        assert_eq!(outcome, whole_outcome);
        assert_eq!(playlist(dir.path()), playlist(whole.path()));
        assert_eq!(segment_files(dir.path()), segment_files(whole.path()));
    }

    /// The whole path, once, against a live ffmpeg: the real
    /// `copy_pipe_args`, a real child process, its real stdout, this reader,
    /// and real files on disk — and what comes out decodes frame for frame
    /// like the source.
    ///
    /// Every other test here feeds `run()` a byte slice, which is right for
    /// the disk semantics but proves nothing about the arguments or about
    /// reading from a pipe that arrives at ffmpeg's pace. This is the one that
    /// would notice if `copy_pipe_args` stopped producing a stream this reader
    /// can follow — the exact failure that would silently put every copy
    /// session on the legacy fallback with only a log line to say so.
    #[tokio::test]
    async fn a_live_ffmpeg_pipe_produces_a_session_that_decodes_like_the_source() {
        use plurx_core::domain::MediaFile;
        use plurx_core::transcode::{copy_pipe_args, Pacing};

        let src = plurx_core::testfixtures::source("clean-cra");
        let file = MediaFile {
            id: 1,
            item_id: 1,
            path: src.clone(),
            size: 1,
            mtime: 1,
            duration_ms: Some(12_000),
            container: Some("mkv".into()),
            video_codec: Some("hevc".into()),
            video_profile: Some("Main".into()),
            width: Some(640),
            height: Some(360),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            bitrate: Some(1_000_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
        };
        // Unpaced: the pacing flags are the daemon's business and a 12 s
        // fixture read at 2× would just make the test slow.
        let args = copy_pipe_args(&file, 0.0, None, true, Pacing::unpaced(), false);
        let mut child = tokio::process::Command::new(plurx_core::testfixtures::ffmpeg())
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawning ffmpeg");
        let stdout = child.stdout.take().expect("stdout pipe");

        let dir = crate::test_tempdir().expect("tempdir");
        let limits = Limits {
            floor_seconds: 3,
            first_floor_seconds: 1,
            max_bytes: 48_000_000,
            max_seconds: 15,
            publish_gate_secs: 0,
        };
        let outcome = run(stdout, dir.path().to_path_buf(), "live", limits).await;
        let _ = child.wait().await;

        let Outcome::Completed(counts) = outcome else {
            panic!("a live ffmpeg pipe was not readable: {outcome:?}");
        };
        assert!(counts.segments >= 2, "{counts:?}");
        assert_eq!(counts.unparseable, 0, "{counts:?}");
        assert_eq!(
            counts.ceiling_cuts, 0,
            "a clean source cut dirty: {counts:?}"
        );
        let text = playlist(dir.path());
        assert!(text.ends_with("#EXT-X-ENDLIST\n"), "{text}");

        // init + every segment, concatenated, against the source. framemd5
        // hashes each frame WITH its pts and duration, so a reordered,
        // retimed or dropped frame all fail here.
        let mut cat = std::fs::read(dir.path().join("init.mp4")).expect("init.mp4");
        for i in 0..counts.segments {
            let seg = dir.path().join(format!("seg{i:05}.m4s"));
            cat.extend_from_slice(&std::fs::read(&seg).expect("segment"));
        }
        let candidate = dir.path().join("candidate.mp4");
        std::fs::write(&candidate, &cat).expect("candidate");

        let framemd5 = |path: &Path, stream: &str| {
            let out = plurx_core::testfixtures::run(
                std::process::Command::new(plurx_core::testfixtures::ffmpeg())
                    .args(["-v", "error", "-i"])
                    .arg(path)
                    .args(["-map", stream, "-f", "framemd5", "-"]),
            );
            String::from_utf8_lossy(&out)
                .lines()
                .filter(|l| !l.starts_with('#'))
                .collect::<Vec<_>>()
                .join("\n")
        };
        // Materialize the reference through the cached path, then compare.
        let _ = plurx_core::testfixtures::pipe("clean-cra");
        let reference = plurx_core::testfixtures::pipe_path("clean-cra");
        for stream in ["0:v:0", "0:a:0"] {
            assert_eq!(
                framemd5(&candidate, stream),
                framemd5(&reference, stream),
                "{stream} decodes differently after a live session"
            );
        }
    }

    #[test]
    fn only_codecs_whose_keyframes_can_be_read_are_attempted() {
        assert!(supports(Some("hevc")));
        assert!(supports(Some("h265")));
        assert!(supports(Some("h264")));
        assert!(!supports(Some("vp9")));
        assert!(!supports(Some("av1")));
        assert!(!supports(None));
    }

    /// The reason string reaches the log, so it has to say something.
    #[test]
    fn the_summary_names_every_count() {
        let counts = SegmentCounts {
            fragments: 40,
            segments: 7,
            clean_cuts: 5,
            ceiling_cuts: 1,
            unparseable: 0,
            tfdt_adjustments: 0,
        };
        let s = summary(&counts);
        for want in [
            "segments 7",
            "clean cuts 5",
            "ceiling cuts 1",
            "fragments 40",
            "unparseable 0",
        ] {
            assert!(s.contains(want), "{s}");
        }
    }

    /// `CutReason` is what the counts are made of; a rename that silently
    /// changed a label would change what perf-report reads.
    #[test]
    fn cut_reasons_keep_their_labels() {
        assert_eq!(CutReason::Clean.label(), "clean");
        assert_eq!(CutReason::ByteCeiling.label(), "byte-ceiling");
        assert_eq!(CutReason::TimeCeiling.label(), "time-ceiling");
        assert_eq!(CutReason::EndOfStream.label(), "eof");
    }
}
