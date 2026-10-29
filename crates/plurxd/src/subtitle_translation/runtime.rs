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
    cue_id, ExecutionProvenance, SpeechTask, TranslationCue, MAX_TRANSLATION_CUES,
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

/// The whole private attempt stays reserved until TempDir has removed it.
/// Catalog coverage is durable; only these disposable execution files evict.
pub(crate) fn register_playback_cache(
    budget: &std::sync::Arc<crate::playback_cache::PlaybackCacheBudget>,
    runtime_root: &std::path::Path,
) {
    budget.register(std::sync::Arc::new(TranslationAttempts {
        root: runtime_root.join("subtitle-translation"),
    }));
}

struct TranslationAttempts {
    root: std::path::PathBuf,
}

#[async_trait::async_trait]
impl crate::playback_cache::CacheOwner for TranslationAttempts {
    async fn candidates(&self) -> std::io::Result<Vec<crate::playback_cache::CacheCandidate>> {
        let mut entries = match tokio::fs::read_dir(&self.root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut candidates = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let metadata = tokio::fs::symlink_metadata(entry.path()).await?;
            if metadata.is_dir() && entry.file_name().to_string_lossy().starts_with("attempt-") {
                candidates.push(crate::playback_cache::CacheCandidate {
                    path: entry.path(),
                    last_used: metadata
                        .modified()
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                });
            }
        }
        Ok(candidates)
    }

