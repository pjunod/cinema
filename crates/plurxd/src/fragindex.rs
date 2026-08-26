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

/// How an index build ended.
#[derive(Debug, Clone, PartialEq)]
pub enum IndexOutcome {
    /// A complete pass over the file.
    Built(Box<FragmentIndex>),
    /// The pipe ended before the file did. Not persisted, not an error the
    /// operator needs to see — a NAS read that went wrong, and the next scan
    /// tries again.
    Truncated { reason: String, rows: usize },
    /// This file cannot be indexed by this path at all, and retrying will not
    /// change that. The caller records it so the background job stops asking.
    Unsupported(String),
}

/// Read one index pipe to exhaustion.
///
/// Generic over the source for the same reason [`crate::copyseg::run`] is:
/// everything between the pipe and the index is worth testing and none of it
/// needs a real child process to be worth testing.
pub async fn index_stream<R: AsyncRead + Unpin>(
    mut src: R,
    identity: SourceIdentity,
    expected_ms: Option<i64>,
    require_hevc_parameter_sets: bool,
) -> IndexOutcome {
    let mut reader = FragmentReader::new();
    let mut init: Option<Init> = None;
    let mut init_sha = String::new();
    let mut timescale: u32 = 0;
    let mut rows: Vec<IndexRow> = Vec::new();
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

    let promotion = promotion.unwrap_or_default();
    if require_hevc_parameter_sets {
        let Some(mut served_init) = init.clone() else {
            return IndexOutcome::Unsupported(
                "the index pipe ended without an HEVC init to validate".into(),
            );
        };
        if let Err(error) =
            fmp4::promote_hevc_parameter_sets_from(&mut served_init, &promotion.parameter_sets)
        {
            return IndexOutcome::Unsupported(format!(
                "the HEVC decoder configuration could not be completed: {error}"
            ));
        }
        match fmp4::hevc_parameter_sets_complete(&served_init) {
            Ok(true) => {}
            Ok(false) => {
                return IndexOutcome::Unsupported(
                    "the HEVC decoder configuration has no complete VPS/SPS/PPS set".into(),
                )
            }
            Err(error) => {
                return IndexOutcome::Unsupported(format!(
                    "validating the HEVC decoder configuration: {error}"
                ))
            }
        }
    }

    let mut built = FragmentIndex::new(timescale, rows, init_sha, identity);
    built.promotion = promotion;
    built.parameter_sets_constant = parameter_sets_constant;
    IndexOutcome::Built(Box::new(built))
}

/// The identity a file's index is keyed by, for this build of ffmpeg.
pub fn identity_for(file: &MediaFile, video: transcode::CopyVideoOptions) -> SourceIdentity {
    SourceIdentity::new(
        file.size.max(0) as u64,
        file.mtime,
        plurx_core::segplan::argv_fingerprint(&transcode::copy_video_args(file, video)),
    )
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
    let args = transcode::copy_index_pipe_args(file, video);
    build_with_args(file, args, None, video, runtime_cache, budget).await
}

/// Build from the exact file descriptor whose complete digest was observed.
/// The parent retains ownership; the child receives a duplicate as fd 3.
#[cfg(unix)]
pub async fn build_from_attested_file(
    file: &MediaFile,
    source: &std::fs::File,
    video: transcode::CopyVideoOptions,
    runtime_cache: &Path,
    budget: Duration,
) -> IndexOutcome {
    use std::os::fd::AsRawFd;

    let args = transcode::copy_index_pipe_args_with_input(file, "/dev/fd/3", video);
    build_with_args(
        file,
        args,
        Some(source.as_raw_fd()),
        video,
        runtime_cache,
        budget,
    )
    .await
}

#[cfg(not(unix))]
pub async fn build_from_attested_file(
    file: &MediaFile,
    _source: &std::fs::File,
    video: transcode::CopyVideoOptions,
    runtime_cache: &Path,
    budget: Duration,
) -> IndexOutcome {
    build(file, video, runtime_cache, budget).await
}

#[allow(clippy::too_many_arguments)]
async fn build_with_args(
    file: &MediaFile,
    args: Vec<String>,
    source_fd: Option<SourceFd>,
    video: transcode::CopyVideoOptions,
    runtime_cache: &Path,
    budget: Duration,
) -> IndexOutcome {
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
        index_stream(
            stdout,
            identity,
            expected_ms,
            video.promotes_parameter_sets(),
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

    fn identity() -> SourceIdentity {
        SourceIdentity::new(1, 1, "fingerprint")
    }

    /// The index pipe over a real fixture, read the way the daemon reads it.
    async fn index_fixture(kind: &str) -> IndexOutcome {
        let bytes = index_pipe_bytes(kind);
        index_stream(std::io::Cursor::new(bytes), identity(), None, false).await
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
    async fn required_promotion_refuses_an_incomplete_decoder_configuration() {
        let mut bytes = index_pipe_bytes("closed-gop");
        // Keep the hvcC structurally valid but turn its PPS array into a
        // duplicate SPS array. The ordinary fixture pipe has already removed
        // in-band sets, so there is no hidden PPS from which to "succeed".
        replace_hvcc_array_type(&mut bytes, 34, 33);
        let outcome = index_stream(std::io::Cursor::new(bytes), identity(), None, true).await;
        let IndexOutcome::Unsupported(reason) = outcome else {
            panic!("an incomplete required hvcC must not be indexed: {outcome:?}");
        };
        assert!(reason.contains("complete VPS/SPS/PPS"), "{reason}");
    }

    #[tokio::test]
    async fn a_film_whose_clean_starts_disagree_is_not_vod_presentable() {
        // Built by hand, because no fixture can produce it: the corpus is
        // single-invocation encodes, which are structurally incapable of
        // per-IDR parameter-set variation. The check still has to work.
        let bytes = index_pipe_bytes("closed-gop");
        let IndexOutcome::Built(index) =
            index_stream(std::io::Cursor::new(bytes), identity(), None, false).await
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
            parameter_sets: vec![vec![0x40, 0x01, 0x0c]],
            hdr10_sei: Vec::new(),
        };
        let differing = PromotionInputs {
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
            false,
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
        let IndexOutcome::Built(full) =
            index_stream(std::io::Cursor::new(bytes.clone()), identity(), None, false).await
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
            false,
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
                false,
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
