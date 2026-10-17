//! Held-source, cancellable CPU execution for the reviewed translation profile.
//!
//! This module does not claim jobs, schedule demands, publish coverage, or make
//! a model ready. The manager owns those transactions and passes one admitted,
//! fenced operation here. Every exit awaits its hash/child before scratch and
//! the caller's admission can retire.

use crate::{
    background_jobs::JobFence,
    fragment_index_cluster::{open_source_playback_fence, SourceFence},
    state::AppState,
    subtitle_models::artifacts::{Engine, ManagedModel},
    transcode::FragmentAdmission,
};
#[cfg(unix)]
use plurx_core::store::subtitle_translation::{
    cue_id, ExecutionProvenance, TranslationCue, MAX_TRANSLATION_CUES,
};
use plurx_core::{
    domain::MediaFile,
    error::StoreError,
    fs_secure::{std_file_identity, FileIdentity},
    store::subtitle_translation::{RangeRecipe, SegmentDraft, SourceRevision},
};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::path::Path;
use std::{
    io::{Read, Seek, SeekFrom},
    time::Duration,
};
#[cfg(unix)]
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
const RATE: i64 = 16_000;
#[cfg(unix)]
const MAX_PCM_BYTES: u64 = 4 * 1024 * 1024;
#[cfg(unix)]
const MAX_CHILD_LOG: usize = 1024 * 1024;

#[cfg(unix)]
#[derive(Debug)]
struct ClockObservation {
    raw_first_sample: i64,
    anchor_sample: i64,
    samples: i64,
}

#[cfg(unix)]
fn log_integer(line: &str, key: &str) -> Option<i64> {
    let (_, tail) = line.split_once(key)?;
    tail.split_whitespace().next()?.parse().ok()
}

#[cfg(unix)]
fn observe_clock(log: &[u8], begin: i64, end: i64) -> Result<ClockObservation, StoreError> {
    let log = std::str::from_utf8(log).map_err(|_| failure("decode_failed"))?;
    let mut raw_first = None;
    let mut raw_previous: Option<(i64, i64, i64)> = None;
    let mut anchor = None;
    let mut next = None;
    let mut samples = 0_i64;
    for line in log.lines() {
        // asettb is explicitly1/16000 before either observed filter. Raw
        // source rate may differ; its pts already uses this declared timebase.
        if line.contains("Parsed_ashowinfo_1 ") {
            let pts =
                log_integer(line, " pts:").ok_or_else(|| failure("source_clock_unsupported"))?;
            let count = log_integer(line, " nb_samples:")
                .filter(|n| (1..=1_000_000).contains(n))
                .ok_or_else(|| failure("source_clock_unsupported"))?;
            let rate = log_integer(line, " rate:")
                .filter(|n| (8_000..=384_000).contains(n))
                .ok_or_else(|| failure("source_clock_unsupported"))?;
            if !(0..=14_400_000 * 16).contains(&pts) {
                return Err(failure("source_clock_unsupported"));
            }
            if let Some((previous, previous_count, previous_rate)) = raw_previous {
                let actual = i128::from(pts - previous) * i128::from(previous_rate);
                let expected = i128::from(previous_count) * i128::from(RATE);
                // asettb may round a non16k input by one output sample. Larger
                // gaps/overlaps are unavailable in the initial clock profile;
                // do not hide them with the downstream padding filter.
                if rate != previous_rate || (actual - expected).abs() > i128::from(previous_rate) {
                    return Err(failure("source_clock_unsupported"));
                }
            }
            raw_first.get_or_insert(pts);
            raw_previous = Some((pts, count, rate));
        }
        if !line.contains("Parsed_ashowinfo_4 ") {
            continue;
        }
        let pts = log_integer(line, " pts:").ok_or_else(|| failure("source_clock_unsupported"))?;
        let count = log_integer(line, " nb_samples:")
            .filter(|n| *n > 0)
            .ok_or_else(|| failure("source_clock_unsupported"))?;
        if log_integer(line, " rate:") != Some(RATE)
            || pts < begin
            || pts >= end
            || next.is_some_and(|expected| pts != expected)
        {
            return Err(failure("source_clock_unsupported"));
        }
        anchor.get_or_insert(pts);
        samples = samples
            .checked_add(count)
            .ok_or_else(|| failure("decode_failed"))?;
        next = Some(
            pts.checked_add(count)
                .ok_or_else(|| failure("decode_failed"))?,
        );
    }
    let raw_first = raw_first.ok_or_else(|| failure("source_clock_unsupported"))?;
    let anchor = anchor.ok_or_else(|| failure("decode_failed"))?;
    // Never manufacture missing speech by padding an input seek that started
    // after the requested window. Leading audio delay is unavailable in this
    // initial aligned-clock profile; the separate marker proof is not a blanket
    // permission to guess the origin of an arbitrary source.
    if raw_first > begin || anchor != begin || next.is_none_or(|n| n > end) {
        return Err(failure("source_clock_unsupported"));
    }
    Ok(ClockObservation {
        raw_first_sample: raw_first,
        anchor_sample: anchor,
        samples,
    })
}