    async fn evict(
        &self,
        candidate: &crate::playback_cache::CacheCandidate,
    ) -> std::io::Result<bool> {
        if candidate.path.parent() != Some(self.root.as_path()) {
            return Ok(false);
        }
        let Some(name) = candidate
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| name.starts_with("attempt-"))
        else {
            return Ok(false);
        };
        match plurx_core::fs_secure::remove_bounded_directory_tree_child(&self.root, name, 64, 2)
            .await
        {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[cfg(unix)]
fn constrain_child_file(command: &mut tokio::process::Command, bytes: u64) {
    // SAFETY: the child hook performs only async-signal-safe libc operations.
    unsafe {
        command.pre_exec(move || {
            let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
            if libc::getrlimit(libc::RLIMIT_FSIZE, limit.as_mut_ptr()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut limit = limit.assume_init();
            limit.rlim_cur = limit.rlim_cur.min(bytes as libc::rlim_t);
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(unix)]
fn scratch_allowance(engine: &Engine, fixture_bytes: u64) -> u64 {
    let engine_bytes = if cfg!(target_os = "macos") {
        engine.identity.size
    } else {
        0
    };
    MAX_PCM_BYTES
        .saturating_add(256 * 1024)
        .saturating_add(engine_bytes)
        .saturating_add(fixture_bytes)
}

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

/// FFmpeg logs frame records and side-data continuations under the same
/// ashowinfo instance. Only records beginning with `n:` carry clock evidence.
/// Select them before the byte cap so metadata cannot displace the window's
/// final timestamps. Malformed frame records remain selected and fail parsing.
#[cfg(unix)]
fn clock_frame_record(line: &[u8]) -> bool {
    let instance = [
        b"[Parsed_ashowinfo_1 ".as_slice(),
        b"[Parsed_ashowinfo_4 ".as_slice(),
    ]
    .iter()
    .any(|marker| line.windows(marker.len()).any(|part| part == *marker));
    instance
        && line
            .windows(2)
            .rposition(|part| part == b"] ")
            .is_some_and(|end| line[end + 2..].starts_with(b"n:"))
}

#[cfg(unix)]
#[cfg(test)]
fn observe_clock(log: &[u8], begin: i64, end: i64) -> Result<ClockObservation, StoreError> {
    observe_source_clock(log, begin, end, 0)
}
#[cfg(unix)]
fn observe_source_clock(
    log: &[u8],
    begin: i64,
    end: i64,
    audio_start: i64,
) -> Result<ClockObservation, StoreError> {
    let log = std::str::from_utf8(log).map_err(|_| failure("decode_failed"))?;
    let mut raw_first = None;
    let mut raw_previous: Option<(i64, i64, i64)> = None;
    let mut anchor = None;
    let mut next = None;
    let mut samples = 0_i64;
    for line in log
        .lines()
        .filter(|line| clock_frame_record(line.as_bytes()))
    {
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
            if !(-14_400_000 * 16..=14_400_000 * 16).contains(&pts) {
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
    // after the requested window. Only the selected stream's verified leading
    // delay can justify padding; its rational source anchor stays authoritative.
    // Padding is legitimate only before the selected stream's measured start.
    // A later input seek overshoot is still refused, as are gaps/overlaps above.
    let delayed_start = begin < audio_start && (raw_first - audio_start).abs() <= 1;
    if (raw_first > begin && !delayed_start) || anchor != begin || next.is_none_or(|n| n > end) {
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

/// Resumable full-content hashing. The producer and read-only attestation use
/// exactly the same byte count, held identity and cancellation checks.
pub(crate) struct SourceHash {
    held: std::fs::File,
    expected: FileIdentity,
    hash: Sha256,
    bytes: u64,
}
impl SourceHash {
    pub(crate) fn new(mut held: std::fs::File, expected: FileIdentity) -> Result<Self, StoreError> {
        if std_file_identity(&held).ok() != Some(expected) {
            return Err(failure("source_changed"));
        }
        held.seek(SeekFrom::Start(0))
            .map_err(|_| failure("worker_source_unavailable"))?;
        Ok(Self {
            held,
            expected,
            hash: Sha256::new(),
            bytes: 0,
        })
    }
    /// One bounded I/O operation; the caller can relinquish CPU between chunks
    /// without discarding the held file position or already verified prefix.
    pub(crate) fn step(
        &mut self,
        cancel: &CancellationToken,
    ) -> Result<Option<String>, StoreError> {
        interrupted(cancel)?;
        let mut buffer = [0_u8; 128 * 1024];
        let length = self
            .held
            .read(&mut buffer)
            .map_err(|_| failure("worker_source_unavailable"))?;
        interrupted(cancel)?;
        if length == 0 {
            if self.bytes != self.expected.size
                || std_file_identity(&self.held).ok() != Some(self.expected)
            {
                return Err(failure("source_changed"));
            }
            return Ok(Some(hex::encode(self.hash.clone().finalize())));
        }
        self.bytes = self
            .bytes
            .checked_add(length as u64)
            .ok_or_else(|| failure("source_changed"))?;
        if self.bytes > self.expected.size {
            return Err(failure("source_changed"));
        }
        self.hash.update(&buffer[..length]);
        Ok(None)
    }
}
fn hash_source(
    held: std::fs::File,
    expected: FileIdentity,
    cancel: &CancellationToken,
) -> Result<String, StoreError> {
    interrupted(cancel)?;
    let mut hash = SourceHash::new(held, expected)?;
    loop {
        if let Some(digest) = hash.step(cancel)? {
            return Ok(digest);
        }
    }
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
        let source = tokio::select! {
            () = cancel.cancelled() => return Err(failure("worker_interrupted")),
            opened = tokio::time::timeout(Duration::from_secs(2), open_source_playback_fence(file, None)) => {
                opened.map_err(|_| failure("worker_source_unavailable"))?
                    .map_err(|_| failure("worker_source_unavailable"))?
            }
        };
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
        () = crate::subtitle_transcription::watch_speech_capacity(&state.transcode, admission) => {
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

/// Cues this core owns, plus how many valid cues the recognizer returned for
/// the whole decoded window (core and context).
#[cfg(unix)]
struct OwnedCues {
    cues: Vec<TranslationCue>,
    recognized: usize,
}

/// A signal-bearing core's result. An empty recognizer result over decoded
/// speech has not established silence, so it stays a failure. When the
/// recognizer did return cues but every one lies in this window's left or
/// right context, it attributed that speech to the neighbouring cores, whose
/// own windows cover it as their core; this core owns no cue, which is a valid
/// empty result. At the source's first and last core there is no context on
/// the outer side, so nothing can be attributed past the file's ends.
#[cfg(unix)]
fn signal_core_cues(owned: OwnedCues) -> Result<Vec<TranslationCue>, StoreError> {
    if owned.recognized == 0 {
        return Err(failure("output_invalid"));
    }
    Ok(owned.cues)
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
) -> Result<OwnedCues, StoreError> {
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
    let mut recognized = 0usize;
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
        // A cue past the decoded audio is not speech any core can own.
        if absolute(from)? < decoded_end {
            recognized += 1;
        }
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
    Ok(OwnedCues { cues, recognized })
}

#[cfg(unix)]
fn rewind(file: &std::fs::File) -> Result<(), StoreError> {
    let mut reader = file;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| failure("worker_source_unavailable"))?;
    Ok(())
}

/// Re-probe the held source, then use the same normalized container/audio
/// mapping advertised by options. Multiple audio tracks are selected by ordinal.
#[cfg(unix)]
async fn selected_clock(
    source: &VerifiedSource,
    ordinal: i64,
    container_ms: i64,
) -> Result<super::SourceClock, StoreError> {
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
        "stream=index,codec_type,start_time,start_pts,time_base,duration:stream_tags:format=start_time",
        "-of",
        "json",
        "/dev/fd/3",
    ]);
    crate::ffmpeg::inherit_file_descriptors(&mut command, &[(&source.fence.handle, 3)]);
    let output = child_output(&mut command, Duration::from_secs(15), 1024 * 1024, false).await?;
    if output.stderr.iter().any(|byte| !byte.is_ascii_whitespace()) {
        return Err(failure("source_clock_unsupported"));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| failure("source_clock_unsupported"))?;
    super::source_clock(&value, ordinal, container_ms).map_err(failure)
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
    audio_start_samples: i64,
}

/// The pinned whisper.cpp revision's include/whisper.h declares
/// WHISPER_CHUNK_SIZE=30 (306c88f4d1286aec1bf96e544632897886af5501;
/// scripts/subtitle-engine-manifest.json). A context window can need a second
/// acoustic encoder block even though its owned core is only thirty seconds.
/// Preserve the existing wall budget per block and cap the input at the
/// registered profile's core plus contexts, never at a caller-supplied limit.
#[cfg(unix)]
fn inference_budget(window: Window) -> Result<Duration, StoreError> {
    const ACOUSTIC_BLOCK_MS: i64 = 30_000;
    const SECONDS_PER_BLOCK: u64 = 120;
    let invalid = || failure("output_invalid");
    let span = window
        .end_ms
        .checked_sub(window.begin_ms)
        .filter(|span| *span > 0)
        .ok_or_else(invalid)?;
    let selected = super::profile::selected_profile();
    let maximum = i64::from(selected.core_ms)
        .checked_add(i64::from(selected.left_context_ms))
        .and_then(|span| span.checked_add(i64::from(selected.right_context_ms)))
        .ok_or_else(invalid)?;
    if window.begin_ms < 0
        || span > maximum
        || window.core_begin_ms < window.begin_ms
        || window.core_end_ms > window.end_ms
        || window
            .core_end_ms
            .checked_sub(window.core_begin_ms)
            .is_none_or(|core| core <= 0 || core > i64::from(selected.core_ms))
    {
        return Err(invalid());
    }
    let blocks = span
        .checked_add(ACOUSTIC_BLOCK_MS - 1)
        .ok_or_else(invalid)?
        / ACOUSTIC_BLOCK_MS;
    let seconds = u64::try_from(blocks)
        .map_err(|_| invalid())?
        .checked_mul(SECONDS_PER_BLOCK)
        .ok_or_else(invalid)?;
    Ok(Duration::from_secs(seconds))
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
            "1",
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
            "1",
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
    constrain_child_file(&mut decode, MAX_PCM_BYTES);
    crate::ffmpeg::inherit_file_descriptors(&mut decode, &[(source, 3)]);
    let wall = match window.purpose {
        Purpose::Range => Duration::from_secs(120),
        Purpose::TranslationSelfTest => Duration::from_secs(30),
    };
    let decoded = plurx_core::process::bounded::output_command_clean_env_selected_stderr(
        &mut decode,
        wall,
        MAX_CHILD_LOG,
        plurx_core::process::ChildWork::background("progressive subtitle translation"),
        clock_frame_record,
    )
    .await
    .map_err(|_| failure("decode_failed"))?;
    if !decoded.status.success() {
        return Err(failure("decode_failed"));
    }
    let clock = observe_source_clock(
        &decoded.stderr,
        begin_sample,
        end_sample,
        window.audio_start_samples,
    )?;
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
    task: SpeechTask,
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
                "--output-vtt",
                "--no-prints",
                "--model",
                "/dev/fd/5",
                "--file",
                "/dev/fd/3",
                "--output-file",
            ])
            .arg(&output_base);
        if task.is_translation() {
            whisper.arg("--translate");
        }
        constrain_child_file(&mut whisper, 256 * 1024);
        crate::ffmpeg::inherit_file_descriptors(
            &mut whisper,
            &[(&decoded.file, 3), (&model.handle, 5), (&engine.handle, 7)],
        );
        child_output(&mut whisper, inference_budget(window)?, 256 * 1024, true).await?;
        if !invocation.unchanged() {
            return Err(failure("packaged_engine_changed"));
        }
        let bytes = read_bounded(&output_base.with_extension("vtt"), 256 * 1024).await?;
        let vtt = std::str::from_utf8(&bytes)
            .map_err(|_| failure("output_invalid"))?
            .replace("\r\n", "\n");
        cues = signal_core_cues(owned_cues(
            &vtt,
            asset_id,
            core_index,
            decoded.clock.anchor_sample,
            decoded.clock.samples,
            window.core_begin_ms,
            window.core_end_ms,
        )?)?;
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
            || !file
                .audio_streams
                .iter()
                .any(|audio| audio.index == recipe.audio_index)
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
        let clock = selected_clock(
            source,
            recipe.audio_index,
            file.duration_ms
                .ok_or_else(|| failure("source_clock_unsupported"))?,
        )
        .await?;
        let catalog = super::selected_file_capability(state, file, recipe.audio_index)
            .await
            .map_err(failure)?;
        if clock.origin_samples != catalog.origin_samples
            || clock.audio_start_samples != catalog.audio_start_samples
            || clock.duration_ms != recipe.duration_ms
        {
            return Err(failure("source_changed"));
        }
        let (core_begin, core_end) = recipe.core_bounds()?;
        let begin = (core_begin - i64::from(recipe.profile.left_context_ms)).max(0);
        let end = (core_end + i64::from(recipe.profile.right_context_ms)).min(recipe.duration_ms);
        let root = state.runtime_cache_dir.join("subtitle-translation");
        let _scratch_reservation =
            crate::playback_cache::reserve(&root, scratch_allowance(engine, 0))
                .await
                .map_err(|_| failure("output_invalid"))?;
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
            audio_start_samples: clock.audio_start_samples,
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
            recipe.profile.task,
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
        () = crate::subtitle_transcription::watch_speech_capacity(&state.transcode, admission) => {
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
        if !selected::registered(profile) {
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
        let _scratch_reservation = crate::playback_cache::reserve(
            &root,
            scratch_allowance(engine, selected::SELF_TEST_FIXTURE.len() as u64),
        )
        .await
        .map_err(|_| failure("output_invalid"))?;
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
            audio_start_samples: 0,
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
            profile.task,
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
        if !(match profile.task {
            SpeechTask::TranslateToEnglish => selected::english_self_test(&cues),
            SpeechTask::Transcribe => selected::transcription_self_test(&cues),
        }) {
            return Err(failure("model_self_test_failed"));
        }
        Ok(())
    });
    tokio::pin!(operation);
    tokio::select! {
        result = &mut operation => result,
        () = crate::subtitle_transcription::watch_speech_capacity(&state.transcode, admission) => {
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

    #[test]
    fn inference_budget_counts_pinned_acoustic_blocks_within_registered_window_bounds() {
        let window = |span: i64| Window {
            purpose: Purpose::Range,
            core_begin_ms: 0,
            core_end_ms: span.min(30_000),
            begin_ms: 0,
            end_ms: span,
            audio_index: 2,
            audio_start_samples: 0,
        };
        for (span, seconds) in [
            (12_960, 120),
            (30_000, 120),
            (30_001, 240),
            (35_000, 240),
            (40_000, 240),
        ] {
            assert_eq!(
                inference_budget(window(span)).expect("qualified span"),
                Duration::from_secs(seconds)
            );
        }
        let mut self_test = window(12_960);
        self_test.purpose = Purpose::TranslationSelfTest;
        assert_eq!(
            inference_budget(self_test).expect("self-test budget"),
            Duration::from_secs(120)
        );
        for span in [0, -1, 40_001, i64::MAX] {
            assert!(inference_budget(window(span)).is_err());
        }
        let mut overflowing = window(1);
        overflowing.begin_ms = i64::MIN;
        overflowing.end_ms = i64::MAX;
        assert!(inference_budget(overflowing).is_err());
        let mut invalid_core = window(30_000);
        invalid_core.core_end_ms = 30_001;
        assert!(inference_budget(invalid_core).is_err());
    }

    fn frame(filter: u8, pts: i64, samples: i64) -> String {
        format!(
            "[Parsed_ashowinfo_{filter} @0] n:0 pts:{pts} pts_time:0 fmt:s16 channels:1 rate:16000 nb_samples:{samples}\n"
        )
    }

    #[test]
    fn observed_clock_ignores_side_data_but_rejects_malformed_frame_records() {
        // E-AC-3 surround emits these continuation records for each frame.
        // Values come from a real deep-seek decoder observation, without media
        // paths or content. They retain the exact 48k-to-16k source clock.
        let log = "[Parsed_ashowinfo_1 @0] n:0 pts:5648000 pts_time:353 fmt:fltp channels:6 rate:48000 nb_samples:1152\n\
                   [Parsed_ashowinfo_1 @0] side data - matrix encoding: none\n\
                   [Parsed_ashowinfo_1 @0] side data - downmix: preferred type: Lt/Rt\n\
                   [Parsed_ashowinfo_1 @0] n:1 pts:5648384 pts_time:353.024 fmt:fltp channels:6 rate:48000 nb_samples:1536\n\
                   [Parsed_ashowinfo_4 @0] n:0 pts:5680000 pts_time:355 fmt:s16 channels:1 rate:16000 nb_samples:111\n\
                   [Parsed_ashowinfo_4 @0] side data - matrix encoding: none\n\
                   [Parsed_ashowinfo_4 @0] n:1 pts:5680111 pts_time:355.007 fmt:s16 channels:1 rate:16000 nb_samples:529\n";
        let clock = observe_clock(log.as_bytes(), 5_680_000, 5_680_640)
            .expect("side data is not malformed clock evidence");
        assert_eq!(
            (clock.raw_first_sample, clock.anchor_sample, clock.samples),
            (5_648_000, 5_680_000, 640)
        );
        let malformed = log.replace("pts:5680111 ", "missing_pts ");
        assert!(observe_clock(malformed.as_bytes(), 5_680_000, 5_680_640).is_err());
        assert!(clock_frame_record(
            b"[Parsed_ashowinfo_4 @0] n:1 missing_pts nb_samples:529"
        ));
        assert!(!clock_frame_record(
            b"[Parsed_ashowinfo_4 @0] side data - matrix encoding: none"
        ));
    }

    #[test]
    fn observed_clock_allows_only_verified_leading_audio_delay_and_negative_preroll() {
        let delayed = frame(1, 8_000, 8_000) + &frame(4, 0, 16_000);
        assert!(observe_source_clock(delayed.as_bytes(), 0, 16_000, 8_000).is_ok());
        assert!(observe_source_clock(delayed.as_bytes(), 0, 16_000, 7_000).is_err());
        let negative = frame(1, -1_000, 17_000) + &frame(4, 0, 16_000);
        assert!(observe_source_clock(negative.as_bytes(), 0, 16_000, -1_000).is_ok());
        let seek_overshoot = frame(1, 18_000, 1_000) + &frame(4, 16_000, 3_000);
        assert!(observe_source_clock(seek_overshoot.as_bytes(), 16_000, 19_000, 8_000).is_err());
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
        .expect("test setup")
        .cues;
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
        let cues = owned_cues(vtt, &asset, 0, 0, 4 * RATE, 0, 4_000)
            .expect("test setup")
            .cues;
        assert_eq!(cues.len(), 2);
        assert!(cues.iter().all(|cue| cue.text == "repeat"));
        let decreasing = vtt.replace("00:00:02.000 -->", "00:00:00.000 -->").replace(
            "00:00:00.000 --> 00:00:03.000",
            "00:00:01.000 --> 00:00:03.000",
        );
        assert!(owned_cues(&decreasing, &asset, 0, 0, 4 * RATE, 0, 4_000).is_err());
    }

    #[test]
    fn context_attributed_speech_leaves_core_empty_but_silent_recognizer_still_fails() {
        let asset = "ab".repeat(32);
        // The last short core of a 61.14 s source: decoded window 55.0-61.14 s,
        // core 60.0-61.14 s. The recognizer dated the closing sentence 0-3.48 s
        // into the window, inside the left context the previous core owns.
        let tail = "WEBVTT\n\n00:00:00.000 --> 00:00:03.480\n normally in combination with practical experience\n\n";
        let owned = owned_cues(tail, &asset, 2, 55 * RATE, 98_240, 60_000, 61_140)
            .expect("valid recognizer output");
        assert_eq!((owned.cues.len(), owned.recognized), (0, 1));
        assert!(signal_core_cues(owned)
            .expect("context-owned speech")
            .is_empty());
        let silent = owned_cues("WEBVTT\n\n", &asset, 2, 55 * RATE, 98_240, 60_000, 61_140)
            .expect("well-formed empty output");
        assert_eq!(silent.recognized, 0);
        assert!(signal_core_cues(silent).is_err());
        // A cue dated past the decoded audio is no neighbour's speech either.
        let past = "WEBVTT\n\n00:00:07.000 --> 00:00:08.000\n invented\n\n";
        let owned = owned_cues(past, &asset, 2, 55 * RATE, 98_240, 60_000, 61_140)
            .expect("valid recognizer output");
        assert_eq!(owned.recognized, 0);
        assert!(signal_core_cues(owned).is_err());
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

    #[test]
    fn resumed_source_hash_rejects_changed_content_and_cancellation_between_chunks() {
        let mut source = tempfile::tempfile().expect("source");
        source
            .write_all(&vec![1_u8; 128 * 1024 + 1])
            .expect("bytes");
        let identity = std_file_identity(&source).expect("identity");
        let mut hash = SourceHash::new(source.try_clone().expect("held"), identity).expect("hash");
        let cancel = CancellationToken::new();
        assert!(hash.step(&cancel).expect("prefix").is_none());
        cancel.cancel();
        assert!(
            hash.step(&cancel).is_err(),
            "a paused cancelled read must not publish"
        );
        source.seek(SeekFrom::Start(0)).expect("rewind");
        source
            .write_all(&[2])
            .expect("replace byte without changing length");
        assert!(
            SourceHash::new(source, identity).is_err(),
            "same-sized mutation changes held identity"
        );
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
    async fn actual_multichannel_decoder_keeps_clock_frames_past_side_data_log_budget() {
        let root = tempfile::tempdir().expect("owned surround fixture");
        let root = root.path().canonicalize().expect("canonical fixture root");
        let source_path = root.join("surround.mkv");
        let decoder = test_decoder();
        let mut generate = tokio::process::Command::new(&decoder);
        generate
            .args([
                "-nostdin",
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=44",
                "-ac",
                "6",
                "-channel_layout",
                "5.1",
                "-c:a",
                "eac3",
                "-b:a",
                "640k",
                "-threads",
                "1",
                "-f",
                "matroska",
            ])
            .arg(&source_path);
        child_output(&mut generate, Duration::from_secs(20), 8192, false)
            .await
            .expect("actual E-AC-3 fixture encoder");
        let source = plurx_core::fs_secure::open_read_nofollow(&source_path)
            .await
            .expect("held surround fixture")
            .into_std()
            .await;
        let scratch = root.join("decode");
        std::fs::create_dir(&scratch).expect("owned decode directory");
        let decoded = decode_window(
            &source,
            Window {
                purpose: Purpose::Range,
                begin_ms: 2_000,
                end_ms: 42_000,
                core_begin_ms: 7_000,
                core_end_ms: 37_000,
                audio_index: 0,
                audio_start_samples: 0,
            },
            &scratch,
            &decoder,
            &CancellationToken::new(),
        )
        .await
        .expect("full surround window retains observed timestamps despite side data");
        assert_eq!(decoded.clock.anchor_sample, 32_000);
        assert_eq!(decoded.clock.samples, 640_000);
        assert!(decoded.has_signal);
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
                audio_start_samples: 0,
            };
            // The held-capability opener rejects symlinked ancestors such as
            // macOS /var. Match the canonical runtime root used in production.
            let scratch_root = scratch.path().canonicalize().expect("canonical scratch");
            let pcm = decode_window(&source, window, &scratch_root, &decoder, &cancel)
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

    #[cfg(target_os = "linux")]
    const OWNED_GROUP_TEST: &str = "subtitle_translation::runtime::tests::actual_timeout_and_cancellation_reap_owned_child_group_before_return";

    fn announced_children(path: &Path) -> Option<(i32, i32)> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut fields = text.split_whitespace();
        let leader = fields.next()?.parse::<i32>().ok()?;
        let descendant = fields.next()?.parse::<i32>().ok()?;
        (fields.next().is_none() && leader > 1 && descendant > 1 && leader != descendant)
            .then_some((leader, descendant))
    }

    #[cfg(target_os = "linux")]
    fn linux_process_identity(pid: i32) -> Option<(i32, i32, String)> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let fields: Vec<_> = text
            .get(text.rfind(')')? + 2..)?
            .split_whitespace()
            .collect();
        Some((
            fields.get(1)?.parse().ok()?,
            fields.get(2)?.parse().ok()?,
            fields.first()?.to_string(),
        ))
    }

    #[cfg(target_os = "linux")]
    fn cleanup_announced_groups(root: &Path, owner: i32) {
        for mode in ["timeout", "cancel"] {
            let Some((leader, descendant)) = announced_children(&root.join(format!("{mode}.pid")))
            else {
                continue;
            };
            let leader_owned = linux_process_identity(leader)
                .is_some_and(|(parent, group, _)| parent == owner && group == leader);
            let descendant_owned =
                linux_process_identity(descendant).is_some_and(|(parent, group, _)| {
                    group == leader && (parent == owner || (parent == leader && leader_owned))
                });
            if leader_owned || descendant_owned {
                // A fresh private announcement plus a live PPID/PGID match binds this signal.
                unsafe {
                    libc::kill(-leader, libc::SIGKILL);
                }
            }
            if owner == std::process::id() as i32 {
                // Only this isolated subreaper can adopt this exact announced descendant.
                let until = std::time::Instant::now() + Duration::from_secs(2);
                loop {
                    let result =
                        unsafe { libc::waitpid(descendant, std::ptr::null_mut(), libc::WNOHANG) };
                    if result != 0 || std::time::Instant::now() >= until {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn validate_reaper_guard(
        root: &Path,
        nonce: &str,
        parent: u32,
        args: &[String],
    ) -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let refuse = || {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid isolated reaper protocol",
            )
        };
        if nonce.len() != 32
            || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !args.iter().map(String::as_str).eq([
                "--exact",
                OWNED_GROUP_TEST,
                "--nocapture",
                "--test-threads=1",
            ])
            || root.canonicalize()? != root
        {
            return Err(refuse());
        }
        let directory = std::fs::symlink_metadata(root)?;
        let guard = std::fs::symlink_metadata(root.join("guard"))?;
        if !directory.is_dir()
            || directory.mode() & 0o7777 != 0o700
            || !guard.is_file()
            || guard.mode() & 0o7777 != 0o600
            || guard.nlink() != 1
            || directory.uid() != unsafe { libc::geteuid() }
            || guard.uid() != directory.uid()
            || std::fs::read_to_string(root.join("guard"))?
                != format!("{nonce}\n{parent}\n{OWNED_GROUP_TEST}\n")
        {
            return Err(refuse());
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn claim_reaper_guard(
        root: &Path,
        nonce: &str,
        parent: u32,
        args: &[String],
    ) -> std::io::Result<std::fs::File> {
        use std::os::unix::fs::OpenOptionsExt;
        validate_reaper_guard(root, nonce, parent, args)?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("claim"))
    }

    #[cfg(target_os = "linux")]
    #[allow(clippy::disallowed_methods)] // Re-exec isolates test-only process-wide subreaper state.
    fn isolated_reaper_process() -> Option<std::path::PathBuf> {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        const NONCE: &str = "PLURX_TEST_SUBTITLE_REAPER_NONCE";
        const ROOT: &str = "PLURX_TEST_SUBTITLE_REAPER_ROOT";
        match (std::env::var(NONCE), std::env::var_os(ROOT)) {
            (Ok(nonce), Some(root)) => {
                let root = std::path::PathBuf::from(root);
                let args: Vec<_> = std::env::args().skip(1).collect();
                let mut claim =
                    claim_reaper_guard(&root, &nonce, unsafe { libc::getppid() } as u32, &args)
                        .expect("isolated reaper protocol must be fresh, exact and exclusive");
                write!(claim, "{nonce}\n{}\n", std::process::id()).expect("write isolated claim");
                // The exact filter isolates this test body; its Tokio runtime is not created yet.
                assert_eq!(
                    unsafe {
                        libc::prctl(
                            libc::PR_SET_CHILD_SUBREAPER,
                            1 as libc::c_ulong,
                            0 as libc::c_ulong,
                            0 as libc::c_ulong,
                            0 as libc::c_ulong,
                        )
                    },
                    0,
                    "subreaper setup failed: {}",
                    std::io::Error::last_os_error()
                );
                Some(root)
            }
            (Err(std::env::VarError::NotPresent), None) => {
                let scratch = tempfile::tempdir().expect("isolated reaper directory");
                std::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o700))
                    .expect("secure fresh empty reaper directory");
                let root = scratch
                    .path()
                    .canonicalize()
                    .expect("canonical isolated directory");
                let nonce = uuid::Uuid::new_v4().simple().to_string();
                let mut guard = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(root.join("guard"))
                    .expect("fresh isolated guard");
                write!(
                    guard,
                    "{nonce}\n{}\n{OWNED_GROUP_TEST}\n",
                    std::process::id()
                )
                .expect("write guard");
                let log = std::fs::OpenOptions::new()
                    .read(true)
                    .append(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(root.join("output"))
                    .expect("private child capture");
                let mut child =
                    std::process::Command::new(std::env::current_exe().expect("test executable"))
                        .args([
                            "--exact",
                            OWNED_GROUP_TEST,
                            "--nocapture",
                            "--test-threads=1",
                        ])
                        .env(NONCE, &nonce)
                        .env(ROOT, &root)
                        .stdin(std::process::Stdio::null())
                        .stdout(log.try_clone().expect("capture clone"))
                        .stderr(log)
                        .spawn()
                        .expect("spawn isolated reaper test");
                let until = std::time::Instant::now() + Duration::from_secs(30);
                let mut refusal = None;
                let status = loop {
                    let bytes = std::fs::metadata(root.join("output"))
                        .expect("capture size")
                        .len();
                    if bytes > 8 * 1024 * 1024 || std::time::Instant::now() >= until {
                        refusal = Some(if bytes > 8 * 1024 * 1024 {
                            "capture exceeds 8 MiB"
                        } else {
                            "30 s isolated protocol deadline"
                        });
                        cleanup_announced_groups(&root, child.id() as i32);
                        if child.try_wait().expect("observe refused child").is_none() {
                            if let Err(error) = child.kill() {
                                assert!(
                                    child.try_wait().expect("observe kill race").is_some(),
                                    "isolated child kill failed: {error}"
                                );
                            }
                            let reap_until = std::time::Instant::now() + Duration::from_secs(2);
                            while child.try_wait().expect("reap refused child").is_none() {
                                assert!(
                                    std::time::Instant::now() < reap_until,
                                    "refused isolated child PID={} did not reap after SIGKILL; identity={:?}",
                                    child.id(),
                                    linux_process_identity(child.id() as i32)
                                );
                                std::thread::sleep(Duration::from_millis(10));
                            }
                        }
                        break None;
                    }
                    if let Some(status) = child.try_wait().expect("observe isolated child") {
                        break Some(status);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                };
                let mut output =
                    std::fs::File::open(root.join("output")).expect("open child diagnostics");
                let begin = output
                    .metadata()
                    .expect("capture metadata")
                    .len()
                    .saturating_sub(16 * 1024);
                output
                    .seek(SeekFrom::Start(begin))
                    .expect("seek diagnostics tail");
                let mut tail = Vec::new();
                output
                    .take(16 * 1024)
                    .read_to_end(&mut tail)
                    .expect("read bounded diagnostics tail");
                let tail = String::from_utf8_lossy(&tail);
                let expected = format!("{nonce}\n{}\n", child.id());
                assert!(
                    status.is_some_and(|status| status.success())
                        && std::fs::read(root.join("claim")).ok().as_deref()
                            == Some(expected.as_bytes())
                        && std::fs::read(root.join("done")).ok().as_deref()
                            == Some(expected.as_bytes()),
                    "isolated owned-group test failed: status={status:?}, refusal={refusal:?}\n{tail}"
                );
                None
            }
            _ => panic!("partial isolated reaper environment is forbidden"),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn isolated_reaper_guard_refuses_wrong_filter_nonce_parent_and_replay() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let scratch = tempfile::tempdir().expect("test setup");
        std::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o700))
            .expect("secure fresh empty reaper directory");
        let root = scratch.path().canonicalize().expect("test setup");
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let parent = std::process::id();
        let mut guard = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("guard"))
            .expect("test setup");
        write!(guard, "{nonce}\n{parent}\n{OWNED_GROUP_TEST}\n").expect("test setup");
        let args: Vec<String> = [
            "--exact",
            OWNED_GROUP_TEST,
            "--nocapture",
            "--test-threads=1",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        validate_reaper_guard(&root, &nonce, parent, &args).expect("valid exact child protocol");
        let mut wrong_filter = args.clone();
        wrong_filter[1] = "another_test".into();
        assert!(claim_reaper_guard(&root, &nonce, parent, &wrong_filter).is_err());
        let mut wrong_nonce = nonce.clone().into_bytes();
        wrong_nonce[0] = if wrong_nonce[0] == b'0' { b'1' } else { b'0' };
        let wrong_nonce = String::from_utf8(wrong_nonce).expect("hex nonce");
        assert!(claim_reaper_guard(&root, &wrong_nonce, parent, &args).is_err());
        assert!(claim_reaper_guard(&root, &nonce, parent + 1, &args).is_err());
        assert!(
            !root.join("claim").exists(),
            "validation must not mutate or activate a subreaper"
        );
        let claim = claim_reaper_guard(&root, &nonce, parent, &args).expect("first exact claim");
        assert_eq!(
            claim_reaper_guard(&root, &nonce, parent, &args)
                .expect_err("replay must fail")
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        drop(claim);
    }

    #[test]
    fn actual_timeout_and_cancellation_reap_owned_child_group_before_return() {
        #[cfg(target_os = "linux")]
        let root = match isolated_reaper_process() {
            Some(root) => root,
            None => return,
        };
        #[cfg(not(target_os = "linux"))]
        let scratch = tempfile::tempdir().expect("test setup");
        #[cfg(not(target_os = "linux"))]
        let root = scratch.path().to_path_buf();
        let result = std::panic::catch_unwind(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test runtime")
                .block_on(check_owned_child_groups(&root));
        });
        #[cfg(target_os = "linux")]
        if result.is_err() {
            cleanup_announced_groups(&root, std::process::id() as i32);
        }
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let claim = std::fs::read(root.join("claim")).expect("read isolated claim");
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(root.join("done"))
                .expect("fresh completion receipt")
                .write_all(&claim)
                .expect("record successful isolated checks");
        }
    }

    async fn check_owned_child_groups(root: &Path) {
        for cancelled in [false, true] {
            let mode = if cancelled { "cancel" } else { "timeout" };
            let pid_path = root.join(format!("{mode}.pid"));
            let mut command = tokio::process::Command::new("/bin/sh");
            command
                .args([
                    "-c",
                    "/bin/sleep 60 & child=$!; printf '%s %s\\n' \"$$\" \"$child\" > \"$1\"; wait",
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
                    result = &mut operation => panic!("mode={mode}: child exited before cancellation: {result:?}"),
                    result = tokio::time::timeout(Duration::from_secs(2), async {
                        while announced_children(&pid_path).is_none() { tokio::time::sleep(Duration::from_millis(10)).await; }
                    }) => { result.expect("owned child pair must be observed before cancellation"); cancel.cancel(); },
                }
            }
            let error = operation.await.expect_err("owned operation must fail");
            let expected = if cancelled {
                "worker_interrupted"
            } else {
                "decode_failed"
            };
            assert!(
                matches!(&error, StoreError::Task(reason) if reason == expected),
                "mode={mode}: expected mapped errorKind={expected}, actual={error:?}"
            );
            let (leader, descendant) =
                announced_children(&pid_path).expect("complete owned PID pair");
            let until = tokio::time::Instant::now() + Duration::from_secs(3);
            let mut empty = 0;
            #[cfg(target_os = "linux")]
            let mut descendant_reaped = false;
            loop {
                #[cfg(target_os = "linux")]
                if !descendant_reaped {
                    let mut status = 0;
                    let waited = unsafe { libc::waitpid(descendant, &mut status, libc::WNOHANG) };
                    let wait_error = (waited < 0).then(std::io::Error::last_os_error);
                    assert!(
                        waited >= 0,
                        "mode={mode} PID={descendant} identity={:?} errorKind={:?}: {}",
                        linux_process_identity(descendant),
                        wait_error.as_ref().map(std::io::Error::kind),
                        wait_error
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default()
                    );
                    if waited == descendant {
                        assert!(
                            libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGKILL,
                            "mode={mode} PID={descendant}: descendant was not killed by the owned group operation, status={status}"
                        );
                        descendant_reaped = true;
                    }
                }
                let mut ps = tokio::process::Command::new("/bin/ps");
                ps.args(["-e", "-o", "pid=,ppid=,pgid=,stat="]);
                let output = child_output(&mut ps, Duration::from_secs(2), 1024 * 1024, false)
                    .await
                    .expect("process snapshot");
                assert!(output.status.success());
                let text = String::from_utf8(output.stdout).expect("process snapshot text");
                let members: Vec<_> = text
                    .lines()
                    .filter(|line| {
                        line.split_whitespace()
                            .nth(2)
                            .and_then(|group| group.parse::<i32>().ok())
                            == Some(leader)
                    })
                    .collect();
                empty = if members.is_empty() { empty + 1 } else { 0 };
                if empty >= 2 {
                    #[cfg(target_os = "linux")]
                    assert!(
                        descendant_reaped,
                        "mode={mode} PID={descendant}: exact adopted descendant was not reaped"
                    );
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < until,
                    "mode={mode} mapped errorKind={expected} leader={leader} descendant={descendant}; owned group outlived return; PID PPID PGID STAT={members:?}"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}