#[cfg(unix)]
fn inspect_pcm(
    held: &mut std::fs::File,
    clock: &ClockObservation,
    core_begin_sample: i64,
    core_end_sample: i64,
    cancel: &CancellationToken,
) -> Result<bool, StoreError> {
    interrupted(cancel)?;
    let size = held.metadata().map_err(|_| failure("decode_failed"))?.len();
    if !(46..=MAX_PCM_BYTES).contains(&size) {
        return Err(failure("decode_failed"));
    }
    held.seek(SeekFrom::Start(0))
        .map_err(|_| failure("decode_failed"))?;
    let mut header = [0_u8; 12];
    held.read_exact(&mut header)
        .map_err(|_| failure("decode_failed"))?;
    if &header[..4] != b"RIFF"
        || &header[8..] != b"WAVE"
        || u64::from(u32::from_le_bytes(header[4..8].try_into().expect("header"))) + 8 != size
    {
        return Err(failure("decode_failed"));
    }
    let mut position = 12_u64;
    let mut format = false;
    let mut data = false;
    let mut signal = false;
    for _ in 0..64 {
        interrupted(cancel)?;
        if position == size {
            return if format && data {
                Ok(signal)
            } else {
                Err(failure("decode_failed"))
            };
        }
        if size - position < 8 {
            return Err(failure("decode_failed"));
        }
        let mut chunk = [0_u8; 8];
        held.read_exact(&mut chunk)
            .map_err(|_| failure("decode_failed"))?;
        let length = u64::from(u32::from_le_bytes(chunk[4..8].try_into().expect("chunk")));
        let stop = position
            .checked_add(8)
            .and_then(|n| n.checked_add(length))
            .and_then(|n| n.checked_add(length % 2))
            .filter(|n| *n <= size)
            .ok_or_else(|| failure("decode_failed"))?;
        match &chunk[..4] {
            b"fmt " => {
                if format || data || length != 16 {
                    return Err(failure("decode_failed"));
                }
                let mut fmt = [0_u8; 16];
                held.read_exact(&mut fmt)
                    .map_err(|_| failure("decode_failed"))?;
                if fmt != [1, 0, 1, 0, 128, 62, 0, 0, 0, 125, 0, 0, 2, 0, 16, 0] {
                    return Err(failure("decode_failed"));
                }
                format = true;
            }
            b"data" => {
                if !format
                    || data
                    || length == 0
                    || length % 2 != 0
                    || i64::try_from(length / 2).ok() != Some(clock.samples)
                    || core_begin_sample < clock.anchor_sample
                    || core_end_sample > clock.anchor_sample + clock.samples
                {
                    return Err(failure("decode_failed"));
                }
                data = true;
                let mut at = 0_u64;
                let mut buffer = [0_u8; 128 * 1024];
                while at < length {
                    interrupted(cancel)?;
                    let count = usize::try_from((length - at).min(buffer.len() as u64))
                        .map_err(|_| failure("decode_failed"))?;
                    held.read_exact(&mut buffer[..count])
                        .map_err(|_| failure("decode_failed"))?;
                    for (ordinal, pair) in buffer[..count].chunks_exact(2).enumerate() {
                        let sample = clock.anchor_sample + (at / 2) as i64 + ordinal as i64;
                        if sample >= core_begin_sample && sample < core_end_sample && pair != [0, 0]
                        {
                            signal = true;
                        }
                    }
                    at += count as u64;
                }
            }
            _ => {}
        }
        held.seek(SeekFrom::Start(stop))
            .map_err(|_| failure("decode_failed"))?;
        position = stop;
    }
    Err(failure("decode_failed"))
}

async fn phase(
    fence: &JobFence,
    phase: plurx_core::store::background_jobs_transcription::SubtitlePhase,
    cancel: &CancellationToken,
) -> Result<(), StoreError> {
    let valid = tokio::select! {
        () = cancel.cancelled() => return Err(failure("worker_interrupted")),
        result = tokio::time::timeout(Duration::from_secs(2), fence.observe_subtitle(phase)) => {
            result.map_err(|_| failure("worker_interrupted"))??
        }
    };
    if valid {
        Ok(())
    } else {
        Err(failure("worker_interrupted"))
    }
}

#[cfg(unix)]
async fn child_output(
    command: &mut tokio::process::Command,
    wall: Duration,
    limit: usize,
    inference: bool,
) -> Result<plurx_core::process::bounded::Output, StoreError> {
    let output = plurx_core::process::bounded::output_command_clean_env(
        command,
        wall,
        limit,
        plurx_core::process::ChildWork::background("progressive subtitle translation"),
    )
    .await
    .map_err(|error| {
        failure(match error.kind() {
            std::io::ErrorKind::Interrupted => "worker_interrupted",
            std::io::ErrorKind::TimedOut if inference => "inference_timeout",
            _ if inference => "inference_failed",
            _ => "decode_failed",
        })
    })?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(failure(if inference {
            "inference_failed"
        } else {
            "decode_failed"
        }))
    }
}

#[cfg(unix)]
async fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, StoreError> {
    let file = plurx_core::fs_secure::open_read_nofollow(path)
        .await
        .map_err(|_| failure("output_invalid"))?;
    if !file
        .metadata()
        .await
        .map_err(|_| failure("output_invalid"))?
        .is_file()
    {
        return Err(failure("output_invalid"));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| failure("output_invalid"))?;
    if bytes.len() > limit {
        Err(failure("segment_too_large"))
    } else {
        Ok(bytes)
    }
}

fn failure(reason: &str) -> StoreError {
    StoreError::Task(reason.into())
}

fn interrupted(cancel: &CancellationToken) -> Result<(), StoreError> {
    if cancel.is_cancelled() {
        Err(failure("worker_interrupted"))
    } else {
        Ok(())
    }
}

/// Source bytes, a complete held local identity, and the authoritative revision
/// used for their hash. A scanner/sample attestation cannot construct this.
pub(crate) struct VerifiedSource {
    fence: SourceFence,
    identity: FileIdentity,
    revision: SourceRevision,
    sha256: String,
}

impl VerifiedSource {
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    pub(crate) fn identity(&self) -> FileIdentity {
        self.identity
    }

    pub(crate) fn revision(&self) -> &SourceRevision {
        &self.revision
    }

    pub(crate) fn object_version(&self) -> &str {
        self.fence.object_version()
    }

    pub(crate) fn unchanged(&self) -> bool {
        self.fence.unchanged() && std_file_identity(&self.fence.handle).ok() == Some(self.identity)
    }

    async fn recheck(
        &self,
        state: &AppState,
        file: &MediaFile,
        cancel: &CancellationToken,
    ) -> Result<(), StoreError> {
        interrupted(cancel)?;
        if !self.unchanged() {
            return Err(failure("source_changed"));
        }
        check_revision(state, file, &self.revision, cancel).await?;
        let current = tokio::select! {
            () = cancel.cancelled() => return Err(failure("worker_interrupted")),
            opened = tokio::time::timeout(Duration::from_secs(2),
                open_source_playback_fence(file, Some(self.fence.object_version()))) => {
                opened.map_err(|_| failure("worker_source_unavailable"))?
                    .map_err(|_| failure("worker_source_unavailable"))?
            }
        };
        if std_file_identity(&current.handle).ok() != Some(self.identity) {
            return Err(failure("source_changed"));
        }
        interrupted(cancel)
    }
}

fn revision_matches(file: &MediaFile, expected: &SourceRevision) -> bool {
    file.id == expected.file_id
        && file.size == expected.source_size
        && file.mtime == expected.source_mtime
        && expected.source_size > 0
}

async fn check_revision(
    state: &AppState,
    file: &MediaFile,
    expected: &SourceRevision,
    cancel: &CancellationToken,
) -> Result<(), StoreError> {
    if !revision_matches(file, expected) {
        return Err(failure("source_changed"));
    }
    let current = tokio::select! {
        () = cancel.cancelled() => return Err(failure("worker_interrupted")),
        result = tokio::time::timeout(Duration::from_secs(2), state.store.get_file(expected.file_id)) => {
            result.map_err(|_| failure("worker_source_unavailable"))??
        }
    };
    match current {
        Some(current) if revision_matches(&current, expected) && current.path == file.path => {
            Ok(())
        }
        _ => Err(failure("source_changed")),
    }
}

fn hash_source(
    mut held: std::fs::File,
    expected: FileIdentity,
    cancel: &CancellationToken,
) -> Result<String, StoreError> {
    interrupted(cancel)?;
    if std_file_identity(&held).ok() != Some(expected) {
        return Err(failure("source_changed"));
    }
    held.seek(SeekFrom::Start(0))
        .map_err(|_| failure("worker_source_unavailable"))?;
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        interrupted(cancel)?;
        let length = held
            .read(&mut buffer)
            .map_err(|_| failure("worker_source_unavailable"))?;
        if length == 0 {
            break;
        }
        bytes = bytes
            .checked_add(length as u64)
            .ok_or_else(|| failure("source_changed"))?;
        if bytes > expected.size {
            return Err(failure("source_changed"));
        }
        hash.update(&buffer[..length]);
    }
    interrupted(cancel)?;
    if bytes != expected.size || std_file_identity(&held).ok() != Some(expected) {
        return Err(failure("source_changed"));
    }
    Ok(hex::encode(hash.finalize()))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn verify_source(
    state: &AppState,
    file: &MediaFile,
    expected: &SourceRevision,
    fence: &JobFence,
    admission: &FragmentAdmission,
    manager_cancel: &CancellationToken,
) -> Result<VerifiedSource, StoreError> {
    let cancel = manager_cancel.child_token();
    let operation = async {
        use plurx_core::store::background_jobs_transcription::SubtitlePhase;
        expected.validate()?;
        interrupted(&cancel)?;
        check_revision(state, file, expected, &cancel).await?;
        phase(fence, SubtitlePhase::CheckingSource, &cancel).await?;
        let source = open_source_playback_fence(file, None)
            .await
            .map_err(|_| failure("worker_source_unavailable"))?;
        let identity =
            std_file_identity(&source.handle).map_err(|_| failure("worker_source_unavailable"))?;
        if u64::try_from(expected.source_size).ok() != Some(identity.size) {
            return Err(failure("source_changed"));
        }
        let held = source
            .handle
            .try_clone()
            .map_err(|_| failure("worker_source_unavailable"))?;
        let hash_cancel = cancel.clone();
        // Never race/drop this JoinHandle on Stop. The owned loop checks the
        // manager-derived token between bounded chunks; always await its exit.
        let sha256 = tokio::task::spawn_blocking(move || hash_source(held, identity, &hash_cancel))
            .await
            .map_err(|_| failure("worker_source_unavailable"))??;
        let verified = VerifiedSource {
            fence: source,
            identity,
            revision: expected.clone(),
            sha256,
        };
        verified.recheck(state, file, &cancel).await?;
        Ok(verified)
    };
    tokio::pin!(operation);
    tokio::select! {
        result = &mut operation => result,
        () = crate::subtitle_transcription::watch_capacity(&state.transcode, admission) => {
            cancel.cancel();
            operation.await
        }
    }
}

#[cfg(unix)]
fn timestamp_ms(value: &str) -> Result<i64, StoreError> {
    let invalid = || failure("output_invalid");
    let parts: Vec<_> = value.split(':').collect();
    if parts.len() != 3 || !(2..=6).contains(&parts[0].len()) || parts[1].len() != 2 {
        return Err(invalid());
    }
    let seconds: Vec<_> = parts[2].split('.').collect();
    if seconds.len() != 2 || seconds[0].len() != 2 || seconds[1].len() != 3 {
        return Err(invalid());
    }
    let number = |s: &str| -> Result<i64, StoreError> {
        if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
            return Err(invalid());
        }
        s.parse().map_err(|_| invalid())
    };
    let (hours, minutes, seconds, millis) = (
        number(parts[0])?,
        number(parts[1])?,
        number(seconds[0])?,
        number(seconds[1])?,
    );
    if minutes >= 60 || seconds >= 60 || millis % 10 != 0 {
        return Err(invalid());
    }
    hours
        .checked_mul(3_600_000)
        .and_then(|n| n.checked_add(minutes * 60_000 + seconds * 1_000 + millis))
        .ok_or_else(invalid)
}

/// Intersect time domains only. Complete cue text is retained literally; this
/// never promises word alignment or silently deduplicates repeated speech.
#[cfg(unix)]
fn owned_cues(
    vtt: &str,
    asset_id: &str,
    core_index: i64,
    anchor_sample: i64,
    decoded_samples: i64,
    core_begin_ms: i64,
    core_end_ms: i64,
) -> Result<Vec<TranslationCue>, StoreError> {
    if vtt.len() > 256 * 1024 || !vtt.starts_with("WEBVTT\n") || vtt.contains('\r') {
        return Err(failure("output_invalid"));
    }
    let decoded_end = anchor_sample
        .checked_add(decoded_samples)
        .ok_or_else(|| failure("output_invalid"))?;
    let begin = core_begin_ms
        .checked_mul(RATE / 1_000)
        .ok_or_else(|| failure("output_invalid"))?;
    let end = core_end_ms
        .checked_mul(RATE / 1_000)
        .ok_or_else(|| failure("output_invalid"))?;
    if anchor_sample < 0 || decoded_samples <= 0 || end <= begin || anchor_sample > begin {
        return Err(failure("source_clock_unsupported"));
    }
    let mut previous_start = None;
    let mut cues = Vec::new();
    let mut blocks = vtt.split("\n\n");
    if blocks.next() != Some("WEBVTT") {
        return Err(failure("output_invalid"));
    }
    for block in blocks {
        if block.trim().is_empty() {
            continue;
        }
        let mut lines = block.lines();
        let first = lines.next().ok_or_else(|| failure("output_invalid"))?;
        let timing = if first.contains(" --> ") {
            first
        } else {
            if first.is_empty() || first.len() > 128 || first.chars().any(char::is_control) {
                return Err(failure("output_invalid"));
            }
            lines.next().ok_or_else(|| failure("output_invalid"))?
        };
        let (from, to) = timing
            .split_once(" --> ")
            .ok_or_else(|| failure("output_invalid"))?;
        let (from, to) = (timestamp_ms(from)?, timestamp_ms(to)?);
        if to <= from || previous_start.is_some_and(|p| from < p) {
            return Err(failure("output_invalid"));
        }
        previous_start = Some(from);
        let text = lines.collect::<Vec<_>>().join("\n");
        if text.trim().is_empty()
            || text.len() > 4_096
            || text
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(failure("output_invalid"));
        }
        let absolute = |ms: i64| {
            ms.checked_mul(RATE / 1_000)
                .and_then(|sample| anchor_sample.checked_add(sample))
                .ok_or_else(|| failure("output_invalid"))
        };
        let start = absolute(from)?.max(begin);
        let stop = absolute(to)?.min(end).min(decoded_end);
        if stop <= start {
            continue;
        }
        let (start_ms, end_ms) = (start / (RATE / 1_000), stop / (RATE / 1_000));
        if end_ms <= start_ms {
            continue;
        }
        if cues.len() >= MAX_TRANSLATION_CUES {
            return Err(failure("segment_too_large"));
        }
        cues.push(TranslationCue {
            cue_id: cue_id(asset_id, core_index, cues.len(), start_ms, end_ms, &text)?,
            start_ms,
            end_ms,
            text,
        });
    }
    // An actually decoded digital-silent core may use empty coverage, but an
    // empty recognizer result over speech is not established silence. The
    // caller owns that distinction and must not turn failures into coverage.
    Ok(cues)
}

#[cfg(unix)]
fn rewind(file: &std::fs::File) -> Result<(), StoreError> {
    let mut reader = file;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| failure("worker_source_unavailable"))?;
    Ok(())
}

/// Initial route is an aligned, single-video/single-audio common source clock.
/// Other clock shapes remain explicit refusals; the controlled delayed marker
/// does not qualify arbitrary edit lists, discontinuities or nonzero origins.
#[cfg(unix)]
async fn aligned_clock(source: &VerifiedSource) -> Result<(), StoreError> {
    let probe = crate::ffmpeg::ffprobe_bin();
    if !Path::new(&probe).is_absolute() {
        return Err(failure("engine_unavailable"));
    }
    rewind(&source.fence.handle)?;
    let mut command = tokio::process::Command::new(probe);
    command.args([
        "-v",
        "error",
        "-protocol_whitelist",
        "file,pipe",
        "-show_entries",
        "stream=codec_type,start_time:format=start_time",
        "-of",
        "json",
        "/dev/fd/3",
    ]);
    crate::ffmpeg::inherit_file_descriptors(&mut command, &[(&source.fence.handle, 3)]);
    let output = child_output(&mut command, Duration::from_secs(15), 64 * 1024, false).await?;
    if output.stderr.iter().any(|byte| !byte.is_ascii_whitespace()) {
        return Err(failure("source_clock_unsupported"));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| failure("source_clock_unsupported"))?;
    let zero = |value: &serde_json::Value| {
        value
            .as_str()
            .and_then(|v| v.parse::<f64>().ok())
            .is_some_and(|v| v == 0.0)
    };
    if !zero(&value["format"]["start_time"]) {
        return Err(failure("source_clock_unsupported"));
    }
    let streams = value["streams"]
        .as_array()
        .ok_or_else(|| failure("source_clock_unsupported"))?;
    for kind in ["video", "audio"] {
        let matched: Vec<_> = streams.iter().filter(|s| s["codec_type"] == kind).collect();
        if matched.len() != 1 || !zero(&matched[0]["start_time"]) {
            return Err(failure("source_clock_unsupported"));
        }
    }
    Ok(())
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum Purpose {
    Range,
    TranslationSelfTest,
}

#[cfg(unix)]
#[derive(Clone, Copy)]
struct Window {
    purpose: Purpose,
    core_begin_ms: i64,
    core_end_ms: i64,
    begin_ms: i64,
    end_ms: i64,
    audio_index: i64,
}

#[cfg(unix)]
struct DecodedPcm {
    file: std::fs::File,
    identity: FileIdentity,
    clock: ClockObservation,
    has_signal: bool,
}

#[cfg(unix)]
async fn decode_window(
    source: &std::fs::File,
    window: Window,
    scratch: &Path,
    decoder: &str,
    cancel: &CancellationToken,
) -> Result<DecodedPcm, StoreError> {
    interrupted(cancel)?;
    let (begin, end) = (window.begin_ms, window.end_ms);
    let (begin_sample, end_sample) = (begin * 16, end * 16);
    let preroll = (begin - 2_000).max(0);
    let wav = scratch.join("audio.wav");
    if !Path::new(decoder).is_absolute() {
        return Err(failure("engine_unavailable"));
    }
    rewind(source)?;
    let mut decode = tokio::process::Command::new(decoder);
    let filter = format!(
            "asettb=1/16000,ashowinfo,aresample=16000:async=1:first_pts={begin_sample}:min_comp=0:min_hard_comp=0:max_soft_comp=0,atrim=start_pts={begin_sample}:end_pts={end_sample},ashowinfo,asetpts=PTS-STARTPTS"
        );
    decode
        .args([
            "-nostdin",
            "-v",
            "info",
            "-xerror",
            "-err_detect",
            "explode",
            "-y",
            "-copyts",
            "-start_at_zero",
            "-protocol_whitelist",
            "file,pipe",
            "-threads",
            "2",
            "-filter_threads",
            "1",
            "-ss",
            &format!("{}.{:03}", preroll / 1_000, preroll % 1_000),
            "-i",
            "/dev/fd/3",
            "-map",
            &format!("0:a:{}", window.audio_index),
            "-vn",
            "-sn",
            "-dn",
            "-af",
            &filter,
            "-threads",
            "2",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
            "-fs",
            &(MAX_PCM_BYTES + 1).to_string(),
            // atrim alone is not a demux/decode termination bound. The
            // filter resets retained PTS to zero, so output -t safely
            // terminates after precisely this window while the observed
            // source anchor remains captured before asetpts.
            "-t",
            &format!("{}.{:03}", (end - begin) / 1_000, (end - begin) % 1_000),
        ])
        .arg(&wav);
    crate::ffmpeg::inherit_file_descriptors(&mut decode, &[(source, 3)]);
    let wall = match window.purpose {
        Purpose::Range => Duration::from_secs(120),
        Purpose::TranslationSelfTest => Duration::from_secs(30),
    };
    let decoded = child_output(&mut decode, wall, MAX_CHILD_LOG, false).await?;
    let clock = observe_clock(&decoded.stderr, begin_sample, end_sample)?;
    let pcm = plurx_core::fs_secure::open_read_nofollow(&wav)
        .await
        .map_err(|_| failure("decode_failed"))?
        .into_std()
        .await;
    let pcm_identity = std_file_identity(&pcm).map_err(|_| failure("decode_failed"))?;
    let mut held = pcm.try_clone().map_err(|_| failure("decode_failed"))?;
    let scan_cancel = cancel.clone();
    let scan_clock = ClockObservation {
        raw_first_sample: clock.raw_first_sample,
        anchor_sample: clock.anchor_sample,
        samples: clock.samples,
    };
    let has_signal = tokio::task::spawn_blocking(move || {
        inspect_pcm(
            &mut held,
            &scan_clock,
            window.core_begin_ms * 16,
            window.core_end_ms * 16,
            &scan_cancel,
        )
    })
    .await
    .map_err(|_| failure("decode_failed"))??;
    interrupted(cancel)?;
    Ok(DecodedPcm {
        file: pcm,
        identity: pcm_identity,
        clock,
        has_signal,
    })
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
async fn infer_window(
    decoded: &DecodedPcm,
    window: Window,
    asset_id: &str,
    core_index: i64,
    language: &str,
    engine: &Engine,
    model: &ManagedModel,
    scratch: &Path,
    fence: &JobFence,
    cancel: &CancellationToken,
) -> Result<Vec<TranslationCue>, StoreError> {
    use plurx_core::store::background_jobs_transcription::SubtitlePhase;
    interrupted(cancel)?;
    crate::subtitle_transcription::managed_artifacts_unchanged(engine, model)?;
    let mut cues = Vec::new();
    if decoded.has_signal {
        phase(fence, SubtitlePhase::Transcribing, cancel).await?;
        rewind(&decoded.file)?;
        rewind(&model.handle)?;
        let invocation =
            crate::subtitle_transcription::engine_invocation(engine, scratch, cancel).await?;
        let output_base = scratch.join("caption");
        let mut whisper = tokio::process::Command::new(&invocation.program);
        whisper
            .args([
                "--threads",
                "2",
                "--processors",
                "1",
                "--no-gpu",
                "--language",
                language,
                "--translate",
                "--output-vtt",
                "--no-prints",
                "--model",
                "/dev/fd/5",
                "--file",
                "/dev/fd/3",
                "--output-file",
            ])
            .arg(&output_base);
        crate::ffmpeg::inherit_file_descriptors(
            &mut whisper,
            &[(&decoded.file, 3), (&model.handle, 5), (&engine.handle, 7)],
        );
        child_output(&mut whisper, Duration::from_secs(120), 256 * 1024, true).await?;
        if !invocation.unchanged() {
            return Err(failure("packaged_engine_changed"));
        }
        let bytes = read_bounded(&output_base.with_extension("vtt"), 256 * 1024).await?;
        let vtt = std::str::from_utf8(&bytes)
            .map_err(|_| failure("output_invalid"))?
            .replace("\r\n", "\n");
        cues = owned_cues(
            &vtt,
            asset_id,
            core_index,
            decoded.clock.anchor_sample,
            decoded.clock.samples,
            window.core_begin_ms,
            window.core_end_ms,
        )?;
        if cues.is_empty() {
            return Err(failure("output_invalid"));
        }
    }
    crate::subtitle_transcription::managed_artifacts_unchanged(engine, model)?;
    if std_file_identity(&decoded.file).ok() != Some(decoded.identity) {
        return Err(failure("output_invalid"));
    }
    interrupted(cancel)?;
    Ok(cues)
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn translate_range(
    state: &AppState,
    file: &MediaFile,
    source: &VerifiedSource,
    recipe: &RangeRecipe,
    engine: &Engine,
    model: &ManagedModel,
    fence: &JobFence,
    admission: &FragmentAdmission,
    manager_cancel: &CancellationToken,
) -> Result<SegmentDraft, StoreError> {
    let cancel = manager_cancel.child_token();
    let operation = plurx_core::process::bounded::cancellable(cancel.clone(), async {
        use plurx_core::store::background_jobs_transcription::SubtitlePhase;
        recipe.validate()?;
        interrupted(&cancel)?;
        if recipe.source != source.revision
            || recipe.source_sha256 != source.sha256
            || file.audio_streams.len() != 1
            || recipe.audio_index != 0
            || file.audio_streams[0].index != recipe.audio_index
        {
            return Err(failure("source_changed"));
        }
        if recipe.profile.model_sha256 != model.sha256 {
            return Err(failure("artifact_digest_mismatch"));
        }
        if recipe.profile.adapter_contract != engine.adapter_contract {
            return Err(failure("packaged_engine_unqualified"));
        }
        crate::subtitle_transcription::managed_artifacts_unchanged(engine, model)?;
        source.recheck(state, file, &cancel).await?;
        phase(fence, SubtitlePhase::Decoding, &cancel).await?;
        aligned_clock(source).await?;
        let (core_begin, core_end) = recipe.core_bounds()?;
        let begin = (core_begin - i64::from(recipe.profile.left_context_ms)).max(0);
        let end = (core_end + i64::from(recipe.profile.right_context_ms)).min(recipe.duration_ms);
        let root = state.runtime_cache_dir.join("subtitle-translation");
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|_| failure("output_invalid"))?;
        let scratch = crate::subtitle_transcription::private_attempt(&root)
            .map_err(|_| failure("output_invalid"))?;
        let window = Window {
            purpose: Purpose::Range,
            core_begin_ms: core_begin,
            core_end_ms: core_end,
            begin_ms: begin,
            end_ms: end,
            audio_index: recipe.audio_index,
        };
        let decoded = decode_window(
            &source.fence.handle,
            window,
            scratch.path(),
            &crate::ffmpeg::ffmpeg_bin(),
            &cancel,
        )
        .await?;
        source.recheck(state, file, &cancel).await?;
        let cues = infer_window(
            &decoded,
            window,
            &recipe.asset_id,
            recipe.core_index,
            &recipe.spoken_language,
            engine,
            model,
            scratch.path(),
            fence,
            &cancel,
        )
        .await?;
        phase(fence, SubtitlePhase::Validating, &cancel).await?;
        crate::subtitle_transcription::managed_artifacts_unchanged(engine, model)?;
        if std_file_identity(&decoded.file).ok() != Some(decoded.identity) {
            return Err(failure("output_invalid"));
        }
        source.recheck(state, file, &cancel).await?;
        let token = tokio::select! {
            () = cancel.cancelled() => return Err(failure("worker_interrupted")),
            result = tokio::time::timeout(Duration::from_secs(2), fence.snapshot()) => {
                result.map_err(|_| failure("worker_interrupted"))?
                    .ok_or_else(|| failure("worker_interrupted"))?
            }
        };
        interrupted(&cancel)?;
        let draft = SegmentDraft {
            asset_id: recipe.asset_id.clone(),
            core_index: recipe.core_index,
            start_ms: core_begin,
            end_ms: core_end,
            cues,
            provenance: ExecutionProvenance {
                engine_sha256: engine.binary_sha256.clone(),
                model_sha256: model.sha256.clone(),
                operation_epoch: recipe.operation_epoch,
                platform: engine.platform.clone(),
                profile_id: recipe.profile.id()?,
                source_sha256: source.sha256.clone(),
                worker_fence: token.fence,
            },
        };
        draft
            .validate(recipe)
            .map_err(|_| failure("output_invalid"))?;
        tracing::debug!(
            raw_first_sample = decoded.clock.raw_first_sample,
            retained_anchor_sample = decoded.clock.anchor_sample,
            decoded_samples = decoded.clock.samples,
            "translation source clock observed"
        );
        Ok(draft)
    });
    tokio::pin!(operation);
    tokio::select! {
        result = &mut operation => result,
        () = crate::subtitle_transcription::watch_capacity(&state.transcode, admission) => {
            cancel.cancel();
            operation.await
        }
    }
}

#[cfg(not(unix))]
#[allow(clippy::too_many_arguments, unused_variables)]
pub(crate) async fn translate_range(
    state: &AppState,
    file: &MediaFile,
    source: &VerifiedSource,
    recipe: &RangeRecipe,
    engine: &Engine,
    model: &ManagedModel,
    fence: &JobFence,
    admission: &FragmentAdmission,
    manager_cancel: &CancellationToken,
) -> Result<SegmentDraft, StoreError> {
    Err(failure("source_clock_unsupported"))
}

/// Qualify the actual foreign-input→English executable path under the same
/// owned decoder, observed clock, PCM validation, fixed argv and cue validator.
/// This returns no readiness receipt and makes no source/coverage publication.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn self_test_translation(
    state: &AppState,
    engine: &Engine,
    model: &ManagedModel,
    profile: &plurx_core::store::subtitle_translation::TranslationProfile,
    fence: &JobFence,
    admission: &FragmentAdmission,
    manager_cancel: &CancellationToken,
) -> Result<(), StoreError> {
    let cancel = manager_cancel.child_token();
    let operation = plurx_core::process::bounded::cancellable(cancel.clone(), async {
        use super::profile as selected;
        use plurx_core::store::background_jobs_transcription::SubtitlePhase;
        if *profile != selected::selected_profile() {
            return Err(failure("translation_unverified"));
        }
        if model.sha256 != profile.model_sha256 {
            return Err(failure("artifact_digest_mismatch"));
        }
        if engine.adapter_contract != profile.adapter_contract {
            return Err(failure("packaged_engine_unqualified"));
        }
        crate::subtitle_transcription::managed_artifacts_unchanged(engine, model)?;
        if hex::encode(Sha256::digest(selected::SELF_TEST_FIXTURE)) != selected::SELF_TEST_SHA256 {
            return Err(failure("model_self_test_failed"));
        }
        // Keep the compiled fixture's redistribution descriptor bounded too;
        // packaging installs this attribution and the complete source license.
        if selected::self_test_notice().len() > 64 * 1024 {
            return Err(failure("model_self_test_failed"));
        }
        let root = state.runtime_cache_dir.join("subtitle-translation");
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|_| failure("output_invalid"))?;
        let scratch = crate::subtitle_transcription::private_attempt(&root)
            .map_err(|_| failure("output_invalid"))?;
        let fixture_path = scratch.path().join("foreign-source.wav");
        let write_cancel = cancel.clone();
        // The compiled rights-cleared source is staged once in this exclusive
        // 0700 attempt. Close the writer and hand the decoder a read-only FD.
        let source = tokio::task::spawn_blocking(move || {
            use std::{io::Write, os::unix::fs::OpenOptionsExt};
            interrupted(&write_cancel)?;
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&fixture_path)
                .map_err(|_| failure("output_invalid"))?;
            for chunk in selected::SELF_TEST_FIXTURE.chunks(128 * 1024) {
                interrupted(&write_cancel)?;
                output
                    .write_all(chunk)
                    .map_err(|_| failure("output_invalid"))?;
            }
            output.flush().map_err(|_| failure("output_invalid"))?;
            drop(output);
            interrupted(&write_cancel)?;
            plurx_core::fs_secure::open_read_nofollow_blocking(&fixture_path)
                .map_err(|_| failure("output_invalid"))
        })
        .await
        .map_err(|_| failure("output_invalid"))??;
        let source_identity = std_file_identity(&source).map_err(|_| failure("output_invalid"))?;
        phase(fence, SubtitlePhase::Decoding, &cancel).await?;
        let duration_ms = selected::SELF_TEST_SAMPLES / 16;
        let window = Window {
            purpose: Purpose::TranslationSelfTest,
            core_begin_ms: 0,
            core_end_ms: duration_ms,
            begin_ms: 0,
            end_ms: duration_ms,
            audio_index: 0,
        };
        let decoded = decode_window(
            &source,
            window,
            scratch.path(),
            &crate::ffmpeg::ffmpeg_bin(),
            &cancel,
        )
        .await?;
        let cues = infer_window(
            &decoded,
            window,
            selected::SELF_TEST_SHA256,
            0,
            selected::SELF_TEST_LANGUAGE,
            engine,
            model,
            scratch.path(),
            fence,
            &cancel,
        )
        .await?;
        phase(fence, SubtitlePhase::Validating, &cancel).await?;
        if std_file_identity(&source).ok() != Some(source_identity) {
            return Err(failure("source_changed"));
        }
        crate::subtitle_transcription::managed_artifacts_unchanged(engine, model)?;
        interrupted(&cancel)?;
        if !selected::english_self_test(&cues) {
            return Err(failure("model_self_test_failed"));
        }
        Ok(())
    });
    tokio::pin!(operation);
    tokio::select! {
        result = &mut operation => result,
        () = crate::subtitle_transcription::watch_capacity(&state.transcode, admission) => {
            cancel.cancel();
            operation.await
        }
    }
}

#[cfg(not(unix))]
#[allow(clippy::too_many_arguments, unused_variables)]
pub(crate) async fn self_test_translation(
    state: &AppState,
    engine: &Engine,
    model: &ManagedModel,
    profile: &plurx_core::store::subtitle_translation::TranslationProfile,
    fence: &JobFence,
    admission: &FragmentAdmission,
    manager_cancel: &CancellationToken,
) -> Result<(), StoreError> {
    Err(failure("source_clock_unsupported"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;

    fn frame(filter: u8, pts: i64, samples: i64) -> String {
        format!("[Parsed_ashowinfo_{filter} @0] n:0 pts:{pts} pts_time:0 fmt:s16 channels:1 rate:16000 nb_samples:{samples}\n")
    }

    #[test]
    fn observed_clock_refuses_seek_overshoot_hidden_by_padding_and_discontinuity() {
        let valid = frame(1, 14_000, 2_000) + &frame(1, 16_000, 2_000) + &frame(4, 16_000, 2_000);
        let clock = observe_clock(valid.as_bytes(), 16_000, 18_000).expect("test setup");
        assert_eq!(
            (clock.raw_first_sample, clock.anchor_sample, clock.samples),
            (14_000, 16_000, 2_000)
        );
        let overshoot = frame(1, 17_000, 1_000) + &frame(4, 16_000, 2_000);
        assert!(observe_clock(overshoot.as_bytes(), 16_000, 18_000).is_err());
        let gap = frame(1, 14_000, 1_000) + &frame(1, 16_000, 2_000) + &frame(4, 16_000, 2_000);
        assert!(observe_clock(gap.as_bytes(), 16_000, 18_000).is_err());
        assert!(observe_clock(b"requested -ss1.000", 16_000, 18_000).is_err());
    }

    #[test]
    fn cues_use_observed_absolute_clock_preserve_literals_and_reject_invalid_times() {
        let asset = "ab".repeat(32);
        let cues = owned_cues(
            "WEBVTT\n\n00:00:00.000 --> 00:00:04.000\nA & B <b> literal\n\n",
            &asset,
            1,
            18 * RATE,
            24 * RATE,
            20_000,
            40_000,
        )
        .expect("test setup");
        assert_eq!(cues.len(), 1);
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (20_000, 22_000));
        assert_eq!(cues[0].text, "A & B <b> literal");
        assert_eq!(
            cues[0].cue_id,
            cue_id(&asset, 1, 0, 20_000, 22_000, &cues[0].text).expect("test setup")
        );
        for invalid in [
            "00:00:00.000 --> 00:00:00.000",
            "00:00:02.000 --> 00:00:01.000",
            "00:00:00.001 --> 00:00:02.000",
            "00:00:60.000 --> 00:01:01.000",
            "99999999999999999999:00:00.000 --> 99999999999999999999:00:01.000",
        ] {
            assert!(owned_cues(
                &format!("WEBVTT\n\n{invalid}\nx\n\n"),
                &asset,
                1,
                18 * RATE,
                24 * RATE,
                20_000,
                40_000
            )
            .is_err());
        }
    }

    #[test]
    fn cue_overlap_is_ordered_but_no_text_deduplication_is_fabricated() {
        let asset = "ab".repeat(32);
        let vtt = "WEBVTT\n\n00:00:00.000 --> 00:00:03.000\nrepeat\n\n00:00:02.000 --> 00:00:04.000\nrepeat\n\n00:00:04.000 --> 00:00:05.000\noutside\n\n";
        let cues = owned_cues(vtt, &asset, 0, 0, 4 * RATE, 0, 4_000).expect("test setup");
        assert_eq!(cues.len(), 2);
        assert!(cues.iter().all(|cue| cue.text == "repeat"));
        let decreasing = vtt.replace("00:00:02.000 -->", "00:00:00.000 -->").replace(
            "00:00:00.000 --> 00:00:03.000",
            "00:00:01.000 --> 00:00:03.000",
        );
        assert!(owned_cues(&decreasing, &asset, 0, 0, 4 * RATE, 0, 4_000).is_err());
    }

    fn wav(samples: &[i16]) -> std::fs::File {
        let mut file = tempfile::tempfile().expect("test setup");
        let bytes = u32::try_from(samples.len() * 2).expect("test setup");
        file.write_all(b"RIFF").expect("test setup");
        file.write_all(&(36 + bytes).to_le_bytes())
            .expect("test setup");
        file.write_all(b"WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data")
            .expect("test setup");
        file.write_all(&bytes.to_le_bytes()).expect("test setup");
        for sample in samples {
            file.write_all(&sample.to_le_bytes()).expect("test setup");
        }
        file
    }

    #[test]
    fn exact_owned_zero_pcm_is_empty_coverage_but_short_or_cancelled_pcm_is_not() {
        let clock = ClockObservation {
            raw_first_sample: 0,
            anchor_sample: 0,
            samples: 4,
        };
        let cancel = CancellationToken::new();
        let mut file = wav(&[7, 0, 0, 9]);
        assert!(!inspect_pcm(&mut file, &clock, 1, 3, &cancel).expect("test setup"));
        assert!(inspect_pcm(&mut file, &clock, 0, 3, &cancel).expect("test setup"));
        assert!(inspect_pcm(&mut file, &clock, 1, 5, &cancel).is_err());
        cancel.cancel();
        assert!(inspect_pcm(&mut file, &clock, 1, 3, &cancel).is_err());
        let mut malformed = wav(&[0, 0]);
        malformed.seek(SeekFrom::Start(0)).expect("test setup");
        malformed.write_all(b"BAD!").expect("test setup");
        assert!(inspect_pcm(&mut malformed, &clock, 0, 2, &CancellationToken::new()).is_err());
    }

    #[test]
    fn source_hash_checks_initial_cancellation_and_held_identity_before_qualification() {
        let mut source = tempfile::tempfile().expect("test setup");
        source.write_all(b"exact source bytes").expect("test setup");
        let identity = std_file_identity(&source).expect("test setup");
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(hash_source(source.try_clone().expect("test setup"), identity, &cancel).is_err());
        let expected = hex::encode(Sha256::digest(b"exact source bytes"));
        assert_eq!(
            hash_source(
                source.try_clone().expect("test setup"),
                identity,
                &CancellationToken::new()
            )
            .expect("test setup"),
            expected
        );
        source.write_all(b"changed").expect("test setup");
        assert!(hash_source(source, identity, &CancellationToken::new()).is_err());
    }

    fn test_decoder() -> String {
        let configured = crate::ffmpeg::ffmpeg_bin();
        if Path::new(&configured).is_absolute() {
            return configured;
        }
        // Unit input discovery only. Production always obtains its trusted
        // absolute packaged utility and the common launcher rejects bare PATH.
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|directory| directory.join(&configured))
            .find(|path| path.is_file())
            .and_then(|path| path.canonicalize().ok())
            .expect("actual FFmpeg is required for the decoder regression")
            .to_string_lossy()
            .into_owned()
    }

    #[tokio::test]
    async fn actual_decoder_bounds_long_source_deep_seek_and_rewinds_held_fd() {
        let mut source = tempfile::tempfile().expect("test setup");
        let bytes = 3_600_u32 * 16_000 * 2;
        source.write_all(b"RIFF").expect("test setup");
        source
            .write_all(&(36 + bytes).to_le_bytes())
            .expect("test setup");
        source
            .write_all(b"WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data")
            .expect("test setup");
        source.write_all(&bytes.to_le_bytes()).expect("test setup");
        source.set_len(44 + u64::from(bytes)).expect("test setup");
        for sample in [8_000_u64, 1_800 * 16_000 + 8_000] {
            source
                .seek(SeekFrom::Start(44 + sample * 2))
                .expect("test setup");
            source
                .write_all(&12_000_i16.to_le_bytes())
                .expect("test setup");
        }
        let decoder = test_decoder();
        let cancel = CancellationToken::new();
        let started = std::time::Instant::now();
        for (begin_ms, end_ms, core_begin_ms, core_end_ms) in [
            (0, 3_000, 0, 2_000),
            (1_799_000, 1_802_000, 1_800_000, 1_801_000),
        ] {
            source.seek(SeekFrom::End(0)).expect("test setup");
            let scratch = tempfile::tempdir().expect("test setup");
            let window = Window {
                purpose: Purpose::Range,
                begin_ms,
                end_ms,
                core_begin_ms,
                core_end_ms,
                audio_index: 0,
            };
            let pcm = decode_window(&source, window, scratch.path(), &decoder, &cancel)
                .await
                .expect("test setup");
            assert_eq!(pcm.clock.anchor_sample, begin_ms * 16);
            assert_eq!(pcm.clock.samples, 48_000);
            assert!(pcm.clock.raw_first_sample <= pcm.clock.anchor_sample);
            assert!(pcm.has_signal);
            assert!((96_044..=96_512).contains(&pcm.identity.size)); // bounded actual WAV metadata
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "short/deep windows must not decode the full hour"
        );
    }

    #[tokio::test]
    async fn actual_timeout_and_cancellation_reap_owned_child_group_before_return() {
        for cancelled in [false, true] {
            let scratch = tempfile::tempdir().expect("test setup");
            let pid_path = scratch.path().join("pid");
            let mut command = tokio::process::Command::new("/bin/sh");
            command
                .args([
                    "-c",
                    "echo $$ > \"$1\"; /bin/sleep 60 & wait",
                    "owned-group",
                ])
                .arg(&pid_path);
            let cancel = CancellationToken::new();
            let operation = plurx_core::process::bounded::cancellable(
                cancel.clone(),
                child_output(
                    &mut command,
                    if cancelled {
                        Duration::from_secs(10)
                    } else {
                        Duration::from_millis(300)
                    },
                    4096,
                    false,
                ),
            );
            tokio::pin!(operation);
            if cancelled {
                tokio::select! {
                    result = &mut operation => panic!("child exited before cancellation: {result:?}"),
                    result = tokio::time::timeout(Duration::from_secs(2), async {
                        while tokio::fs::metadata(&pid_path).await.is_err() {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    }) => { result.expect("owned child must be observed before cancellation"); cancel.cancel(); },
                }
            }
            assert!(operation.await.is_err());
            let pgid = std::fs::read_to_string(pid_path)
                .expect("test setup")
                .trim()
                .to_owned();
            let until = tokio::time::Instant::now() + Duration::from_secs(3);
            let mut empty = 0;
            loop {
                let mut ps = tokio::process::Command::new("/bin/ps");
                ps.args(["-e", "-o", "pgid="]);
                let output = child_output(&mut ps, Duration::from_secs(2), 1024 * 1024, false)
                    .await
                    .expect("test setup");
                assert!(output.status.success());
                let alive = String::from_utf8(output.stdout)
                    .expect("test setup")
                    .lines()
                    .any(|line| line.trim() == pgid);
                empty = if alive { 0 } else { empty + 1 };
                if empty >= 2 {
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < until,
                    "owned child/group outlived timeout/cancellation"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}
