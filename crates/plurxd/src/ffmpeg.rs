//! The ffmpeg binaries and what this particular build can do.
//!
//! Every delivery path shells out to the same two binaries, and two of them
//! (the progressive remux and the HLS sessions) need the same answer to the
//! same question: does this build understand the input-pacing flags? Probing
//! it in one place, once per process, is what keeps the answer consistent —
//! and keeps a stream from failing to start because one path guessed.

#[cfg(target_os = "linux")]
pub(crate) mod linux_dolby;

use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use plurx_core::domain::MediaFile;
use plurx_core::transcode::{
    output_size, EffectiveRateControl, Encoder, OutputGrade, Pacing, Pipeline,
};

/// Resolve a held Windows source handle to the path handed to stock ffmpeg.
/// The caller retains the handle until the child has opened the path.
#[cfg(windows)]
pub(crate) fn windows_source_path(source: &std::fs::File) -> Result<std::path::PathBuf, String> {
    plurx_core::fs_secure::std_file_path(source)
        .map_err(|error| format!("resolving held Windows source path: {error}"))
}

/// Close D2's pathname handoff as far as stock ffmpeg permits: immediately
/// before spawn, reopen without following reparse points and require the
/// complete FileIdInfo identity to match the handle retained by plurxd.
#[cfg(windows)]
pub(crate) fn verify_windows_source_path(
    source: &std::fs::File,
    path: &std::path::Path,
) -> Result<(), String> {
    let held = plurx_core::fs_secure::std_file_identity(source)
        .map_err(|error| format!("stating held Windows source: {error}"))?;
    let reopened = plurx_core::fs_secure::open_read_nofollow_blocking(path)
        .map_err(|error| format!("reopening Windows source without reparse traversal: {error}"))?;
    let current = plurx_core::fs_secure::std_file_identity(&reopened)
        .map_err(|error| format!("stating reopened Windows source: {error}"))?;
    if held != current {
        return Err("Windows source changed before ffmpeg spawn".to_owned());
    }
    Ok(())
}

/// An override wins only when it names something. An empty `PLURX_FFMPEG=` is
/// what a Compose file produces for an unset variable, and treating that as a
/// binary called "" would fail every spawn with a confusing ENOENT.
fn resolve_bin(override_value: Option<String>, fallback: &str) -> String {
    plurx_core::process::resolve_media_tool(override_value, fallback, None)
}

/// ffmpeg binary, overridable via `PLURX_FFMPEG` (jellyfin-ffmpeg / pinned path).
pub fn ffmpeg_bin() -> String {
    plurx_core::process::media_tool_bin("ffmpeg", std::env::var("PLURX_FFMPEG").ok())
}

/// ffprobe binary, overridable via `PLURX_FFPROBE` (jellyfin-ffmpeg / pinned).
pub fn ffprobe_bin() -> String {
    plurx_core::process::media_tool_bin("ffprobe", std::env::var("PLURX_FFPROBE").ok())
}

/// Dedicated self-contained parser for descriptor-bound local source facts.
/// General scanning keeps its configured FFprobe and codec/network behavior.
pub fn bound_ffprobe_bin() -> String {
    resolve_bin(std::env::var("PLURX_BOUND_FFPROBE").ok(), &ffprobe_bin())
}

/// Pass held source/sidecar capabilities into reserved child FDs. Duplicate
/// every original before assigning any target: an original may itself be
/// fd 3 or fd 5. The post-fork closure performs only descriptor syscalls.
pub(crate) fn inherit_file_descriptors(
    command: &mut tokio::process::Command,
    files: &[(&std::fs::File, i32)],
) {
    inherit_file_descriptors_with_cwd(command, files, None);
}

/// Install inherited descriptors and optionally make one directory descriptor
/// the child's working directory. Keeping both operations in this one
/// post-fork hook preserves the dup-all-before-dup2 ordering when a held file
/// happens to occupy one of the reserved target numbers.
pub(crate) fn inherit_file_descriptors_with_cwd(
    command: &mut tokio::process::Command,
    files: &[(&std::fs::File, i32)],
    cwd_target: Option<i32>,
) {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let descriptors = files
            .iter()
            .map(|(file, target)| (file.as_raw_fd(), *target))
            .collect::<Vec<_>>();
        inherit_raw_file_descriptors_with_cwd(command, &descriptors, cwd_target);
    }
    #[cfg(not(unix))]
    let _ = (command, files, cwd_target);
}

/// Raw-descriptor form used by the producer spawn builder after callers have
/// reduced held capabilities to the only values safe to capture in `pre_exec`.
#[cfg(unix)]
pub(crate) fn inherit_raw_file_descriptors_with_cwd(
    command: &mut tokio::process::Command,
    descriptors: &[(std::os::fd::RawFd, i32)],
    cwd_target: Option<i32>,
) {
    assert!(
        descriptors.len() <= 7
            && descriptors
                .iter()
                .all(|(_, target)| (3..=9).contains(target))
    );
    assert!(cwd_target.is_none_or(|target| descriptors.iter().any(|(_, fd)| *fd == target)));
    let descriptors = descriptors.to_vec();
    unsafe {
        command.pre_exec(move || {
            let mut copies = [None; 7];
            for (index, (fd, target)) in descriptors.iter().enumerate() {
                let duplicate = libc::fcntl(*fd, libc::F_DUPFD_CLOEXEC, 10);
                if duplicate == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                copies[index] = Some((duplicate, *target));
            }
            for (duplicate, target) in copies.into_iter().flatten() {
                if libc::dup2(duplicate, target) == -1 {
                    libc::close(duplicate);
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(duplicate);
            }
            #[cfg(target_os = "macos")]
            if let Some(target) = cwd_target {
                if libc::fchdir(target) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

/// Drain diagnostics alongside the media pipe without letting a noisy
/// encoder block on stderr or allocate an unbounded line/string. Keep the
/// last 8 KiB, where encoder/filter failures normally explain their exit.
pub(crate) async fn drain_diagnostics(mut input: impl AsyncRead + Unpin) -> String {
    const LIMIT: usize = 8 * 1024;
    let mut tail = Vec::with_capacity(LIMIT);
    let mut chunk = [0u8; 2048];
    while let Ok(read) = input.read(&mut chunk).await {
        if read == 0 {
            break;
        }
        let discard = (tail.len() + read).saturating_sub(LIMIT);
        tail.drain(..discard);
        tail.extend_from_slice(&chunk[..read]);
    }
    String::from_utf8_lossy(&tail).into_owned()
}

/// A producer's drained stderr tail, split into the lines an operator should
/// see and the informational lines a library prints on every start.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ProducerDiagnostic {
    /// Everything not known to be informational, in order. A warning's worth.
    pub(crate) actionable: String,
    /// Known informational lines, in order. Debug detail only.
    pub(crate) informational: String,
}

/// Line prefixes that are never a producer diagnostic. libva writes its own
/// `libva info:` lines (the VA-API version, the driver it tries to open, the
/// init function it found, `va_openDriver() returns 0`) straight to stderr,
/// whatever FFmpeg's `-loglevel` says, once for every VA-API or QSV device a
/// child opens; a healthy QSV generation therefore always had a "diagnostic".
/// `libva error:` is not on this list and stays actionable.
const INFORMATIONAL_DIAGNOSTIC_PREFIXES: &[&str] = &["libva info:"];

/// Whether one stderr line is informational output rather than a diagnostic:
/// it starts, after leading whitespace, with one of
/// [`INFORMATIONAL_DIAGNOSTIC_PREFIXES`]. The one rule every ffmpeg stderr
/// consumer uses, line by line ([`classify_diagnostic`], the rolling
/// transcode's stderr log, the Live TV readiness error).
pub(crate) fn is_informational_diagnostic(line: &str) -> bool {
    let trimmed = line.trim_start();
    INFORMATIONAL_DIAGNOSTIC_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// Classify a [`drain_diagnostics`] tail line by line. Blank lines are
/// dropped; a line is informational only when it starts with one of
/// [`INFORMATIONAL_DIAGNOSTIC_PREFIXES`], so anything unrecognised stays
/// actionable.
pub(crate) fn classify_diagnostic(tail: &str) -> ProducerDiagnostic {
    let mut actionable = Vec::new();
    let mut informational = Vec::new();
    for line in tail.lines() {
        let line = line.trim_end_matches('\r');
        let trimmed = line.trim_start();
        if trimmed.trim_end().is_empty() {
            continue;
        }
        if is_informational_diagnostic(trimmed) {
            informational.push(line);
        } else {
            actionable.push(line);
        }
    }
    ProducerDiagnostic {
        actionable: actionable.join("\n"),
        informational: informational.join("\n"),
    }
}

/// A file-producing child with no captured stdout and one bounded stderr
/// reader. Cancellation transfers the exact child to a reap owner, never a
/// detached diagnostic reader. Used by whole-track burn extraction.
pub(crate) struct BoundedDiagnosticChild {
    child: Option<tokio::process::Child>,
    child_job: Option<crate::process_control::ChildJob>,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    #[cfg(test)]
    reaped: Option<tokio::sync::oneshot::Sender<()>>,
}

impl BoundedDiagnosticChild {
    pub fn spawn(
        command: &mut tokio::process::Command,
        work: crate::process_control::ChildWork,
    ) -> std::io::Result<Self> {
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let (mut child, child_job) = crate::process_control::spawn_job_owned(command, work)?;
        let stderr = child.stderr.take();
        Ok(Self {
            child: Some(child),
            child_job: Some(child_job),
            stdout: None,
            stderr,
            #[cfg(test)]
            reaped: None,
        })
    }

    /// Spawn a child whose media output is owned by the daemon. The caller
    /// must consume it with [`Self::output_to_bounded_file`]; no subprocess
    /// ever receives a cache pathname it can grow past the enforced bound.
    pub fn spawn_piped_output(
        command: &mut tokio::process::Command,
        work: crate::process_control::ChildWork,
    ) -> std::io::Result<Self> {
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let (mut child, child_job) = crate::process_control::spawn_job_owned(command, work)?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        Ok(Self {
            child: Some(child),
            child_job: Some(child_job),
            stdout,
            stderr,
            #[cfg(test)]
            reaped: None,
        })
    }

    pub async fn output(self) -> std::io::Result<(std::process::ExitStatus, String)> {
        self.output_cancellable(&tokio_util::sync::CancellationToken::new())
            .await
    }

    /// Cooperative cancellation joins the child before releasing its caller's
    /// resource permits. Dropping the entire task still retains the reap owner.
    pub async fn output_cancellable(
        mut self,
        cancelled: &tokio_util::sync::CancellationToken,
    ) -> std::io::Result<(std::process::ExitStatus, String)> {
        let stderr = self
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("extractor stderr was not piped"))?;
        let child = self.child.as_mut().expect("owned extraction child");
        let wait = async {
            tokio::select! {
                biased;
                _ = cancelled.cancelled() => {
                    let _ = child.start_kill();
                    child.wait().await
                }
                status = child.wait() => status,
            }
        };
        let (status, diagnostics) = tokio::join!(wait, drain_diagnostics(stderr));
        let status = status?;
        self.child.take(); // Successful wait, including nonzero exit, proves reap.
        self.child_job.take();
        #[cfg(test)]
        if let Some(reaped) = self.reaped.take() {
            let _ = reaped.send(());
        }
        Ok((status, diagnostics))
    }

    /// Copy stdout into a newly-created file and stop the exact producer as
    /// soon as it attempts to exceed `max_bytes`. At most `max_bytes` reach
    /// disk; stderr is drained concurrently and the child is reaped before an
    /// error is returned.
    pub async fn output_to_bounded_file(
        self,
        path: &std::path::Path,
        max_bytes: u64,
    ) -> std::io::Result<(std::process::ExitStatus, String)> {
        self.output_to_bounded_file_cancellable(
            path,
            max_bytes,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
    }

    pub async fn output_to_bounded_file_cancellable(
        mut self,
        path: &std::path::Path,
        max_bytes: u64,
        cancelled: &tokio_util::sync::CancellationToken,
    ) -> std::io::Result<(std::process::ExitStatus, String)> {
        let mut stdout = self
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("extractor stdout was not piped"))?;
        let stderr = self
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("extractor stderr was not piped"))?;
        let child = self.child.as_mut().expect("owned extraction child");
        let copy = async {
            let result = async {
                let mut output = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .await?;
                let mut total = 0_u64;
                // This buffer lives across awaits. Inline storage propagates
                // through extractor/timeout/select future states; their debug
                // poll frames exceeded the worker stack during window warming.
                // Keep the same bounded chunk owned by the IO future on heap.
                let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
                let exceeded = loop {
                    let read = tokio::select! {
                        biased;
                        _ = cancelled.cancelled() => return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "analysis cancelled")),
                        result = stdout.read(&mut buffer) => result?,
                    };
                    if read == 0 {
                        break false;
                    }
                    let remaining = max_bytes.saturating_sub(total);
                    let accepted = usize::try_from(remaining.min(read as u64)).unwrap_or(read);
                    if accepted > 0 {
                        output.write_all(&buffer[..accepted]).await?;
                        total += accepted as u64;
                    }
                    if accepted < read {
                        break true;
                    }
                };
                output.flush().await?;
                Ok::<_, std::io::Error>(exceeded)
            }
            .await;

            // A failed daemon write is just as terminal as an exceeded cap.
            // Stop the exact writer before joining its diagnostic pipe, or an
            // encoder blocked on stdout could keep this error path alive.
            if !matches!(&result, Ok(false)) {
                let _ = child.start_kill();
            }
            result
        };
        let (copy_result, diagnostics) = tokio::join!(copy, drain_diagnostics(stderr));
        let status = self
            .child
            .as_mut()
            .expect("owned extraction child")
            .wait()
            .await?;
        self.child.take();
        self.child_job.take();
        #[cfg(test)]
        if let Some(reaped) = self.reaped.take() {
            let _ = reaped.send(());
        }
        let exceeded = copy_result?;
        if exceeded {
            return Err(std::io::Error::other(format!(
                "extractor output exceeded its disk bound of {max_bytes} bytes"
            )));
        }
        Ok((status, diagnostics))
    }
}

impl Drop for BoundedDiagnosticChild {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let child_job = self.child_job.take();
        let _ = child.start_kill();
        #[cfg(test)]
        let reaped = self.reaped.take();
        tokio::spawn(async move {
            let _child_job = child_job;
            let result = child.wait().await;
            if let Err(ref error) = result {
                tracing::warn!(%error, "reaping cancelled burn-track extraction failed");
            }
            #[cfg(test)]
            if result.is_ok() {
                if let Some(reaped) = reaped {
                    let _ = reaped.send(());
                }
            }
        });
    }
}

#[derive(Clone, Debug)]
pub(crate) struct EncodedExecutable {
    pub path: std::path::PathBuf,
    pub digest: String,
    object_version: String,
    linux_binding: Option<plurx_core::transcode::LinuxDolbyExecutionBinding>,
}

#[derive(Default)]
struct ExecutableCaptureCache {
    executables: Vec<EncodedExecutable>,
    #[cfg(test)]
    hashes: u64,
}

/// Retained attestations, one per media tool path. A VOD start captures the
/// held-source reporter and then the encoder; with a single slot each capture
/// evicted the other, so every start re-hashed both binaries under this lock.
const EXECUTABLE_CAPTURE_SLOTS: usize = 8;

// Concurrent captures of one tool share its hash; every reuse still checks the
// exact object identity that the production launch and publication fences
// require.
static EXECUTABLE_CAPTURE_CACHE: std::sync::OnceLock<tokio::sync::Mutex<ExecutableCaptureCache>> =
    std::sync::OnceLock::new();

impl EncodedExecutable {
    /// A selected Mac graph belongs to the implementation frozen in its plan.
    /// Ordinary plans retain their existing executable-selection behavior.
    pub(crate) fn matches_macos_plan(
        &self,
        plan: &plurx_core::transcode::ResolvedTranscode,
    ) -> bool {
        plan.captured_processing_ffmpeg_sha256()
            .is_none_or(|digest| digest == self.digest)
    }

    pub(crate) async fn capture_program(program: &str) -> Result<Self, String> {
        // PATH metadata and canonicalization can block on remote/cold storage.
        // Resolve away from runtime workers so callers can enforce their own
        // capture deadline; this read-only task owns no subprocess or media.
        let program = program.to_owned();
        let path = tokio::task::spawn_blocking(move || resolve_executable_path(&program))
            .await
            .map_err(|error| format!("executable resolution task failed: {error}"))?
            .ok_or("cannot resolve the producer executable")?;
        Self::capture_at(path).await
    }
    pub async fn capture() -> Result<Self, String> {
        Self::capture_program(&ffmpeg_bin()).await
    }

    pub(crate) async fn capture_at(path: std::path::PathBuf) -> Result<Self, String> {
        Self::capture_at_with_cache(path, EXECUTABLE_CAPTURE_CACHE.get_or_init(Default::default))
            .await
    }

    async fn capture_at_with_cache(
        path: std::path::PathBuf,
        cache: &tokio::sync::Mutex<ExecutableCaptureCache>,
    ) -> Result<Self, String> {
        let mut cache = cache.lock().await;
        if let Some(captured) = cache.executables.iter().find(|value| value.path == path) {
            let objects = vec![captured.attestation_object()].into();
            if engine_objects_are_current_batch(None, objects).await.0 {
                return Ok(captured.clone());
            }
        }
        let (digest, object_version) = hash_engine_object(&path).await?;
        let captured = Self {
            path,
            digest: hex::encode(digest),
            object_version,
            linux_binding: None,
        };
        #[cfg(test)]
        {
            cache.hashes += 1;
        }
        cache
            .executables
            .retain(|value| value.path != captured.path);
        if cache.executables.len() >= EXECUTABLE_CAPTURE_SLOTS {
            cache.executables.remove(0);
        }
        cache.executables.push(captured.clone());
        Ok(captured)
    }

    /// Validate this captured canonical launch object; do not follow the
    /// configured alias and silently substitute another executable here.
    pub(crate) async fn is_current(&self) -> bool {
        engine_objects_are_current_batch_with_linux(
            None,
            vec![self.attestation_object()].into(),
            self.linux_binding.clone(),
        )
        .await
        .0
    }

    pub(crate) fn bind_linux_plan(
        self,
        plan: &plurx_core::transcode::ResolvedTranscode,
    ) -> Result<Self, String> {
        let policy = plan.options().strict_dolby.as_ref();
        if policy.is_none_or(|policy| policy.linux_identity().is_none()) {
            return Ok(self);
        }
        let binding = policy
            .and_then(|policy| policy.linux_execution_binding())
            .cloned()
            .ok_or(
                "strict_dolby_implementation_changed: Linux plan has no retained execution binding",
            )?;
        self.bind_linux_execution(binding)
    }

    fn bind_linux_execution(
        mut self,
        binding: plurx_core::transcode::LinuxDolbyExecutionBinding,
    ) -> Result<Self, String> {
        let producer = binding
            .objects()
            .iter()
            .find(|object| {
                object.kind == plurx_core::transcode::LinuxExecutionObjectKind::ProducerExecutable
            })
            .ok_or("strict_dolby_implementation_changed: no retained producer object")?;
        if producer.path != self.path
            || producer.version.as_deref() != Some(self.object_version.as_str())
        {
            return Err("strict_dolby_implementation_changed: producer loader origin differs from the observed implementation".to_owned());
        }
        self.linux_binding = Some(binding);
        Ok(self)
    }

    /// Finite preparation retains an already-bound Linux execution object.
    /// Ordinary capture keeps its incumbent behavior; no active strict plan
    /// reopens the configured pathname to acquire a newer package implicitly.
    pub(crate) async fn capture_for_preparation(retained: &Self) -> Result<Self, String> {
        if retained.linux_binding.is_some() {
            Ok(retained.clone())
        } else {
            Self::capture().await
        }
    }

    pub(crate) fn processing_child_env(&self) -> Vec<(&str, &std::ffi::OsStr)> {
        self.linux_binding
            .iter()
            .flat_map(|binding| binding.child_env())
            .map(|(key, value)| (key.as_str(), std::ffi::OsStr::new(value)))
            .collect()
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn validate_source_clock_reporter(
        &self,
        path: &std::path::Path,
        digest: &str,
    ) -> Result<Option<plurx_core::transcode::LinuxSourceClockAssociation>, String> {
        use plurx_core::transcode::LinuxExecutionObjectKind;
        let Some(binding) = self.linux_binding.as_ref() else {
            return Ok(None);
        };
        let association = binding
            .source_clock()
            .ok_or("strict_dolby_clock_provenance_missing")?;
        let matches = binding.objects().iter().any(|object| {
            object.path == path
                && match object.kind {
                    LinuxExecutionObjectKind::NativeReporterExecutable => {
                        digest == association.native_reporter_digest()
                    }
                    LinuxExecutionObjectKind::SealedParserExecutable => {
                        digest == association.sealed_parser_digest()
                    }
                    _ => false,
                }
        });
        if !matches {
            return Err("strict_dolby_clock_reporter_implementation_mismatch".to_owned());
        }
        Ok(Some(association.clone()))
    }

    /// The `(path, version)` pair this executable attests.
    ///
    /// Callers fold this into the engine's blocking batch instead of
    /// statting it inline. The check is paid before every producer launch
    /// and before every segment is published, so an inline `metadata` on a
    /// cold page cache or a network mount would block a runtime worker that
    /// is also pumping media bodies — the exact stall the batch exists to
    /// remove.
    pub fn attestation_object(&self) -> (std::path::PathBuf, String) {
        (
            self.path.clone(),
            pinned_engine_object_version(&self.object_version, &self.digest),
        )
    }
}

/// The executable and first line it reports from `-version`.
pub async fn ffmpeg_build() -> String {
    let bin = ffmpeg_bin();
    let version = probe_ffmpeg(&["-version"])
        .await
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| !line.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "version unavailable".to_owned());
    format!("{bin} ({version})")
}

/// Keep the private completed output linked until its held-descriptor probe
/// finishes. Some bind filesystems cannot reopen an unlinked file through
/// /proc/self/fd even while the original descriptor remains valid.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn stage_completed_probe_file(
    bytes: &[u8],
    cache: &std::path::Path,
) -> std::io::Result<tempfile::NamedTempFile> {
    use std::io::{Seek, SeekFrom, Write};
    let mut held = tempfile::NamedTempFile::new_in(cache)?;
    held.write_all(bytes)?;
    held.seek(SeekFrom::Start(0))?;
    Ok(held)
}

/// Probe the exact source capability retained by a recipe preparer. Comparing
/// this document with the scanner's document prevents a same-size,
/// same-second pathname replacement from pairing fresh bytes with stale
/// geometry, tracks, cadence, or color facts.
///
/// `work` is the caller's: a session start that is waiting on this probe
/// passes a realtime class, since the probe runs under a five-second bound
/// whose miss the viewer sees as a refusal.
#[cfg(test)]
pub(crate) async fn held_source_probe_json(
    source: &std::fs::File,
    work: crate::process_control::ChildWork,
) -> Result<String, String> {
    let reporter = EncodedExecutable::capture_program(&ffprobe_bin()).await?;
    held_source_probe_json_bound(source, work, &reporter).await
}

/// The legacy held probe keeps its established bounded child owner but launches
/// the exact reporter captured before this query, rather than resolving an alias.
pub(crate) async fn held_source_probe_json_bound(
    source: &std::fs::File,
    work: crate::process_control::ChildWork,
    reporter: &EncodedExecutable,
) -> Result<String, String> {
    if !reporter.is_current().await {
        return Err("held clock reporter changed before query".into());
    }
    let command = source_held_probe_command(source, &reporter.path)?;
    let output = bounded_command_output_cancellable(
        command,
        ENGINE_PROBE_TIMEOUT,
        ENGINE_PROBE_MAX_BYTES,
        "engine probe",
        None,
        work,
    )
    .await?;
    if !reporter.is_current().await {
        return Err("held clock reporter changed during query".into());
    }
    let document = String::from_utf8(output.stdout).map_err(|_| "held probe JSON is not UTF-8")?;
    // The source clock is already bound to reporter bytes above. Preserve the
    // existing document stamp solely for stored media-fact comparison.
    Ok(stamped_with_this_reporter(document).await)
}

/// Name the build that produced a just-taken probe, so a later comparison can
/// tell an unchanged source from a changed reporter. A document that cannot be
/// parsed is handed back untouched: the comparison reports the parse failure,
/// which is the honest answer, rather than a stamp over unreadable output.
async fn stamped_with_this_reporter(document: String) -> String {
    let Some(reporter) = plurx_core::scan::probe::reporter_identity_of(&ffprobe_bin()).await else {
        return document;
    };
    let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&document) else {
        return document;
    };
    plurx_core::scan::probe::stamp_reporter(&mut parsed, reporter);
    serde_json::to_string(&parsed).unwrap_or(document)
}

/// The content-index probe has its own budget. A metadata read on a slow held
/// source may legitimately take longer than an executable capability probe,
/// while its JSON is expected to remain far smaller.
pub(crate) async fn held_source_index_probe_json(
    source: &std::fs::File,
    budget: Duration,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<String, String> {
    let document = held_source_probe_json_with_limits(
        source,
        Duration::from_secs(30).min(budget),
        1024 * 1024,
        "index metadata probe",
        crate::process_control::ChildWork::background("fragment index metadata probe"),
        cancel,
    )
    .await?;
    // Stamped for the same reason the engine probe is: the two entry points
    // must not disagree about a field whose whole purpose is to be compared.
    Ok(stamped_with_this_reporter(document).await)
}

/// How far past its start an EOF packet probe's interval ends: about 31
/// years, so the read still stops at the file's natural end.
///
/// The end is spelled out because ffprobe 8.0 does not honour an open-ended
/// interval: `-read_intervals "0%"` returned no packets at all, or every
/// packet, depending on which `-show_entries` fields accompanied it, while
/// the same interval with an explicit end returned every packet each time.
const EOF_PROBE_SPAN_SECONDS: i64 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeldPacketProbeRequest {
    pub stream_index: u32,
    /// `None` means the prefix. A value means an approximate seek followed by
    /// a read to natural EOF, spelled with an explicit end far past any file
    /// ([`EOF_PROBE_SPAN_SECONDS`]).
    pub start_seconds: Option<i64>,
    /// The prefix uses an explicit selected-packet count. EOF probes must use
    /// `None`; a capped tail is not EOF evidence.
    pub packet_limit: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HeldPacketProbeError {
    Timeout,
    OutputLimit,
    Source(String),
    Process(String),
}

impl HeldPacketProbeError {
    pub(crate) fn reason(&self) -> String {
        match self {
            Self::Timeout => "packet probe timed out".to_owned(),
            Self::OutputLimit => "packet probe exceeded its output bound".to_owned(),
            Self::Source(reason) | Self::Process(reason) => reason.clone(),
        }
    }
}

/// Read selected packet timestamps from the already-held source.
///
/// Every exit path owns, kills when necessary, and reaps the child before it
/// returns. The caller sequences requests sharing the descriptor and resets
/// its offset even after an error.
pub(crate) async fn held_source_packet_probe_json(
    source: &std::fs::File,
    request: HeldPacketProbeRequest,
    timeout: Duration,
    max_stdout_bytes: u64,
) -> Result<Vec<u8>, HeldPacketProbeError> {
    #[cfg(windows)]
    let (source_path, held_identity) = {
        let held_identity = plurx_core::fs_secure::std_file_identity(source).map_err(|error| {
            HeldPacketProbeError::Source(format!("reading held source identity: {error}"))
        })?;
        let source_path = plurx_core::fs_secure::std_file_path(source).map_err(|error| {
            HeldPacketProbeError::Source(format!("resolving held source path: {error}"))
        })?;
        let reopened =
            plurx_core::fs_secure::open_read_nofollow_blocking(&source_path).map_err(|error| {
                HeldPacketProbeError::Source(format!("reopening held source path: {error}"))
            })?;
        if plurx_core::fs_secure::std_file_identity(&reopened).map_err(|error| {
            HeldPacketProbeError::Source(format!("reading reopened source identity: {error}"))
        })? != held_identity
        {
            return Err(HeldPacketProbeError::Source(
                "held source path changed before packet probe launch".to_owned(),
            ));
        }
        (source_path, held_identity)
    };

    let mut command = tokio::process::Command::new(ffprobe_bin());
    #[cfg(unix)]
    inherit_file_descriptors(&mut command, &[(source, 3)]);
    command.args([
        "-v",
        "error",
        "-print_format",
        "json",
        "-select_streams",
        &request.stream_index.to_string(),
        "-show_packets",
        "-show_entries",
        "packet=stream_index,pts,dts,duration",
    ]);
    let interval = match (request.start_seconds, request.packet_limit) {
        (None, Some(limit)) => Some(format!("%+#{limit}")),
        (Some(start), None) => Some(format!("{}%+{EOF_PROBE_SPAN_SECONDS}", start.max(0))),
        (None, None) => None,
        (Some(_), Some(_)) => {
            return Err(HeldPacketProbeError::Process(
                "an EOF packet probe cannot carry a packet cap".to_owned(),
            ));
        }
    };
    if let Some(interval) = interval {
        command.args(["-read_intervals", &interval]);
    }
    #[cfg(unix)]
    command.arg("/dev/fd/3");
    #[cfg(windows)]
    command.arg(&source_path);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let (mut child, _child_job) = crate::process_control::spawn_job_owned(
        &mut command,
        crate::process_control::ChildWork::background("fragment index packet probe"),
    )
    .map_err(|error| HeldPacketProbeError::Process(format!("spawning packet probe: {error}")))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        HeldPacketProbeError::Process("packet probe started without stdout".to_owned())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        HeldPacketProbeError::Process("packet probe started without stderr".to_owned())
    })?;
    let collect = async {
        tokio::try_join!(
            read_packet_probe_pipe(stdout, max_stdout_bytes),
            read_packet_probe_pipe(stderr, 64 * 1_024),
            async {
                child.wait().await.map_err(|error| {
                    HeldPacketProbeError::Process(format!("waiting for packet probe: {error}"))
                })
            }
        )
    };
    let collected = tokio::time::timeout(timeout, collect).await;
    let result = match collected {
        Ok(Ok((stdout, _stderr, status))) if status.success() => Ok(stdout),
        Ok(Ok((_stdout, stderr, status))) => Err(HeldPacketProbeError::Process(format!(
            "packet probe exited {status}: {}",
            String::from_utf8_lossy(&stderr).trim()
        ))),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(HeldPacketProbeError::Timeout),
    };
    if result.is_err() {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
    }
    #[cfg(windows)]
    {
        if plurx_core::fs_secure::std_file_identity(source).map_err(|error| {
            HeldPacketProbeError::Source(format!("re-reading held source identity: {error}"))
        })? != held_identity
        {
            return Err(HeldPacketProbeError::Source(
                "held source changed during packet probe".to_owned(),
            ));
        }
        let current =
            plurx_core::fs_secure::open_read_nofollow_blocking(&source_path).map_err(|error| {
                HeldPacketProbeError::Source(format!(
                    "reopening packet-probed source path: {error}"
                ))
            })?;
        if plurx_core::fs_secure::std_file_identity(&current).map_err(|error| {
            HeldPacketProbeError::Source(format!("reading packet-probed source identity: {error}"))
        })? != held_identity
        {
            return Err(HeldPacketProbeError::Source(
                "held source path changed during packet probe".to_owned(),
            ));
        }
    }
    result
}

async fn read_packet_probe_pipe(
    input: impl AsyncRead + Unpin,
    max_bytes: u64,
) -> Result<Vec<u8>, HeldPacketProbeError> {
    let mut bytes = Vec::new();
    input
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| {
            HeldPacketProbeError::Process(format!("reading packet probe output: {error}"))
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(HeldPacketProbeError::OutputLimit);
    }
    Ok(bytes)
}

/// Actual held-descriptor probe command for an admitted Source owner. The
/// caller retains child/job/pipe ownership; this function never spawns.
pub(crate) fn source_held_probe_command(
    source: &std::fs::File,
    executable: &std::path::Path,
) -> Result<tokio::process::Command, String> {
    let mut command = tokio::process::Command::new(executable);
    #[cfg(unix)]
    {
        inherit_file_descriptors(&mut command, &[(source, 3)]);
    }
    command.args([
        "-v",
        "error",
        "-threads",
        "1",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
        "-show_chapters",
    ]);
    #[cfg(unix)]
    command.arg("/dev/fd/3");
    #[cfg(windows)]
    {
        let path = windows_source_path(source)?;
        verify_windows_source_path(source, &path)?;
        command.arg(path);
    }
    Ok(command)
}

/// Build a burn's subtitle-only Matroska sidecar from the held source,
/// without starting unowned subtitle work: the selected track with its ASS
/// styling or bitmap display state intact, and the source's attachments, so
/// a text burn's embedded fonts travel with the track to libass. The same
/// extraction Local's burn cache runs; the Source runs it as its own child.
pub(crate) fn source_burn_sidecar_command(
    source: &std::fs::File,
    index: i64,
    max_bytes: u64,
) -> Result<tokio::process::Command, String> {
    if !(0..=4095).contains(&index) {
        return Err("Source burn track is out of bounds".into());
    }
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    #[cfg(unix)]
    inherit_file_descriptors(&mut command, &[(source, 3)]);
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-copyts",
        "-start_at_zero",
        "-i",
    ]);
    #[cfg(unix)]
    command.arg("/dev/fd/3");
    #[cfg(windows)]
    {
        let path = windows_source_path(source)?;
        verify_windows_source_path(source, &path)?;
        command.arg(path);
    }
    command
        .args([
            "-map",
            &format!("0:s:{index}"),
            "-map",
            "0:t?",
            "-c",
            "copy",
            "-avoid_negative_ts",
            "disabled",
            "-f",
            "matroska",
            "-fs",
        ])
        .arg((max_bytes + 1).to_string())
        .arg("pipe:1");
    Ok(command)
}

/// Build native text extraction without starting unowned subtitle work.
pub(crate) fn source_native_text_command(
    source: &std::fs::File,
    ordinal: u16,
) -> Result<tokio::process::Command, String> {
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    #[cfg(unix)]
    inherit_file_descriptors(&mut command, &[(source, 3)]);
    command.args(["-hide_banner", "-loglevel", "error", "-threads", "1", "-i"]);
    #[cfg(unix)]
    command.arg("/dev/fd/3");
    #[cfg(windows)]
    {
        let path = windows_source_path(source)?;
        verify_windows_source_path(source, &path)?;
        command.arg(path);
    }
    command.args([
        "-map",
        &format!("0:s:{ordinal}"),
        "-vn",
        "-an",
        "-threads",
        "1",
        "-f",
        "webvtt",
        "pipe:1",
    ]);
    Ok(command)
}

async fn held_source_probe_json_with_limits(
    source: &std::fs::File,
    timeout: Duration,
    max_bytes: u64,
    label: &'static str,
    work: crate::process_control::ChildWork,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<String, String> {
    #[cfg(windows)]
    {
        let held_identity = plurx_core::fs_secure::std_file_identity(source)
            .map_err(|error| format!("reading held source identity: {error}"))?;
        let source_path = plurx_core::fs_secure::std_file_path(source)
            .map_err(|error| format!("resolving held source path: {error}"))?;
        let reopened = plurx_core::fs_secure::open_read_nofollow_blocking(&source_path)
            .map_err(|error| format!("reopening held source path: {error}"))?;
        if plurx_core::fs_secure::std_file_identity(&reopened)
            .map_err(|error| format!("reading reopened source identity: {error}"))?
            != held_identity
        {
            return Err("held source path changed before FFprobe launch".to_owned());
        }
        let mut command = tokio::process::Command::new(ffprobe_bin());
        command.args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            "-show_chapters",
        ]);
        command.arg(&source_path);
        let output =
            bounded_command_output_cancellable(command, timeout, max_bytes, label, cancel, work)
                .await?;
        if plurx_core::fs_secure::std_file_identity(source)
            .map_err(|error| format!("re-reading held source identity: {error}"))?
            != held_identity
        {
            return Err("held source changed during FFprobe".to_owned());
        }
        let current = plurx_core::fs_secure::open_read_nofollow_blocking(&source_path)
            .map_err(|error| format!("reopening probed source path: {error}"))?;
        if plurx_core::fs_secure::std_file_identity(&current)
            .map_err(|error| format!("reading probed source identity: {error}"))?
            != held_identity
        {
            return Err("held source path changed during FFprobe".to_owned());
        }
        return String::from_utf8(output.stdout)
            .map_err(|error| format!("ffprobe returned non-UTF-8 JSON: {error}"));
    }
    #[cfg(unix)]
    {
        let mut command = tokio::process::Command::new(ffprobe_bin());
        inherit_file_descriptors(&mut command, &[(source, 3)]);
        command.args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            "-show_chapters",
            "/dev/fd/3",
        ]);
        let output =
            bounded_command_output_cancellable(command, timeout, max_bytes, label, cancel, work)
                .await?;
        String::from_utf8(output.stdout)
            .map_err(|error| format!("ffprobe returned non-UTF-8 JSON: {error}"))
    }
}

fn normalized_probe_document(raw: &str) -> Result<serde_json::Value, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(raw).map_err(|error| format!("invalid ffprobe JSON: {error}"))?;
    if let Some(document) = value.as_object_mut() {
        // Application-owned measurements are grafted after the scan. A fresh
        // probe never carries them; their presence says nothing about whether
        // the source bytes changed. Keep all FFprobe-owned fields comparable.
        document.remove(plurx_core::transcode::hevc_census::PROBE_KEY);
        document.remove(plurx_core::store::CONTENT_ENCODING_PROBE_KEY);
        document.remove(plurx_core::scan::dv_title::PROBE_FIELD);
        document.remove(plurx_core::transcode::dv_fel_qualification::PROBE_KEY);
    }
    if let Some(format) = value
        .get_mut("format")
        .and_then(serde_json::Value::as_object_mut)
    {
        // The scan used the library pathname; the attested probe deliberately
        // used /dev/fd/3. The spelling is not a media fact.
        format.remove("filename");
    }
    // New FFprobe releases report explicit defaults which older scans omit.
    // Canonicalize only the measured empty/zero values; non-default media
    // facts must still invalidate the stored probe.
    if let Some(format) = value.get_mut("format") {
        remove_probe_default(format, "nb_stream_groups", &serde_json::json!(0));
    }
    if let Some(streams) = value
        .get_mut("streams")
        .and_then(serde_json::Value::as_array_mut)
    {
        for stream in streams {
            remove_probe_default(stream, "initial_padding", &serde_json::json!(0));
            remove_probe_default(stream, "view_ids_available", &serde_json::json!(""));
            remove_probe_default(stream, "view_pos_available", &serde_json::json!(""));
            if let Some(disposition) = stream.get_mut("disposition") {
                remove_probe_default(disposition, "multilayer", &serde_json::json!(0));
                remove_probe_default(disposition, "non_diegetic", &serde_json::json!(0));
            }
            if let Some(side_data) = stream
                .get_mut("side_data_list")
                .and_then(serde_json::Value::as_array_mut)
            {
                for item in side_data {
                    if item
                        .get("side_data_type")
                        .and_then(serde_json::Value::as_str)
                        == Some("DOVI configuration record")
                    {
                        remove_probe_default(item, "dv_md_compression", &serde_json::json!("none"));
                    }
                }
            }
        }
    }
    Ok(value)
}

fn remove_probe_default(value: &mut serde_json::Value, key: &str, default: &serde_json::Value) {
    if let Some(fields) = value.as_object_mut() {
        if fields.get(key) == Some(default) {
            fields.remove(key);
        }
    }
}

fn ignore_optional_stream_field_omissions(
    stored: &mut serde_json::Value,
    held: &mut serde_json::Value,
) {
    const OPTIONAL_CODEC_REPORT_FIELDS: [&str; 4] =
        ["closed_captions", "film_grain", "refs", "mime_codec_string"];
    const OPTIONAL_AUDIO_REPORT_FIELDS: [&str; 5] = [
        "dmix_mode",
        "loro_cmixlev",
        "loro_surmixlev",
        "ltrt_cmixlev",
        "ltrt_surmixlev",
    ];
    // Codec name paired with the exact derived profile a newer reporter adds.
    // FFmpeg a4e5b946 (March 2023) began reading the E-AC-3 extension type A
    // flag; TrueHD gained its equivalent earlier. An old scan that never
    // reported the property proves nothing about it either way, so the
    // omission is treated as unknown rather than as a changed source.
    const ATMOS_PROFILE_OMISSIONS: [(&str, &str); 2] = [
        ("truehd", "Dolby TrueHD + Dolby Atmos"),
        ("eac3", "Dolby Digital Plus + Dolby Atmos"),
    ];
    let (Some(stored_streams), Some(held_streams)) = (
        stored
            .get_mut("streams")
            .and_then(serde_json::Value::as_array_mut),
        held.get_mut("streams")
            .and_then(serde_json::Value::as_array_mut),
    ) else {
        return;
    };
    for (stored_stream, held_stream) in stored_streams.iter_mut().zip(held_streams.iter_mut()) {
        let (Some(stored_stream), Some(held_stream)) =
            (stored_stream.as_object_mut(), held_stream.as_object_mut())
        else {
            continue;
        };
        for field in OPTIONAL_CODEC_REPORT_FIELDS {
            // ffprobe releases may omit these decoder-analysis fields even
            // when probing the same bytes. Compare a reported value whenever
            // both documents have one, but do not turn schema availability
            // into a source-replacement verdict.
            if !stored_stream.contains_key(field) || !held_stream.contains_key(field) {
                stored_stream.remove(field);
                held_stream.remove(field);
            }
        }
        // Streams are paired by position, so the pairing itself has to be
        // proved before anything is removed from it: the same explicit
        // non-negative integer index on both sides, and audio on both sides.
        // A missing, null, negative, fractional, string or moved index is not
        // a pairing, and a report that changed one must refuse.
        let matching_audio_stream = matches!(
            (
                stored_stream
                    .get("index")
                    .and_then(serde_json::Value::as_u64),
                held_stream.get("index").and_then(serde_json::Value::as_u64),
            ),
            (Some(stored_index), Some(held_index)) if stored_index == held_index
        ) && stored_stream
            .get("codec_type")
            .and_then(serde_json::Value::as_str)
            == Some("audio")
            && held_stream
                .get("codec_type")
                .and_then(serde_json::Value::as_str)
                == Some("audio");
        if matching_audio_stream {
            for field in OPTIONAL_AUDIO_REPORT_FIELDS {
                if !stored_stream.contains_key(field) || !held_stream.contains_key(field) {
                    stored_stream.remove(field);
                    held_stream.remove(field);
                }
            }
            // Newer FFprobe derives an Atmos profile name that older scans
            // omitted entirely; the bitstream did not change, only what the
            // reporter is able to say about it. Admit that exact omission and
            // nothing else: the codec must match a known pairing on both
            // sides, exactly one document must omit `profile`, and the other
            // must carry the literal name. Two reported profiles are always
            // compared, so reported-Atmos versus reported-non-Atmos still
            // refuses.
            for (codec_name, atmos_profile) in ATMOS_PROFILE_OMISSIONS {
                if stored_stream
                    .get("codec_name")
                    .and_then(serde_json::Value::as_str)
                    != Some(codec_name)
                    || held_stream
                        .get("codec_name")
                        .and_then(serde_json::Value::as_str)
                        != Some(codec_name)
                {
                    continue;
                }
                let stored_omits_profile = !stored_stream.contains_key("profile");
                let held_omits_profile = !held_stream.contains_key("profile");
                let admits_omission = (stored_omits_profile
                    && held_stream
                        .get("profile")
                        .and_then(serde_json::Value::as_str)
                        == Some(atmos_profile))
                    || (held_omits_profile
                        && stored_stream
                            .get("profile")
                            .and_then(serde_json::Value::as_str)
                            == Some(atmos_profile));
                if admits_omission {
                    stored_stream.remove("profile");
                    held_stream.remove("profile");
                }
                break;
            }
        }
    }
}

/// How two normalized probe documents disagree at one field path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProbeDifferenceKind {
    /// One document carries the field and the other does not.
    Missing,
    /// Both carry a scalar of the same shape with different contents.
    Value,
    /// The two values are different JSON shapes.
    Type,
    /// Two arrays at the same path have different lengths.
    Length,
}

impl ProbeDifferenceKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Value => "value",
            Self::Type => "type",
            Self::Length => "length",
        }
    }
}

/// One normalized field path and how it disagreed. Deliberately carries no
/// value, no pathname and no free-form tag text: this is a diagnosis aid, not
/// a probe dump, and it is written to a log an operator reads.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ProbeDifference {
    pub path: String,
    pub kind: ProbeDifferenceKind,
}

/// The comparison result, with a bounded explanation of a refusal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ProbeComparison {
    pub same: bool,
    /// Admitted on the media facts rather than on the whole document, because
    /// the two documents were proved to come from different FFprobe builds.
    pub admitted_on_reporter_drift: bool,
    pub differences: Vec<ProbeDifference>,
    /// Collection stopped at the limit; more differences exist.
    pub truncated: bool,
}

impl ProbeComparison {
    /// A single log-safe line: `"/streams/2/profile missing"`.
    pub(crate) fn rendered_differences(&self) -> String {
        let mut rendered = self
            .differences
            .iter()
            .map(|difference| format!("{} {}", difference.path, difference.kind.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        if self.truncated {
            if !rendered.is_empty() {
                rendered.push_str(", ");
            }
            rendered.push_str("…more");
        }
        if rendered.is_empty() {
            // Two documents that are unequal always disagree somewhere, but a
            // caller rendering this into a refusal must never be handed an
            // empty clause if that ever stops being true.
            rendered.push_str("an unnamed field");
        }
        rendered
    }
}

/// At most this many differing paths are collected before the walk stops.
const PROBE_DIFFERENCE_LIMIT: usize = 8;
/// A rendered path longer than this is truncated; no path can grow unbounded
/// from a document an external tool produced.
const PROBE_PATH_MAX_CHARS: usize = 128;

/// FFprobe schema names this server is willing to name in a log. Anything
/// else — a private container tag, a field a future release adds — is
/// collapsed to a category so a log line can never leak library text.
const KNOWN_PROBE_FIELDS: &[&str] = &[
    "attached_pic",
    "avg_frame_rate",
    "bit_rate",
    "bits_per_raw_sample",
    "bits_per_sample",
    "bl_present_flag",
    "blue_x",
    "blue_y",
    "captions",
    "channel_layout",
    "channels",
    "chapters",
    "chroma_location",
    "clean_effects",
    "closed_captions",
    "codec_long_name",
    "codec_name",
    "codec_tag",
    "codec_tag_string",
    "codec_type",
    "coded_height",
    "coded_width",
    "color_primaries",
    "color_range",
    "color_space",
    "color_transfer",
    "comment",
    "default",
    "dependent",
    "descriptions",
    "disposition",
    "display_aspect_ratio",
    "displaymatrix",
    "dmix_mode",
    "dub",
    "duration",
    "duration_ts",
    "dv_bl_signal_compatibility_id",
    "dv_level",
    "dv_md_compression",
    "dv_profile",
    "dv_version_major",
    "dv_version_minor",
    "el_present_flag",
    "end",
    "end_time",
    "extradata_size",
    "field_order",
    "film_grain",
    "filename",
    "forced",
    "format",
    "format_long_name",
    "format_name",
    "green_x",
    "green_y",
    "has_b_frames",
    "hearing_impaired",
    "height",
    "id",
    "index",
    "initial_padding",
    "is_avc",
    "karaoke",
    "level",
    "loro_cmixlev",
    "loro_surmixlev",
    "ltrt_cmixlev",
    "ltrt_surmixlev",
    "lyrics",
    "max_average",
    "max_bit_rate",
    "max_content",
    "max_luminance",
    "metadata",
    "mime_codec_string",
    "min_luminance",
    "multilayer",
    "nal_length_size",
    "nb_frames",
    "nb_programs",
    "nb_read_frames",
    "nb_read_packets",
    "nb_stream_groups",
    "nb_streams",
    "non_diegetic",
    "original",
    "pix_fmt",
    "plurx_probe_reporter",
    "probe_score",
    "profile",
    "programs",
    "r_frame_rate",
    "red_x",
    "red_y",
    "refs",
    "rotation",
    "rpu_present_flag",
    "sample_aspect_ratio",
    "sample_fmt",
    "sample_rate",
    "service_type",
    "side_data_list",
    "side_data_type",
    "size",
    "start",
    "start_pts",
    "start_time",
    "still_image",
    "stream_groups",
    "streams",
    "tags",
    "time_base",
    "timed_thumbnails",
    "view_ids_available",
    "view_pos_available",
    "visual_impaired",
    "white_point_x",
    "white_point_y",
    "width",
];

/// Render one object key for a diagnostic path. A key inside a `tags` object
/// is always collapsed, because container tags carry title text.
fn rendered_probe_field(key: &str, inside_tags: bool) -> &'static str {
    if inside_tags {
        // The language a track declares is a catalog fact rather than title
        // text, and it is the one container tag the comparison reads.
        return if key == "language" {
            "language"
        } else {
            "<field>"
        };
    }
    KNOWN_PROBE_FIELDS
        .iter()
        .copied()
        .find(|known| *known == key)
        .unwrap_or("<unknown-field>")
}

fn rendered_probe_path(segments: &[String]) -> String {
    let mut path = String::new();
    for segment in segments {
        path.push('/');
        path.push_str(segment);
    }
    if path.chars().count() > PROBE_PATH_MAX_CHARS {
        path = path.chars().take(PROBE_PATH_MAX_CHARS).collect();
    }
    if path.is_empty() {
        path.push('/');
    }
    path
}

/// Walk two normalized documents and record where they disagree, stopping at
/// `PROBE_DIFFERENCE_LIMIT`. The inputs are the same normalized copies the
/// admission decision used, so a diagnosis can never contradict the verdict.
fn collect_probe_differences(
    stored: &serde_json::Value,
    held: &serde_json::Value,
    segments: &mut Vec<String>,
    inside_tags: bool,
    differences: &mut Vec<ProbeDifference>,
    truncated: &mut bool,
) {
    if differences.len() >= PROBE_DIFFERENCE_LIMIT {
        *truncated = true;
        return;
    }
    match (stored, held) {
        (serde_json::Value::Object(stored_fields), serde_json::Value::Object(held_fields)) => {
            let mut keys: Vec<&String> = stored_fields.keys().collect();
            for key in held_fields.keys() {
                if !stored_fields.contains_key(key) {
                    keys.push(key);
                }
            }
            for key in keys {
                if differences.len() >= PROBE_DIFFERENCE_LIMIT {
                    *truncated = true;
                    return;
                }
                segments.push(rendered_probe_field(key, inside_tags).to_string());
                match (stored_fields.get(key), held_fields.get(key)) {
                    (Some(stored_value), Some(held_value)) => collect_probe_differences(
                        stored_value,
                        held_value,
                        segments,
                        inside_tags || key == "tags",
                        differences,
                        truncated,
                    ),
                    _ => differences.push(ProbeDifference {
                        path: rendered_probe_path(segments),
                        kind: ProbeDifferenceKind::Missing,
                    }),
                }
                segments.pop();
            }
        }
        (serde_json::Value::Array(stored_items), serde_json::Value::Array(held_items)) => {
            if stored_items.len() != held_items.len() {
                differences.push(ProbeDifference {
                    path: rendered_probe_path(segments),
                    kind: ProbeDifferenceKind::Length,
                });
                return;
            }
            for (at, (stored_item, held_item)) in
                stored_items.iter().zip(held_items.iter()).enumerate()
            {
                if differences.len() >= PROBE_DIFFERENCE_LIMIT {
                    *truncated = true;
                    return;
                }
                segments.push(at.to_string());
                collect_probe_differences(
                    stored_item,
                    held_item,
                    segments,
                    inside_tags,
                    differences,
                    truncated,
                );
                segments.pop();
            }
        }
        _ if stored == held => {}
        _ => differences.push(ProbeDifference {
            path: rendered_probe_path(segments),
            kind: if std::mem::discriminant(stored) == std::mem::discriminant(held) {
                ProbeDifferenceKind::Value
            } else {
                ProbeDifferenceKind::Type
            },
        }),
    }
}

/// Compare a stored scan against a held-source probe and, on a refusal, say
/// which normalized fields disagreed. Admission is unchanged: `same` is the
/// same verdict [`probes_describe_same_input`] has always returned.
pub(crate) fn compare_probe_documents(stored: &str, held: &str) -> Result<ProbeComparison, String> {
    let (stored, held) = normalized_probe_pair(stored, held)?;
    if stored == held {
        return Ok(ProbeComparison {
            same: true,
            admitted_on_reporter_drift: false,
            differences: Vec::new(),
            truncated: false,
        });
    }
    // Two documents from the same FFprobe build describe the same bytes the
    // same way, so everything that moved between them is a fact about the
    // source and the whole document is the right comparison. Different builds
    // do not: they add derived labels, drop container tags, and estimate
    // durations and bitrates to different precision over a file nothing
    // touched. A *proved* difference in reporter — and nothing weaker — buys
    // the narrower comparison, which is the same reasoning each of the
    // field-by-field exceptions above was making one field at a time.
    let reporters_differ = match (
        plurx_core::scan::probe::reporter_of(&stored),
        plurx_core::scan::probe::reporter_of(&held),
    ) {
        (Some(stored), Some(held)) => stored != held,
        // A scan kept from before this server stamped its documents, read by a
        // probe that could name itself. That is a different build by
        // construction: the stamp ships with the build that writes it.
        (None, Some(_)) => true,
        // The held probe could not name itself, so there is no proof of
        // anything. Absence of proof must not be read as proof, or one failed
        // `ffprobe -version` would narrow the gate for a whole library whose
        // documents came from the build running right now.
        _ => false,
    };
    if reporters_differ && stream_lists_are_pairable(&stored, &held) {
        let (stored_facts, held_facts) = source_fact_pair(&stored, &held);
        if stored_facts == held_facts {
            return Ok(ProbeComparison {
                same: true,
                admitted_on_reporter_drift: true,
                differences: Vec::new(),
                truncated: false,
            });
        }
        return Ok(probe_document_differences(&stored_facts, &held_facts));
    }
    Ok(probe_document_differences(&stored, &held))
}

/// Where two documents the comparison refused disagree. The inputs are the
/// same copies the verdict used, so a diagnosis cannot contradict it.
fn probe_document_differences(
    stored: &serde_json::Value,
    held: &serde_json::Value,
) -> ProbeComparison {
    let mut differences = Vec::new();
    let mut truncated = false;
    collect_probe_differences(
        stored,
        held,
        &mut Vec::new(),
        false,
        &mut differences,
        &mut truncated,
    );
    ProbeComparison {
        same: false,
        admitted_on_reporter_drift: false,
        differences,
        truncated,
    }
}

fn normalized_probe_pair(
    stored: &str,
    held: &str,
) -> Result<(serde_json::Value, serde_json::Value), String> {
    let mut stored = normalized_probe_document(stored)?;
    let mut held = normalized_probe_document(held)?;
    // Before the scanner requested -show_chapters, omission meant unmeasured,
    // not an empty chapter list. Compare chapter arrays when both probes
    // measured them, including empty-to-populated changes. A malformed value
    // is never treated as an optional report.
    if stored.get("chapters").is_none() && held.get("chapters").is_some_and(|v| v.is_array()) {
        if let Some(fields) = held.as_object_mut() {
            fields.remove("chapters");
        }
    } else if held.get("chapters").is_none() && stored.get("chapters").is_some_and(|v| v.is_array())
    {
        if let Some(fields) = stored.as_object_mut() {
            fields.remove("chapters");
        }
    }
    ignore_optional_stream_field_omissions(&mut stored, &mut held);
    Ok((stored, held))
}

/// Container facts a stored scan asserts about a source.
const FACT_FORMAT_FIELDS: &[&str] = &["duration", "format_name", "nb_streams", "size"];

/// Per-stream facts the catalog derives from, the recipe reads, or the
/// delivery decision acts on: geometry, cadence, codec identity, the byte
/// layout of the parameter sets, color, and the audio shape.
///
/// This list is a whitelist on purpose, but it is not a judgement about what
/// "matters" — it is the set of fields two FFprobe builds were measured
/// agreeing about over unchanged bytes. Anything left out is left out because
/// it drifts with the reporter (`start_time`, `start_pts`, `bit_rate`,
/// `initial_padding`, `ts_id`, `refs`, `film_grain`, `closed_captions`,
/// `mime_codec_string`, the downmix levels), and every field named here is
/// compared strictly, including when only one document reports it.
const FACT_STREAM_FIELDS: &[&str] = &[
    "avg_frame_rate",
    "bits_per_raw_sample",
    "bits_per_sample",
    "channel_layout",
    "channels",
    "chroma_location",
    "codec_name",
    "codec_tag_string",
    "codec_type",
    "coded_height",
    "coded_width",
    "color_primaries",
    "color_range",
    "color_space",
    "color_transfer",
    "display_aspect_ratio",
    // The HEVC sample-entry decision and the copy path's parameter-set
    // promotion are read out of the STORED document (`plurx_core::transcode::
    // hevc_parameter_set_promotion_required`), so the byte layout of the
    // extradata is a fact about this source, not about who described it.
    "extradata_size",
    "field_order",
    "has_b_frames",
    "height",
    "index",
    "is_avc",
    "level",
    "nal_length_size",
    "pix_fmt",
    "profile",
    "r_frame_rate",
    "sample_aspect_ratio",
    "sample_fmt",
    "sample_rate",
    "time_base",
    "width",
];

/// The dispositions the product reads.
const FACT_DISPOSITION_FIELDS: &[&str] = &[
    "attached_pic",
    "default",
    "forced",
    "hearing_impaired",
    "visual_impaired",
];

/// Container tags are title text, with one exception: the language a track
/// declares is a catalog fact and is compared.
const FACT_TAG_FIELDS: &[&str] = &["language"];

const FACT_CHAPTER_FIELDS: &[&str] = &["end", "end_time", "start", "start_time", "time_base"];

/// FFprobe builds disagree about a container duration by tens of milliseconds
/// over the same bytes — measured at 2511.520 against 2511.477 seconds on one
/// production file. The tolerance is relative so it cannot swallow a quarter
/// of a four-second clip, floored so it survives that drift on a short one and
/// capped so it never grows into a real edit on a long one.
const FACT_DURATION_DRIFT_FRACTION: f64 = 1e-4;
const FACT_DURATION_TOLERANCE_MIN_SECONDS: f64 = 0.05;
const FACT_DURATION_TOLERANCE_MAX_SECONDS: f64 = 1.0;

fn kept_fields(source: Option<&serde_json::Value>, fields: &[&str]) -> serde_json::Value {
    let mut kept = serde_json::Map::new();
    if let Some(present) = source.and_then(serde_json::Value::as_object) {
        for field in fields {
            if let Some(value) = present.get(*field) {
                kept.insert((*field).to_owned(), value.clone());
            }
        }
    }
    serde_json::Value::Object(kept)
}

fn probe_duration_seconds(value: Option<&serde_json::Value>) -> Option<f64> {
    let seconds = match value {
        Some(serde_json::Value::String(text)) => text.parse::<f64>().ok(),
        Some(serde_json::Value::Number(number)) => number.as_f64(),
        _ => None,
    }?;
    seconds.is_finite().then_some(seconds)
}

/// One stream, reduced to the facts above.
///
/// `side_data_list` is carried whole and keyed by type rather than filtered to
/// a known set: a record this projection did not recognize is still a record
/// that changed, and dropping it would admit an HDR10+ source replaced by an
/// HDR10 one. Keying by type also means a reporter that emits the records in a
/// different order does not pair a mastering-display record against a Dolby
/// Vision one.
fn stream_facts(stream: &serde_json::Value) -> serde_json::Value {
    let mut facts = kept_fields(Some(stream), FACT_STREAM_FIELDS);
    let object = facts.as_object_mut().expect("kept fields build an object");
    object.insert(
        "disposition".to_owned(),
        kept_fields(stream.get("disposition"), FACT_DISPOSITION_FIELDS),
    );
    object.insert(
        "tags".to_owned(),
        kept_fields(stream.get("tags"), FACT_TAG_FIELDS),
    );
    let mut side_data = serde_json::Map::new();
    for record in stream
        .get("side_data_list")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let kind = record
            .get("side_data_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("<unnamed>")
            .to_owned();
        side_data
            .entry(kind)
            .or_insert_with(|| serde_json::Value::Array(Vec::new()))
            .as_array_mut()
            .expect("entry is an array")
            .push(record.clone());
    }
    object.insert(
        "side_data_list".to_owned(),
        serde_json::Value::Object(side_data),
    );
    serde_json::Value::Object(object.clone())
}

/// The media facts one probe document asserts, as this product models them.
fn source_facts(document: &serde_json::Value) -> serde_json::Value {
    let mut facts = serde_json::Map::new();
    facts.insert(
        "format".to_owned(),
        kept_fields(document.get("format"), FACT_FORMAT_FIELDS),
    );
    if let Some(streams) = document
        .get("streams")
        .and_then(serde_json::Value::as_array)
    {
        facts.insert(
            "streams".to_owned(),
            serde_json::Value::Array(streams.iter().map(stream_facts).collect()),
        );
    }
    if let Some(chapters) = document
        .get("chapters")
        .and_then(serde_json::Value::as_array)
    {
        facts.insert(
            "chapters".to_owned(),
            serde_json::Value::Array(
                chapters
                    .iter()
                    .map(|chapter| kept_fields(Some(chapter), FACT_CHAPTER_FIELDS))
                    .collect(),
            ),
        );
    }
    serde_json::Value::Object(facts)
}

/// Held sources this process admitted on their media facts rather than on the
/// whole stored document. Work this server does on weaker evidence than it
/// prefers is work an operator must be able to see without a shell.
static REPORTER_DRIFT_ADMISSIONS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub(crate) fn note_reporter_drift_admission() {
    REPORTER_DRIFT_ADMISSIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn reporter_drift_admissions() -> u64 {
    REPORTER_DRIFT_ADMISSIONS.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn reporter_drift_prometheus() -> String {
    format!(
        "# HELP plurx_probe_reporter_drift_admissions_total Held sources admitted on their media facts because their stored scan came from a different FFprobe build.\n\
         # TYPE plurx_probe_reporter_drift_admissions_total counter\n\
         plurx_probe_reporter_drift_admissions_total {}\n",
        reporter_drift_admissions()
    )
}

/// Whether the two stream lists can be compared position by position at all.
/// The fact comparison pairs by position, so the pairing itself has to be
/// proved first: the same count, and the same explicit non-negative integer
/// index at every position. Without that proof there is no weaker verdict to
/// fall back to and the whole-document comparison stands.
fn stream_lists_are_pairable(stored: &serde_json::Value, held: &serde_json::Value) -> bool {
    let (Some(stored), Some(held)) = (
        stored.get("streams").and_then(serde_json::Value::as_array),
        held.get("streams").and_then(serde_json::Value::as_array),
    ) else {
        return false;
    };
    if stored.len() != held.len() {
        return false;
    }
    stored.iter().zip(held.iter()).all(|(stored, held)| {
        matches!(
            (
                stored.get("index").and_then(serde_json::Value::as_u64),
                held.get("index").and_then(serde_json::Value::as_u64),
            ),
            (Some(stored), Some(held)) if stored == held
        )
    })
}

/// Both documents' facts, with the one measured tolerance applied.
///
/// Every field named in the projection is otherwise compared strictly,
/// including its absence: these are facts about the source, and a reporter
/// that did not mention one is not evidence that the source lost it. Only
/// fields the projection never names are ignored, and each of those is there
/// because two builds were measured disagreeing about it over unchanged bytes.
fn source_fact_pair(
    stored: &serde_json::Value,
    held: &serde_json::Value,
) -> (serde_json::Value, serde_json::Value) {
    let mut stored = source_facts(stored);
    let mut held = source_facts(held);
    if let (Some(stored_seconds), Some(held_seconds)) = (
        probe_duration_seconds(stored.pointer("/format/duration")),
        probe_duration_seconds(held.pointer("/format/duration")),
    ) {
        let tolerance = (stored_seconds.abs() * FACT_DURATION_DRIFT_FRACTION).clamp(
            FACT_DURATION_TOLERANCE_MIN_SECONDS,
            FACT_DURATION_TOLERANCE_MAX_SECONDS,
        );
        if (stored_seconds - held_seconds).abs() <= tolerance {
            let canonical = serde_json::json!(format!("{stored_seconds:.3}"));
            if let Some(duration) = stored.pointer_mut("/format/duration") {
                *duration = canonical.clone();
            }
            if let Some(duration) = held.pointer_mut("/format/duration") {
                *duration = canonical;
            }
        }
    }
    (stored, held)
}

/// Which pacing flags this ffmpeg understands. `-readrate` landed in 5.1 and
/// `-readrate_initial_burst` in 6.1; passing either to an older build is a hard
/// exit, not a warning, so probe rather than assume. Probed once per process.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct PacingCaps {
    pub readrate: bool,
    pub initial_burst: bool,
}

static PACING: tokio::sync::OnceCell<PacingCaps> = tokio::sync::OnceCell::const_new();
static DOVI_RPU: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
static DOVI_RESHAPE: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
/// One probe result per hardware encoder pairing for the Dolby Vision reshape.
/// Keyed by `Encoder::name()`; a pairing is attempted only after it is proved
/// here, which is the same discipline every other renderer gets.
static DOVI_RESHAPE_HW: std::sync::OnceLock<
    tokio::sync::Mutex<std::collections::HashMap<&'static str, bool>>,
> = std::sync::OnceLock::new();
static DOVI_PASSTHROUGH: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
static DOVI_PASSTHROUGH_QSV: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
static HDR10_PASSTHROUGH: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
static HDR10_PASSTHROUGH_QSV: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
static HDR10_PASSTHROUGH_VAAPI: [tokio::sync::OnceCell<bool>; 2] =
    [const { tokio::sync::OnceCell::const_new() }; 2];
/// What this process knows about the VA-API HEVC Main10 decode: proved (kept
/// for the process), or failed `failures` times and next tried at `retry_at`.
struct Main10DecodeProof {
    proved: bool,
    failures: u32,
    retry_at: Option<std::time::Instant>,
}
static HEVC_MAIN10_VAAPI_DECODE: std::sync::Mutex<Main10DecodeProof> =
    std::sync::Mutex::new(Main10DecodeProof {
        proved: false,
        failures: 0,
        retry_at: None,
    });
/// One probe at a time; a caller that finds one running does not wait.
static HEVC_MAIN10_VAAPI_PROBE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// A two-picture 256x144 HEVC Main10 clip (x265, `-bitexact`), so the probe
/// needs no encoder of its own.
const HEVC_MAIN10_PROBE_CLIP: &[u8] = include_bytes!("ffmpeg/hevc-main10-probe.mkv");
/// Decoding two small pictures takes well under a second on a working device.
const HEVC_MAIN10_PROBE_DEADLINE: Duration = Duration::from_secs(5);
static FRAGMENT_INDEX_ENGINE: tokio::sync::OnceCell<FragmentIndexEngine> =
    tokio::sync::OnceCell::const_new();
static ENCODED_PROCESS_IDENTITY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

const ENGINE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
// Fontconfig walks the complete active rules and font-file closure. On a
// saturated transcoding or CI host that bounded inventory can legitimately
// take longer than the lightweight executable capability probes above.
const FONT_ENGINE_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// Runaway guard for what a probe subprocess may hand back, not a correctness
/// gate: exceeding it turns a real answer into "this build has no features",
/// so it has to sit far above anything a healthy ffmpeg can legitimately say.
///
/// It was 1 MiB, which `ffmpeg -h full` outgrew years ago — measured stdout is
/// 1,005,926 bytes on 4.4.2 and 1,165,847 bytes on 6.1.1, and 8.x lists more
/// still. Every build from 6.1 on therefore failed the pacing probe and was
/// classified as having no `-readrate`, so remux streams ran unpaced and HLS
/// fell back to realtime pacing — silently, because a bounded read is
/// indistinguishable from a missing binary at the call site.
const ENGINE_PROBE_MAX_BYTES: u64 = 16 * 1024 * 1024;
const ENGINE_OBJECT_MAX_BYTES: u64 = 512 * 1024 * 1024;

// Reuse the process histogram's established latency bounds. Attestation has
// both sub-millisecond warm-cache stats and bounded 30-second probes; +Inf
// retains the latter without inventing a second bucket vocabulary.
const ENGINE_ATTESTATION_BUCKETS_MS: [u64; 7] = [100, 250, 500, 1_000, 2_500, 5_000, 10_000];
const ENGINE_ATTESTATION_MEDIA: usize = 0;
const ENGINE_ATTESTATION_FONT: usize = 1;
const ENGINE_ATTESTATION_SPAWN: usize = 0;
const ENGINE_ATTESTATION_STAT: usize = 1;
static ENGINE_ATTESTATION_BUCKETS: [[[std::sync::atomic::AtomicU64; 8]; 2]; 2] = [const {
    [const { [const { std::sync::atomic::AtomicU64::new(0) }; ENGINE_ATTESTATION_BUCKETS_MS.len() + 1] };
        2]
}; 2];
static ENGINE_ATTESTATION_MICROS: [[std::sync::atomic::AtomicU64; 2]; 2] =
    [const { [const { std::sync::atomic::AtomicU64::new(0) }; 2] }; 2];

#[derive(Clone, Copy)]
enum EngineAttestationKind {
    Media,
    Font,
}

impl EngineAttestationKind {
    fn index(self) -> usize {
        match self {
            Self::Media => ENGINE_ATTESTATION_MEDIA,
            Self::Font => ENGINE_ATTESTATION_FONT,
        }
    }

    #[cfg(test)]
    fn label(self) -> &'static str {
        match self {
            Self::Media => "media",
            Self::Font => "font",
        }
    }
}

#[derive(Clone, Copy)]
enum EngineAttestationPhase {
    Spawn,
    Stat,
}

impl EngineAttestationPhase {
    fn index(self) -> usize {
        match self {
            Self::Spawn => ENGINE_ATTESTATION_SPAWN,
            Self::Stat => ENGINE_ATTESTATION_STAT,
        }
    }

    #[cfg(test)]
    fn label(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Stat => "stat",
        }
    }
}

/// What one logical attestation owes the histogram.
///
/// An attestation is several internal blocking batches — the captured media
/// objects, the captured font objects, the font re-enumeration's own stat
/// loop, and the enumerated closure — and each series must be charged once
/// for the whole check, not once per batch. Observing per batch made
/// `plurx_engine_attestation_seconds_count{kind="font",phase="stat"}` read
/// three times the number of checks on a text-burn recipe, so `_sum/_count`
/// was not the mean cost of a check and a before/after comparison across a
/// milestone that collapses those batches compared different units.
///
/// Time is summed per `(kind, phase)`; the series is chosen by what was
/// actually statted, never by the recipe's kind. A burn recipe stats the
/// media dependency closure too, and that cost belongs under `media`.
#[derive(Default)]
struct EngineAttestationCharges {
    entries: Vec<EngineAttestationCharge>,
}

struct EngineAttestationCharge {
    kind: EngineAttestationKind,
    phase: EngineAttestationPhase,
    elapsed: Duration,
    /// How many internal blocking batches were folded into this one
    /// observation. Three, for the font stats of a text-burn check.
    batches: usize,
}

impl EngineAttestationCharges {
    /// Fold one internal batch into its series. Every batch is charged
    /// through here, so the flushed observation count is the number of
    /// attestations rather than the number of batches.
    fn add(
        &mut self,
        kind: EngineAttestationKind,
        phase: EngineAttestationPhase,
        elapsed: Duration,
    ) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| {
            entry.kind.index() == kind.index() && entry.phase.index() == phase.index()
        }) {
            entry.elapsed = entry.elapsed.saturating_add(elapsed);
            entry.batches += 1;
            return;
        }
        self.entries.push(EngineAttestationCharge {
            kind,
            phase,
            elapsed,
            batches: 1,
        });
    }

    fn flush(&self) {
        for entry in &self.entries {
            observe_engine_attestation(entry.kind, entry.phase, entry.elapsed);
        }
    }

    /// The series this attestation charged and how many batches each folded,
    /// sorted, so a test can pin both the label chosen for a batch and the
    /// fact that the series is observed once however many batches it ran.
    #[cfg(test)]
    fn charged(&self) -> Vec<(&'static str, &'static str, usize)> {
        let mut charged: Vec<(&'static str, &'static str, usize)> = self
            .entries
            .iter()
            .map(|entry| (entry.kind.label(), entry.phase.label(), entry.batches))
            .collect();
        charged.sort_unstable();
        charged
    }
}

fn observe_engine_attestation(
    kind: EngineAttestationKind,
    phase: EngineAttestationPhase,
    elapsed: Duration,
) {
    use std::sync::atomic::Ordering;

    let kind = kind.index();
    let phase = phase.index();
    let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    let bucket = ENGINE_ATTESTATION_BUCKETS_MS
        .iter()
        .position(|bound| elapsed_ms <= *bound)
        .unwrap_or(ENGINE_ATTESTATION_BUCKETS_MS.len());
    ENGINE_ATTESTATION_BUCKETS[kind][phase][bucket].fetch_add(1, Ordering::Relaxed);
    ENGINE_ATTESTATION_MICROS[kind][phase].fetch_add(
        u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
}

pub(crate) fn engine_attestation_prometheus() -> String {
    use std::sync::atomic::Ordering;

    let mut out = String::from(
        "# HELP plurx_engine_attestation_seconds Time spent probing and statting immutable media-engine inputs.\n\
         # TYPE plurx_engine_attestation_seconds histogram\n",
    );
    for (kind_index, kind) in ["media", "font"].iter().enumerate() {
        for (phase_index, phase) in ["spawn", "stat"].iter().enumerate() {
            let mut cumulative = 0_u64;
            for (bucket_index, bound_ms) in ENGINE_ATTESTATION_BUCKETS_MS.iter().enumerate() {
                cumulative = cumulative.saturating_add(
                    ENGINE_ATTESTATION_BUCKETS[kind_index][phase_index][bucket_index]
                        .load(Ordering::Relaxed),
                );
                out.push_str(&format!(
                    "plurx_engine_attestation_seconds_bucket{{kind=\"{kind}\",phase=\"{phase}\",le=\"{}\"}} {cumulative}\n",
                    *bound_ms as f64 / 1_000.0
                ));
            }
            cumulative = cumulative.saturating_add(
                ENGINE_ATTESTATION_BUCKETS[kind_index][phase_index]
                    [ENGINE_ATTESTATION_BUCKETS_MS.len()]
                .load(Ordering::Relaxed),
            );
            let sum = ENGINE_ATTESTATION_MICROS[kind_index][phase_index].load(Ordering::Relaxed)
                as f64
                / 1_000_000.0;
            out.push_str(&format!(
                "plurx_engine_attestation_seconds_bucket{{kind=\"{kind}\",phase=\"{phase}\",le=\"+Inf\"}} {cumulative}\n\
                 plurx_engine_attestation_seconds_sum{{kind=\"{kind}\",phase=\"{phase}\"}} {sum}\n\
                 plurx_engine_attestation_seconds_count{{kind=\"{kind}\",phase=\"{phase}\"}} {cumulative}\n"
            ));
        }
    }
    out
}

#[derive(Clone)]
struct FragmentIndexEngine {
    digest: String,
    objects: Arc<[(std::path::PathBuf, String)]>,
    usable: bool,
}

/// The complete process-local renderer identity retained by an encoded VOD
/// recipe. The random process component deliberately prevents durable cache
/// reuse across nodes or daemon restarts: hardware/driver behavior cannot be
/// proven byte-identical merely because the selected encoder has the same
/// name. Within one daemon, every loaded dependency and (for text burn) every
/// object of the recipe's frozen Fontconfig environment is still rechecked
/// before spawning and publication.
#[cfg(target_os = "macos")]
async fn clock_package_bytes(
    path: std::path::PathBuf,
) -> Result<(Vec<u8>, EncodedExecutable), String> {
    let metadata = tokio::fs::symlink_metadata(&path)
        .await
        .map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return Err("clock provenance is not a bounded regular file".into());
    }
    let object = EncodedExecutable::capture_at(path.clone()).await?;
    let bytes = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > 1024 * 1024 {
            return Err("clock provenance exceeds its bound".to_owned());
        }
        Ok(bytes)
    })
    .await
    .map_err(|error| error.to_string())??;
    if hex::encode(Sha256::digest(&bytes)) != object.digest || !object.is_current().await {
        return Err("clock package provenance changed during capture".into());
    }
    Ok((bytes, object))
}

#[cfg(target_os = "macos")]
async fn clock_package_document(
    path: std::path::PathBuf,
) -> Result<(serde_json::Value, EncodedExecutable), String> {
    let (bytes, object) = clock_package_bytes(path).await?;
    let document =
        serde_json::from_slice(&bytes).map_err(|_| "clock package provenance malformed")?;
    Ok((document, object))
}

#[derive(Debug, Clone)]
pub(crate) struct EncodedEngine {
    pub digest: String,
    process_identity: String,
    /// The encoder dependency closure. Statted under `kind="media"`.
    objects: Arc<[(std::path::PathBuf, String)]>,
    /// The frozen Fontconfig environment a text burn renders under, kept
    /// apart from `objects` so its cost is charged under `kind="font"`.
    /// Empty for every recipe that does not burn text.
    font_objects: Arc<[(std::path::PathBuf, String)]>,
    /// The environment itself: the producer reads only its copies and links,
    /// and the last recipe holding it removes it.
    font_env: Option<Arc<crate::fontenv::FontEnvironment>>,
}

impl EncodedEngine {
    /// A deployed Mac parser and producer share the validated package's demux
    /// source/quilt. Legacy pairs retain their exact reporter identity without
    /// claiming a matched-source package. This adds objects to the existing
    /// production/publication fence, not a separate package registry.
    pub(crate) async fn bind_clock_pair(
        &mut self,
        reporter: &EncodedExecutable,
        producer: &EncodedExecutable,
    ) -> Result<(), String> {
        self.bind_clock_reporter(reporter).await?;
        #[cfg(target_os = "macos")]
        {
            let root = producer
                .path
                .parent()
                .ok_or("clock producer package parent missing")?;
            let native_path = root.join("provenance/manifest.json");
            let parser_path = root.join("provenance/source-parser/manifest.json");
            if !native_path.exists() && !parser_path.exists() {
                return Ok(()); // exact legacy reporter, no matched-source claim
            }
            // An official baseline with no sealed companion remains a legacy
            // reporter tuple; a present module cannot downgrade if its manifest
            // is removed.
            if native_path.exists()
                && !parser_path.exists()
                && !root.join("plurx-source-parser.wasm").exists()
            {
                return Ok(());
            }
            let (native, native_object) = clock_package_document(native_path).await?;
            let (parser, parser_object) = clock_package_document(parser_path).await?;
            const SOURCE: &str = "87bedcefc860cd234cdeddbebdcd3ce45aeedc81d0fbb258ed839ff778be3140";
            const COMMIT: &str = "253db2a7b0a8045c54ce68ce33d7f601229b1822";
            for document in [&native, &parser] {
                if document["source_sha256"].as_str() != Some(SOURCE)
                    || document["source_commit"].as_str() != Some(COMMIT)
                {
                    return Err("clock parser and producer source provenance differ".into());
                }
            }
            let quilt = parser["source_patch_sha256"]
                .as_object()
                .ok_or("clock parser quilt provenance missing")?;
            if quilt.len() != 100
                || quilt.iter().any(|(name, digest)| {
                    native["source_recipe_sha256"][format!("debian/patches/{name}")] != *digest
                })
            {
                return Err("clock parser and producer quilt provenance differ".into());
            }
            let native_probe = EncodedExecutable::capture_at(root.join("ffprobe")).await?;
            let module =
                EncodedExecutable::capture_at(root.join("plurx-source-parser.wasm")).await?;
            if native["binaries"]["ffmpeg"]["sha256"].as_str() != Some(producer.digest.as_str())
                || native["binaries"]["ffprobe"]["sha256"].as_str()
                    != Some(native_probe.digest.as_str())
                || parser["module_sha256"].as_str() != Some(module.digest.as_str())
                || native["source_parser"]["sha256"].as_str() != Some(module.digest.as_str())
                || (reporter.digest != native_probe.digest && reporter.digest != module.digest)
            {
                return Err(
                    "clock reporter/producer images differ from their matched package".into(),
                );
            }
            // Local additions may alter decoding/rendering, never the demux
            // origin semantics this package association authorizes.
            if let Some(patches) = native["compiled_source"]["local_patches"].as_object() {
                for (name, expected) in patches {
                    let relative = std::path::Path::new(name);
                    if relative.components().count() != 1 {
                        return Err("clock package local patch name is unsafe".into());
                    }
                    let (patch, object) =
                        clock_package_bytes(root.join("provenance/local-patches").join(relative))
                            .await?;
                    if expected.as_str() != Some(object.digest.as_str()) {
                        return Err("clock package local patch bytes differ".into());
                    }
                    let text =
                        std::str::from_utf8(&patch).map_err(|_| "clock patch is not text")?;
                    if text
                        .lines()
                        .filter_map(|line| line.strip_prefix("+++ "))
                        .any(|path| {
                            path != "/dev/null"
                                && !path.strip_prefix("b/").is_some_and(|path| {
                                    path.starts_with("libavcodec/")
                                        || path.starts_with("libavfilter/")
                                        || path.starts_with("libavutil/")
                                        || path == "configure"
                                })
                        })
                    {
                        return Err(
                            "clock local patch changes the parser/producer timing contract".into(),
                        );
                    }
                    self.bind_clock_reporter(&object).await?;
                }
            }
            for object in [&native_object, &parser_object, &native_probe, &module] {
                self.bind_clock_reporter(object).await?;
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(association) =
            producer.validate_source_clock_reporter(&reporter.path, &reporter.digest)?
        {
            if !producer.is_current().await {
                return Err("held Linux source-clock package changed".into());
            }
            let mut hash = Sha256::new();
            hash.update(b"plurx/held-source-clock-linux-package/v1\0");
            hash.update(self.digest.as_bytes());
            hash.update(association.capsule_digest().as_bytes());
            hash.update(association.source_digest().as_bytes());
            hash.update(reporter.digest.as_bytes());
            self.digest = hex::encode(hash.finalize());
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = producer; // exact reporter; no matched-source package claim
        Ok(())
    }

    /// The held timing witness names its actual reporter, not the stored scan.
    /// Reuse the media object fence for both native and sealed parser images.
    pub(crate) async fn bind_clock_reporter(
        &mut self,
        reporter: &EncodedExecutable,
    ) -> Result<(), String> {
        if !reporter.is_current().await {
            return Err("held source clock reporter changed".into());
        }
        let mut hash = Sha256::new();
        hash.update(b"plurx/held-source-clock-reporter/v1\0");
        hash.update(self.digest.as_bytes());
        hash.update(reporter.digest.as_bytes());
        self.digest = hex::encode(hash.finalize());
        let mut objects = self.objects.to_vec();
        objects.push(reporter.attestation_object());
        self.objects = objects.into();
        Ok(())
    }

    /// `text_burn` is the runtime cache a text burn's frozen Fontconfig
    /// environment is built under, or `None` for a recipe that burns no text.
    pub async fn capture(text_burn: Option<&std::path::Path>) -> Result<Self, String> {
        Self::capture_from(text_burn, None).await
    }

    /// `source_config` replaces the daemon's Fontconfig configuration for the
    /// live enumeration only; tests use it to stand up a known closure.
    pub(crate) async fn capture_from(
        text_burn: Option<&std::path::Path>,
        source_config: Option<&std::path::Path>,
    ) -> Result<Self, String> {
        let media = FRAGMENT_INDEX_ENGINE
            .get_or_init(fragment_index_engine_inner)
            .await;
        Self::capture_with_media(text_burn, source_config, media, None).await
    }

    /// The Source's capture: every child — the encoder's own version and
    /// dependency queries and, for a text burn, the Fontconfig enumeration
    /// and the frozen environment's proofs — runs as this Source operation's
    /// owned work, under its permit, deadline and cancellation. The frozen
    /// environment itself is the reference-counted directory the engine
    /// holds, so the last recipe holding it (the Source rendition, released
    /// at settlement) removes it.
    pub(crate) async fn capture_source(
        execution: &crate::transcode::source_preparation::SourceCommandExecutor<'_>,
        text_burn: Option<&std::path::Path>,
    ) -> Result<(Self, String), String> {
        // A Source cancellation/refusal cannot poison Local's cached snapshot.
        let (media, build) = fragment_index_engine_with_source(Some(execution)).await;
        Ok((
            Self::capture_with_media(text_burn, None, &media, Some(execution)).await?,
            build,
        ))
    }

    async fn capture_with_media(
        text_burn: Option<&std::path::Path>,
        source_config: Option<&std::path::Path>,
        media: &FragmentIndexEngine,
        execution: Option<&crate::transcode::source_preparation::SourceCommandExecutor<'_>>,
    ) -> Result<Self, String> {
        let mut charges = EngineAttestationCharges::default();
        if !media.usable {
            return Err("the encoder dependency closure could not be attested".to_owned());
        }
        let (media_current, media_elapsed) =
            engine_objects_are_current_batch(None, Arc::clone(&media.objects)).await;
        charges.add(
            EngineAttestationKind::Media,
            EngineAttestationPhase::Stat,
            media_elapsed,
        );
        if !media_current {
            charges.flush();
            return Err("the encoder dependency closure could not be attested".to_owned());
        }
        let process = encoded_process_identity();
        let mut digest = Sha256::new();
        digest.update(b"plurx/encoded-vod/engine-v1\0");
        digest.update(media.digest.as_bytes());
        digest.update(process.as_bytes());
        let mut objects = media.objects.to_vec();
        let mut font_objects: Vec<(std::path::PathBuf, String)> = Vec::new();
        let mut font_env = None;
        if let Some(root) = text_burn {
            // Fontconfig's closure is live configuration, unlike the process's
            // loaded media libraries. Probe it for every recipe capture so a
            // newly installed font or rule cannot reuse the old URI identity,
            // then freeze what it found so the producer can see nothing else.
            let enumeration = match font_render_engine_inner(source_config, execution, root).await {
                Ok(enumeration) => enumeration,
                Err(error) => {
                    charges.flush();
                    return Err(error);
                }
            };
            charges.add(
                EngineAttestationKind::Font,
                EngineAttestationPhase::Spawn,
                enumeration.spawn_elapsed,
            );
            charges.add(
                EngineAttestationKind::Font,
                EngineAttestationPhase::Stat,
                enumeration.stat_elapsed,
            );
            let fonts = &enumeration.engine;
            let attested = if fonts.usable {
                let (current, elapsed) =
                    engine_objects_are_current_batch(None, Arc::clone(&fonts.objects)).await;
                charges.add(
                    EngineAttestationKind::Font,
                    EngineAttestationPhase::Stat,
                    elapsed,
                );
                current
            } else {
                false
            };
            if !attested {
                charges.flush();
                return Err(
                    "Fontconfig rules and resolved font files could not be attested".to_owned(),
                );
            }
            let versions: std::collections::HashMap<_, _> = fonts.objects.iter().cloned().collect();
            let (frozen, cost) = crate::fontenv::freeze(
                root,
                crate::fontenv::FontSources {
                    digest: &fonts.digest,
                    rules: &enumeration.rules,
                    fonts: &enumeration.fonts,
                    versions: &versions,
                    listing: &enumeration.listing,
                    search_path: enumeration.search_path.as_deref(),
                },
                execution,
            )
            .await;
            charges.add(
                EngineAttestationKind::Font,
                EngineAttestationPhase::Spawn,
                cost.spawn,
            );
            charges.add(
                EngineAttestationKind::Font,
                EngineAttestationPhase::Stat,
                cost.stat,
            );
            let frozen = match frozen {
                Ok(frozen) => frozen,
                Err(error) => {
                    charges.flush();
                    return Err(format!(
                        "the text burn's Fontconfig environment could not be frozen: {error}"
                    ));
                }
            };
            digest.update(frozen.digest().as_bytes());
            font_objects = frozen.objects().to_vec();
            font_env = Some(frozen);
        }
        charges.flush();
        objects.sort_by(|left, right| left.0.cmp(&right.0));
        objects.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
        font_objects.sort_by(|left, right| left.0.cmp(&right.0));
        font_objects.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
        Ok(Self {
            digest: hex::encode(digest.finalize()),
            process_identity: process.to_owned(),
            objects: objects.into(),
            font_objects: font_objects.into(),
            font_env,
        })
    }

    /// Re-attest every input which can change the bytes emitted under this
    /// recipe. A text burn is not re-enumerated: its producer reads only the
    /// frozen environment, so a font or rule installed since capture cannot
    /// reach it, and a replaced, removed or edited object of the environment
    /// changes a captured version.
    #[cfg(test)]
    pub async fn is_current(&self) -> bool {
        self.is_current_charged(None).await.0
    }

    /// Attest the recipe's encoder executable in the same blocking batch as
    /// the dependency closure it belongs to.
    ///
    /// The executable used to be statted inline by the caller, ahead of this
    /// batch and short-circuiting it, which left one synchronous
    /// `std::fs::metadata` on a runtime worker on every producer launch and
    /// every segment materialisation — for non-burn renditions as well as
    /// burn ones. Folding the pair in costs one `PathBuf`/`String` clone per
    /// check and leaves the whole attestation as one blocking task. The
    /// answer is unchanged: the batch compares versions with `all`, which
    /// short-circuits on the executable exactly as the caller's `||` did.
    pub async fn is_current_with_executable(&self, executable: &EncodedExecutable) -> bool {
        let object = Some(executable.attestation_object());
        if executable.linux_binding.is_none() {
            return self.is_current_charged(object).await.0;
        }
        self.is_current_charged_with_linux(object, executable.linux_binding.clone())
            .await
            .0
    }

    /// The answer plus what it charged the histogram. Tests read the charges
    /// directly so the series a check attributes its cost to can be pinned
    /// without racing other tests on the process-wide counters.
    async fn is_current_charged(
        &self,
        executable: Option<(std::path::PathBuf, String)>,
    ) -> (bool, EngineAttestationCharges) {
        self.is_current_charged_with_linux(executable, None).await
    }

    async fn is_current_charged_with_linux(
        &self,
        executable: Option<(std::path::PathBuf, String)>,
        linux_binding: Option<plurx_core::transcode::LinuxDolbyExecutionBinding>,
    ) -> (bool, EngineAttestationCharges) {
        let mut charges = EngineAttestationCharges::default();
        let (media_current, media_elapsed) = engine_objects_are_current_batch_with_linux(
            executable,
            Arc::clone(&self.objects),
            linux_binding,
        )
        .await;
        charges.add(
            EngineAttestationKind::Media,
            EngineAttestationPhase::Stat,
            media_elapsed,
        );
        if !media_current {
            charges.flush();
            return (false, charges);
        }
        if self.font_env.is_none() {
            charges.flush();
            return (true, charges);
        }

        // One stat batch and no child: the frozen environment cannot gain
        // inputs, so there is nothing for `fc-list` to find.
        let (fonts_current, fonts_elapsed) =
            engine_objects_are_current_batch(None, Arc::clone(&self.font_objects)).await;
        charges.add(
            EngineAttestationKind::Font,
            EngineAttestationPhase::Stat,
            fonts_elapsed,
        );
        charges.flush();
        (fonts_current, charges)
    }

    /// What a producer running this recipe adds to its environment: a text
    /// burn names its frozen Fontconfig sysroot and root configuration, so
    /// libass resolves fonts from the captured environment and nothing
    /// else. Child-local, like `configure_ffmpeg_runtime`'s cache; the
    /// daemon's own environment and every other child are untouched.
    pub(crate) fn child_env(&self) -> Vec<(&'static str, &std::ffi::OsStr)> {
        self.font_env
            .iter()
            .flat_map(|environment| environment.child_env())
            .collect()
    }

    pub(crate) fn font_digest(&self) -> Option<&str> {
        self.font_env
            .as_ref()
            .map(|environment| environment.digest())
    }

    #[cfg(test)]
    pub(crate) fn font_environment(&self) -> Option<&Arc<crate::fontenv::FontEnvironment>> {
        self.font_env.as_ref()
    }

    #[cfg(test)]
    pub(crate) async fn is_current_charged_for_test(
        &self,
    ) -> (bool, Vec<(&'static str, &'static str, usize)>) {
        let (current, charges) = self.is_current_charged(None).await;
        (current, charges.charged())
    }

    pub fn process_identity(&self) -> &str {
        &self.process_identity
    }

    #[cfg(test)]
    pub(crate) async fn capture_test_objects(
        paths: &[std::path::PathBuf],
        process: &str,
    ) -> Result<Self, String> {
        let mut digest = Sha256::new();
        digest.update(b"plurx/encoded-vod/test-engine\0");
        digest.update(process.as_bytes());
        let mut objects = Vec::new();
        for path in paths {
            let (object_digest, version) = hash_engine_object(path).await?;
            objects.push((
                path.clone(),
                pinned_engine_object_version(&version, &hex::encode(&object_digest)),
            ));
            digest.update(object_digest);
        }
        Ok(Self {
            digest: hex::encode(digest.finalize()),
            process_identity: process.to_owned(),
            objects: objects.into(),
            font_objects: Vec::new().into(),
            font_env: None,
        })
    }
}

/// A process generation is intentionally part of every encoded rendition key.
/// VOD serving also persists this value beside encoded media so startup can
/// distinguish obsolete generations from copy renditions without guessing
/// from directory names.
pub(crate) fn encoded_process_identity() -> &'static str {
    ENCODED_PROCESS_IDENTITY
        .get_or_init(|| uuid::Uuid::new_v4().to_string())
        .as_str()
}

/// Digest the executable bytes and its complete self/dependency reports once
/// per daemon. Fragment indexes compare copied sample sizes, so two nominally
/// equal ffmpeg versions are not interchangeable unless the actual engine is.
pub async fn fragment_index_engine_digest() -> String {
    FRAGMENT_INDEX_ENGINE
        .get_or_init(fragment_index_engine_inner)
        .await
        .digest
        .clone()
}

/// Fail closed if the configured executable or any loaded dependency has
/// changed since the daemon established the cache-key digest. Callers check
/// both before spawning and before publication, so an in-place engine upgrade
/// cannot emit bytes under the retired identity; restart establishes a new
/// digest and keyspace.
pub async fn fragment_index_engine_is_current() -> bool {
    let engine = FRAGMENT_INDEX_ENGINE
        .get_or_init(fragment_index_engine_inner)
        .await;
    fragment_index_engine_snapshot_is_current(engine).await
}

async fn fragment_index_engine_snapshot_is_current(engine: &FragmentIndexEngine) -> bool {
    if !engine.usable {
        return false;
    }
    let (current, elapsed) =
        engine_objects_are_current_batch(None, Arc::clone(&engine.objects)).await;
    observe_engine_attestation(
        EngineAttestationKind::Media,
        EngineAttestationPhase::Stat,
        elapsed,
    );
    current
}

fn pinned_engine_object_version(version: &str, digest: &str) -> String {
    format!("{version}|sha256:{digest}")
}

#[cfg(unix)]
fn engine_metadata_has_settled(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(seconds), Ok(nanoseconds)) = (
        u64::try_from(metadata.ctime()),
        u32::try_from(metadata.ctime_nsec()),
    ) else {
        return false;
    };
    if nanoseconds >= 1_000_000_000 {
        return false;
    }
    std::time::SystemTime::UNIX_EPOCH
        .checked_add(Duration::new(seconds, nanoseconds))
        .and_then(|stamp| std::time::SystemTime::now().duration_since(stamp).ok())
        .is_some_and(|age| age >= Duration::from_secs(2))
}

#[cfg(not(unix))]
fn engine_metadata_has_settled(_: &std::fs::Metadata) -> bool {
    true
}

fn engine_object_is_current(path: &std::path::Path, expected: &str) -> bool {
    let (version, digest) = expected
        .split_once("|sha256:")
        .map_or((expected, None), |(version, digest)| {
            (version, Some(digest))
        });
    if engine_path_version(path).ok().as_deref() != Some(version) {
        return false;
    }
    let Some(expected_digest) = digest else {
        return true;
    };
    if let Some(cached) = cached_engine_digest(path, version) {
        return hex::encode(cached) == expected_digest;
    }
    // Timestamp updates can be coalesced for a newly written object. Until
    // a hash was taken outside that window, publication checks its pinned
    // bytes as well as metadata. Installed, settled binaries keep the cheap
    // metadata-only path.
    let verify = || -> Option<bool> {
        use std::io::Read;
        let mut file = std::fs::File::open(path).ok()?;
        let before = file.metadata().ok()?;
        let settled_before_read = engine_metadata_has_settled(&before);
        if before.len() > ENGINE_OBJECT_MAX_BYTES {
            return Some(false);
        }
        let mut hash = Sha256::new();
        let mut buffer = [0_u8; 256 * 1024];
        let mut count = 0_u64;
        loop {
            let read = file.read(&mut buffer).ok()?;
            if read == 0 {
                break;
            }
            count += read as u64;
            if count > ENGINE_OBJECT_MAX_BYTES {
                return Some(false);
            }
            hash.update(&buffer[..read]);
        }
        let after = file.metadata().ok()?;
        if engine_path_version(path).ok().as_deref() != Some(version) {
            return Some(false);
        }
        #[cfg(unix)]
        if engine_object_version(&after).ok().as_deref() != Some(version) {
            return Some(false);
        }
        let actual = hash.finalize().to_vec();
        if hex::encode(&actual) != expected_digest {
            return Some(false);
        }
        remember_engine_digest(path, version, &actual, settled_before_read);
        Some(true)
    };
    verify().unwrap_or(false)
}

fn engine_objects_are_current(objects: &[(std::path::PathBuf, String)]) -> bool {
    objects
        .iter()
        .all(|(path, expected)| engine_object_is_current(path, expected))
}

/// Run the same fail-closed identity comparison away from runtime workers.
/// A cold or remote dependency can make `metadata` block, and this check is
/// paid before every encoded segment is published.
///
/// `extra` is an object that is not part of the captured list — today the
/// recipe's encoder executable — checked first and inside the same blocking
/// task, so no caller has to stat it on a runtime worker. It reports its
/// elapsed time rather than observing, because one attestation is several of
/// these batches and the histogram is charged once for the whole check.
pub(crate) async fn engine_objects_are_current_batch(
    extra: Option<(std::path::PathBuf, String)>,
    objects: Arc<[(std::path::PathBuf, String)]>,
) -> (bool, Duration) {
    engine_objects_are_current_batch_with_linux(extra, objects, None).await
}

async fn engine_objects_are_current_batch_with_linux(
    extra: Option<(std::path::PathBuf, String)>,
    objects: Arc<[(std::path::PathBuf, String)]>,
    linux_binding: Option<plurx_core::transcode::LinuxDolbyExecutionBinding>,
) -> (bool, Duration) {
    let started = Instant::now();
    let current = tokio::task::spawn_blocking(move || {
        if let Some((path, expected)) = extra {
            if !engine_object_is_current(&path, &expected) {
                return false;
            }
        }
        engine_objects_are_current(&objects)
            && linux_binding.as_ref().is_none_or(|binding| {
                binding
                    .objects()
                    .iter()
                    .all(linux_execution_object_is_current)
            })
    })
    .await
    .unwrap_or(false);
    (current, started.elapsed())
}

fn linux_execution_object_is_current(object: &plurx_core::transcode::LinuxExecutionObject) -> bool {
    use plurx_core::transcode::LinuxExecutionObjectKind;
    match object.kind {
        LinuxExecutionObjectKind::AbsentLookup => std::fs::symlink_metadata(&object.path)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
        LinuxExecutionObjectKind::ProducerExecutable
        | LinuxExecutionObjectKind::NativeReporterExecutable
        | LinuxExecutionObjectKind::SealedParserExecutable
        | LinuxExecutionObjectKind::FollowedFile => {
            engine_path_version(&object.path).ok().as_ref() == object.version.as_ref()
        }
        LinuxExecutionObjectKind::ContentDigest => {
            use std::io::Read;
            std::fs::File::open(&object.path).ok().is_some_and(|file| {
                let mut bytes = Vec::new();
                file.take(256 * 1024 + 1).read_to_end(&mut bytes).is_ok()
                    && bytes.len() <= 256 * 1024
                    && object
                        .version
                        .as_ref()
                        .is_some_and(|expected| hex::encode(Sha256::digest(&bytes)) == *expected)
            })
        }
        LinuxExecutionObjectKind::LookupObject => {
            #[cfg(unix)]
            {
                std::fs::symlink_metadata(&object.path)
                    .ok()
                    .and_then(|metadata| engine_object_version(&metadata).ok())
                    .as_ref()
                    == object.version.as_ref()
            }
            #[cfg(not(unix))]
            {
                false
            }
        }
    }
}

async fn fragment_index_engine_inner() -> FragmentIndexEngine {
    fragment_index_engine_with_source(None).await.0
}
async fn fragment_index_engine_with_source(
    execution: Option<&crate::transcode::source_preparation::SourceCommandExecutor<'_>>,
) -> (FragmentIndexEngine, String) {
    let probe_started = Instant::now();
    let bin = ffmpeg_bin();
    let resolved = resolve_executable_path(&bin);
    let mut digest = Sha256::new();
    digest.update(b"plurx/fragment-index/engine\0");
    let mut usable = true;
    let mut objects = Vec::new();

    let version_output = {
        let mut command = tokio::process::Command::new(&bin);
        command.arg("-version");
        match execution {
            Some(execution) => execution.output(command, 1024 * 1024).await,
            None => bounded_command_output(command).await,
        }
    };
    let build = version_output
        .as_ref()
        .ok()
        .and_then(|output| std::str::from_utf8(&output.stdout).ok())
        .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
        .map_or_else(
            || format!("{bin} (version unavailable)"),
            |line| format!("{bin} ({line})"),
        );
    match version_output {
        Ok(output) => {
            digest.update((output.stdout.len() as u64).to_be_bytes());
            digest.update(output.stdout);
            digest.update((output.stderr.len() as u64).to_be_bytes());
            digest.update(output.stderr);
        }
        Err(error) => {
            usable = false;
            digest.update(error.as_bytes());
        }
    }

    #[cfg(target_os = "linux")]
    let dependency_probe = resolved.as_ref().map(|path| ("ldd", path));
    #[cfg(target_os = "macos")]
    let dependency_probe = resolved.as_ref().map(|path| ("otool", path));
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let dependency_probe: Option<(&str, &std::path::PathBuf)> = None;
    let mut dependency_paths = Vec::new();
    if let Some((tool, path)) = dependency_probe {
        let mut command = tokio::process::Command::new(tool);
        #[cfg(target_os = "macos")]
        command.arg("-L");
        command.arg(path);
        let output = match execution {
            Some(execution) => execution.output(command, 1024 * 1024).await,
            None => bounded_command_output(command).await,
        };
        match output {
            Ok(output) => {
                let stdout = normalized_dependency_report(&output.stdout);
                match dependency_paths_from_report(&stdout) {
                    Ok(paths) => dependency_paths = paths,
                    Err(error) => {
                        usable = false;
                        digest.update(error.as_bytes());
                    }
                }
            }
            Err(error) => {
                usable = false;
                digest.update(error.as_bytes());
            }
        }
    } else {
        usable = false;
    }
    observe_engine_attestation(
        EngineAttestationKind::Media,
        EngineAttestationPhase::Spawn,
        probe_started.elapsed(),
    );

    if let Some(path) = resolved {
        dependency_paths.push(path);
    } else {
        usable = false;
    }
    dependency_paths.sort();
    dependency_paths.dedup();
    let mut object_digests = Vec::new();
    for path in dependency_paths {
        match hash_engine_object(&path).await {
            Ok((object_digest, version)) => {
                objects.push((
                    path,
                    pinned_engine_object_version(&version, &hex::encode(&object_digest)),
                ));
                object_digests.push(object_digest);
            }
            Err(error) => {
                #[cfg(target_os = "macos")]
                if dyld_shared_cache_path(&path) {
                    // Current macOS stores system dylibs in the dyld shared
                    // cache rather than at the install names printed by
                    // `otool`. Those bytes cannot be opened individually;
                    // the install name and reported dylib version are already
                    // in the normalized dependency report, while the encoded
                    // identity is additionally process-local so an OS update
                    // can never resurrect this key after restart.
                    digest.update(b"dyld-shared-cache\0");
                    digest.update(path.as_os_str().as_encoded_bytes());
                    continue;
                }
                #[cfg(not(target_os = "macos"))]
                let _ = &path;
                usable = false;
                digest.update(error.as_bytes());
            }
        }
    }
    object_digests.sort();
    for object_digest in object_digests {
        digest.update((object_digest.len() as u64).to_be_bytes());
        digest.update(object_digest);
    }
    (
        FragmentIndexEngine {
            digest: hex::encode(digest.finalize()),
            objects: objects.into(),
            usable,
        },
        build,
    )
}

/// A Fontconfig enumeration with the two costs it incurred kept apart: the
/// `fc-list`/`fc-conflist` children and the stat loop over what they named.
/// The caller charges each series once for the whole attestation.
struct FontEnumeration {
    search_path: Option<std::ffi::OsString>,
    engine: FragmentIndexEngine,
    spawn_elapsed: Duration,
    stat_elapsed: Duration,
    /// Every loaded configuration file, in the order `fc-conflist` reports
    /// it: the order each *finished* loading, root last. That is not the
    /// order its rules apply (see `fontenv`), so the freeze only compares it.
    rules: Vec<std::path::PathBuf>,
    /// Every font file `fc-list` named, sorted and unique.
    fonts: Vec<std::path::PathBuf>,
    /// The raw `fc-list` answer, one line per face.
    listing: String,
}

/// A Fontconfig tool run under the font probe budget, answering its stdout.
pub(crate) async fn font_probe_output(
    command: tokio::process::Command,
    execution: Option<&crate::transcode::source_preparation::SourceCommandExecutor<'_>>,
) -> Result<Vec<u8>, String> {
    match execution {
        // A Source capture's Fontconfig child is that Source operation's own
        // owned work, never an unowned capability probe.
        Some(execution) => execution
            .output(command, ENGINE_PROBE_MAX_BYTES as usize)
            .await
            .map(|output| output.stdout),
        None => bounded_command_output_with_timeout(command, FONT_ENGINE_PROBE_TIMEOUT)
            .await
            .map(|output| output.stdout),
    }
}

async fn font_render_engine_inner(
    source_config: Option<&std::path::Path>,
    execution: Option<&crate::transcode::source_preparation::SourceCommandExecutor<'_>>,
    runtime_cache: &std::path::Path,
) -> Result<FontEnumeration, String> {
    let probe_started = Instant::now();
    let mut digest = Sha256::new();
    digest.update(b"plurx/font-render/engine-v1\0");
    let mut usable = true;
    let mut paths = Vec::new();
    let mut fonts = Vec::new();
    let mut rules: Vec<std::path::PathBuf> = Vec::new();
    let mut listing = String::new();

    let live = crate::fontenv::live_config(source_config)?;
    let list_bin = crate::fontenv::tool_bin("fc-list");
    let conflist_bin = crate::fontenv::tool_bin("fc-conflist");
    let mut font_list = tokio::process::Command::new(&list_bin);
    font_list.arg(crate::fontenv::FONT_LISTING_FORMAT);
    let mut configuration = tokio::process::Command::new(&conflist_bin);
    live.apply(&mut font_list);
    live.apply(&mut configuration);
    if cfg!(target_os = "macos") {
        crate::producer_spawn::configure_ffmpeg_runtime(&mut font_list, runtime_cache);
        crate::producer_spawn::configure_ffmpeg_runtime(&mut configuration, runtime_cache);
        // The exact bundled query implementations are part of the same font
        // authority as the rule and face bytes, not an incidental PATH tool.
        for bin in [&list_bin, &conflist_bin] {
            if std::path::Path::new(bin).is_absolute() {
                paths.push(std::path::PathBuf::from(bin));
            }
        }
    }
    match font_probe_output(font_list, execution).await {
        Ok(stdout) => {
            digest.update((stdout.len() as u64).to_be_bytes());
            digest.update(&stdout);
            listing = String::from_utf8_lossy(&stdout).into_owned();
            fonts.extend(
                listing
                    .lines()
                    .filter_map(|line| line.split('\t').next())
                    .filter(|path| path.starts_with('/'))
                    .map(std::path::PathBuf::from),
            );
            fonts.sort();
            fonts.dedup();
            paths.extend(fonts.iter().cloned());
        }
        Err(error) => {
            usable = false;
            digest.update(error.as_bytes());
        }
    }

    match font_probe_output(configuration, execution).await {
        Ok(stdout) => {
            // fc-conflist also reports available but inactive rules. Those
            // files are not consumed by this authority and may appear when a
            // build prefix or system inventory changes. Bind only the ordered
            // active rows; their paths and versions remain attested below.
            hash_active_font_configuration(&mut digest, &stdout);
            for rule in String::from_utf8_lossy(&stdout)
                .lines()
                .filter_map(|line| line.strip_prefix("+ "))
                .filter_map(|line| line.split_once(": ").map(|(path, _)| path))
                .filter(|path| path.starts_with('/'))
                .map(std::path::PathBuf::from)
            {
                if !rules.contains(&rule) {
                    rules.push(rule);
                }
            }
            paths.extend(rules.iter().cloned());
        }
        Err(error) => {
            usable = false;
            digest.update(error.as_bytes());
        }
    }
    let spawn_elapsed = probe_started.elapsed();

    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        usable = false;
        digest.update(b"no font inputs discovered");
    }
    let (versions, stat_elapsed) = font_object_versions(paths).await;
    let objects = versions.objects;
    let mut object_digests = versions.object_digests;
    for error in versions.errors {
        usable = false;
        digest.update(error.as_bytes());
    }
    object_digests.sort();
    for object_digest in object_digests {
        digest.update((object_digest.len() as u64).to_be_bytes());
        digest.update(object_digest);
    }
    Ok(FontEnumeration {
        search_path: live.search,
        engine: FragmentIndexEngine {
            digest: hex::encode(digest.finalize()),
            objects: objects.into(),
            usable,
        },
        spawn_elapsed,
        stat_elapsed,
        rules,
        fonts,
        listing,
    })
}

fn hash_active_font_configuration(digest: &mut Sha256, stdout: &[u8]) {
    digest.update(b"plurx/font-render/active-configuration-v1\0");
    for line in stdout.split(|byte| *byte == b'\n') {
        if line.starts_with(b"+ ") {
            digest.update((line.len() as u64).to_be_bytes());
            digest.update(line);
        }
    }
}

struct FontObjectVersions {
    objects: Vec<(std::path::PathBuf, String)>,
    object_digests: Vec<Vec<u8>>,
    errors: Vec<String>,
}

async fn font_object_versions(paths: Vec<std::path::PathBuf>) -> (FontObjectVersions, Duration) {
    font_object_versions_observed(paths, || {}).await
}

/// Reports its elapsed time rather than observing it: it is one batch inside
/// a larger attestation, and the histogram is charged per attestation.
async fn font_object_versions_observed<F>(
    paths: Vec<std::path::PathBuf>,
    observe_task: F,
) -> (FontObjectVersions, Duration)
where
    F: FnOnce() + Send + 'static,
{
    let started = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        observe_task();
        let mut objects = Vec::new();
        let mut object_digests = Vec::new();
        let mut errors = Vec::new();
        for path in paths {
            match engine_path_version(&path) {
                Ok(version) => {
                    let mut identity = Sha256::new();
                    identity.update(path.as_os_str().as_encoded_bytes());
                    identity.update(version.as_bytes());
                    object_digests.push(identity.finalize().to_vec());
                    objects.push((path, version));
                }
                Err(error) => errors.push(error),
            }
        }
        FontObjectVersions {
            objects,
            object_digests,
            errors,
        }
    })
    .await
    .unwrap_or_else(|error| FontObjectVersions {
        objects: Vec::new(),
        object_digests: Vec::new(),
        errors: vec![format!("font object stat task failed: {error}")],
    });
    (result, started.elapsed())
}

pub(crate) struct BoundedOutput {
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// Every engine probe is a capability probe nobody is waiting on.
const ENGINE_PROBE: crate::process_control::ChildWork =
    crate::process_control::ChildWork::background("engine capability probe");

async fn bounded_command_output(command: tokio::process::Command) -> Result<BoundedOutput, String> {
    bounded_command_output_with_timeout(command, ENGINE_PROBE_TIMEOUT).await
}

async fn bounded_command_output_with_timeout(
    command: tokio::process::Command,
    timeout: Duration,
) -> Result<BoundedOutput, String> {
    bounded_command_output_with_limits(
        command,
        timeout,
        ENGINE_PROBE_MAX_BYTES,
        "engine probe",
        ENGINE_PROBE,
    )
    .await
}

async fn bounded_command_output_with_limits(
    command: tokio::process::Command,
    timeout: Duration,
    max_bytes: u64,
    label: &'static str,
    work: crate::process_control::ChildWork,
) -> Result<BoundedOutput, String> {
    bounded_command_output_cancellable(command, timeout, max_bytes, label, None, work).await
}

/// Captured by the existing bounded child owner even on a nonzero exit.
/// Pipe overflow remains an error and cannot establish frame absence.
pub(crate) struct BoundedCommandOutcome {
    pub(crate) status: std::process::ExitStatus,
    pub(crate) output: Result<BoundedOutput, String>,
}

pub(crate) async fn bounded_command_output_cancellable(
    command: tokio::process::Command,
    timeout: Duration,
    max_bytes: u64,
    label: &'static str,
    cancel: Option<&tokio_util::sync::CancellationToken>,
    work: crate::process_control::ChildWork,
) -> Result<BoundedOutput, String> {
    let outcome =
        bounded_command_capture_cancellable(command, timeout, max_bytes, label, cancel, work)
            .await?;
    if !outcome.status.success() {
        return Err(format!("{label} exited {}", outcome.status));
    }
    outcome.output
}

pub(crate) async fn bounded_command_capture_cancellable(
    mut command: tokio::process::Command,
    timeout: Duration,
    max_bytes: u64,
    label: &'static str,
    cancel: Option<&tokio_util::sync::CancellationToken>,
    work: crate::process_control::ChildWork,
) -> Result<BoundedCommandOutcome, String> {
    if cancel.is_some_and(|token| token.is_cancelled()) {
        return Err(format!("{label} cancelled"));
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let (mut child, _child_job) = crate::process_control::spawn_job_owned(&mut command, work)
        .map_err(|error| error.to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{label} has no stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{label} has no stderr"))?;
    let stopped = {
        let collect = async {
            let (stdout, stderr, status) = tokio::join!(
                read_bounded_with_limit(stdout, max_bytes, label),
                read_bounded_with_limit(stderr, max_bytes, label),
                child.wait()
            );
            let status = status.map_err(|e| e.to_string())?;
            Ok(BoundedCommandOutcome {
                status,
                output: stdout
                    .and_then(|stdout| stderr.map(|stderr| BoundedOutput { stdout, stderr })),
            })
        };
        tokio::pin!(collect);
        tokio::select! {
            result = &mut collect => return result,
            () = tokio::time::sleep(timeout) => format!("{label} timed out after {} seconds", timeout.as_secs()),
            () = async { match cancel { Some(token) => token.cancelled().await, None => std::future::pending().await } } => format!("{label} cancelled"),
        }
    };
    let _ = child.start_kill();
    // Keep the caller's admission alive until the child is reaped.
    child
        .wait()
        .await
        .map_err(|error| format!("{stopped}; waiting for child: {error}"))?;
    Err(stopped)
}

#[cfg_attr(not(test), allow(dead_code))]
async fn read_bounded(input: impl AsyncRead + Unpin) -> Result<Vec<u8>, String> {
    read_bounded_with_limit(input, ENGINE_PROBE_MAX_BYTES, "engine probe").await
}

async fn read_bounded_with_limit(
    input: impl AsyncRead + Unpin,
    max_bytes: u64,
    label: &'static str,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    input
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > max_bytes {
        // Name the bound. The last time this fired it read as "could not probe
        // ffmpeg", which sent three people looking for a missing binary.
        return Err(format!(
            "{label} exceeded its output bound of {max_bytes} bytes"
        ));
    }
    Ok(bytes)
}

/// Digests of engine objects keyed by path and the cheap object version
/// (`engine_object_version`: device, inode, size, mtime and ctime). The same
/// version identity already decides whether a hash raced a replacement, so a
/// settled version can reuse the hash. A newly changed file is rehashed until
/// timestamp coalescing can no longer hide an immediate write. The key set is the
/// resolved encoder and its loaded dependencies, so it stays small.
/// Digest per engine object path, tagged with the object version it was taken at.
type EngineObjectDigests =
    std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, (String, Vec<u8>, bool)>>;
static ENGINE_OBJECT_DIGESTS: std::sync::LazyLock<EngineObjectDigests> =
    std::sync::LazyLock::new(Default::default);

fn cached_engine_digest(path: &std::path::Path, version: &str) -> Option<Vec<u8>> {
    ENGINE_OBJECT_DIGESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(path)
        .filter(|(cached, _, settled)| *settled && cached == version)
        .map(|(_, digest, _)| digest.clone())
}

fn remember_engine_digest(path: &std::path::Path, version: &str, digest: &[u8], settled: bool) {
    ENGINE_OBJECT_DIGESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            path.to_owned(),
            (version.to_owned(), digest.to_vec(), settled),
        );
}

/// SHA-256 of one engine object plus its object version. Only the first
/// capture of an object version reads the bytes; every start path captures
/// the executable several times, and hashing a ~100 MB binary each time cost
/// seconds of disk read per playback start.
async fn hash_engine_object(path: &std::path::Path) -> Result<(Vec<u8>, String), String> {
    #[cfg(unix)]
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|error| format!("stat {}: {error}", path.display()))?;
    #[cfg(windows)]
    let source = {
        let source_path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            plurx_core::fs_secure::open_read_nofollow_blocking(&source_path)
        })
        .await
        .map_err(|error| format!("engine open task failed: {error}"))?
        .map_err(|error| format!("open {}: {error}", path.display()))?
    };
    #[cfg(windows)]
    let metadata = source
        .metadata()
        .map_err(|error| format!("stat {}: {error}", path.display()))?;
    if metadata.len() > ENGINE_OBJECT_MAX_BYTES {
        return Err(format!(
            "engine object {} exceeds size bound",
            path.display()
        ));
    }
    #[cfg(unix)]
    let version = engine_object_version(&metadata)?;
    #[cfg(windows)]
    let version = windows_engine_object_version(&source)?;
    let settled_before_read = engine_metadata_has_settled(&metadata);
    if let Some(digest) = cached_engine_digest(path, &version) {
        return Ok((digest, version));
    }
    #[cfg(unix)]
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| format!("open {}: {error}", path.display()))?;
    #[cfg(windows)]
    let mut file = tokio::fs::File::from_std(
        source
            .try_clone()
            .map_err(|error| format!("clone engine object {}: {error}", path.display()))?,
    );
    let mut object = Sha256::new();
    let mut buffer = vec![0_u8; 256 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        object.update(&buffer[..read]);
    }
    #[cfg(unix)]
    let after = file
        .metadata()
        .await
        .map_err(|error| format!("re-stat {}: {error}", path.display()))?;
    #[cfg(unix)]
    let after_version = engine_object_version(&after)?;
    #[cfg(windows)]
    let after_version = windows_engine_object_version(&source)?;
    if after_version != version {
        return Err(format!(
            "engine object {} changed while hashing",
            path.display()
        ));
    }
    let digest = object.finalize().to_vec();
    remember_engine_digest(path, &version, &digest, settled_before_read);
    Ok((digest, version))
}

pub(crate) fn engine_path_version(path: &std::path::Path) -> Result<String, String> {
    #[cfg(unix)]
    {
        let metadata =
            std::fs::metadata(path).map_err(|error| format!("stat {}: {error}", path.display()))?;
        engine_object_version(&metadata)
    }
    #[cfg(windows)]
    {
        let file = plurx_core::fs_secure::open_read_nofollow_blocking(path)
            .map_err(|error| format!("open {}: {error}", path.display()))?;
        windows_engine_object_version(&file)
    }
}

#[cfg(unix)]
fn engine_object_version(metadata: &std::fs::Metadata) -> Result<String, String> {
    use std::os::unix::fs::MetadataExt;
    Ok(format!(
        "{}:{}:{}:{}:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.size(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    ))
}

#[cfg(windows)]
fn windows_engine_object_version(file: &std::fs::File) -> Result<String, String> {
    let identity =
        plurx_core::fs_secure::std_file_identity(file).map_err(|error| error.to_string())?;
    Ok(format!(
        "{}:{}:{}:{}:{}:{}",
        identity.device,
        identity.inode,
        identity.inode_high,
        identity.size,
        identity.changed_seconds,
        identity.changed_nanoseconds
    ))
}

#[cfg(target_os = "linux")]
fn dependency_paths_from_report(report: &[u8]) -> Result<Vec<std::path::PathBuf>, String> {
    let mut paths = Vec::new();
    for line in String::from_utf8_lossy(report).lines() {
        if line.contains("=> not found") {
            return Err(format!("unresolved ffmpeg dependency: {}", line.trim()));
        }
        let trimmed = line.trim();
        let candidate = trimmed
            .split_once("=>")
            .map(|(_, rest)| rest.trim())
            .unwrap_or(trimmed)
            .split_whitespace()
            .next()
            .unwrap_or_default();
        if candidate.starts_with('/') {
            paths.push(
                std::fs::canonicalize(candidate)
                    .map_err(|error| format!("resolve dependency {candidate}: {error}"))?,
            );
        }
    }
    Ok(paths)
}

#[cfg(target_os = "macos")]
fn dependency_paths_from_report(report: &[u8]) -> Result<Vec<std::path::PathBuf>, String> {
    String::from_utf8_lossy(report)
        .lines()
        .skip(1)
        .map(|line| {
            let candidate = line.split_whitespace().next().unwrap_or_default();
            if !candidate.starts_with('/') {
                return Err(format!("unresolved ffmpeg dependency: {candidate}"));
            }
            let path = std::path::PathBuf::from(candidate);
            if dyld_shared_cache_path(&path) {
                Ok(path)
            } else {
                std::fs::canonicalize(candidate)
                    .map_err(|error| format!("resolve dependency {candidate}: {error}"))
            }
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn dyld_shared_cache_path(path: &std::path::Path) -> bool {
    path.starts_with("/System/Library/") || path.starts_with("/usr/lib/")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn dependency_paths_from_report(_report: &[u8]) -> Result<Vec<std::path::PathBuf>, String> {
    Err("dependency attestation is unavailable on this platform".to_owned())
}

/// Linux ldd appends ASLR load addresses to otherwise stable dependency
/// identities. They differ per invocation and would make equal nodes compute
/// different cache keys, so retain the complete report except those runtime
/// addresses. macOS otool output passes through unchanged.
fn normalized_dependency_report(report: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(report)
        .lines()
        .map(|line| {
            line.rsplit_once(" (0x")
                .filter(|(_, suffix)| suffix.ends_with(')'))
                .map_or(line, |(identity, _)| identity)
        })
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

/// The encoder every producer runs, as [`EncodedExecutable::capture`]
/// attests it; a text burn's frozen environment is proved against the same
/// program.
pub(crate) fn encoder_executable_path() -> Option<std::path::PathBuf> {
    resolve_executable_path(&ffmpeg_bin())
}

fn resolve_executable_path(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(bin);
    if path.components().count() > 1 {
        return std::fs::canonicalize(path).ok();
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .map(|directory| directory.join(bin))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| std::fs::canonicalize(candidate).ok())
}

/// Does this build carry a given bitstream filter? Matched on a whole line of
/// `ffmpeg -bsfs`, which lists exactly one filter per line — a substring
/// search would report `dovi_rpu` for anything merely mentioning it.
fn declares_bsf(list: &str, name: &str) -> bool {
    list.lines().any(|l| l.trim() == name)
}

/// libplacebo help is option-oriented rather than one-name-per-line. Match
/// the declaration token so a prose mention cannot admit a renderer whose
/// installed filter does not actually expose Dolby Vision reshaping.
fn declares_filter_option(help: &str, name: &str) -> bool {
    help.lines().any(|line| {
        line.split_whitespace()
            .next()
            .is_some_and(|token| token == name)
    })
}

/// Help and filter listings go to stdout on modern builds and to stderr on
/// older ones, so every question here is asked of both.
fn merged_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(stderr));
    text
}

/// Ask this ffmpeg one question and hand back what it said.
///
/// The thin shell around the subprocess: everything that *decides* anything
/// from the answer is a pure function below, so a missing binary and a build
/// without a feature are classified by tested code rather than by the spawn.
async fn probe_ffmpeg(args: &[&str]) -> Result<String, String> {
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    command.args(args);
    match bounded_command_output(command).await {
        Ok(out) => Ok(merged_output(&out.stdout, &out.stderr)),
        Err(error) => Err(error),
    }
}

/// Read-only Developer advice. A missing measurement is not a save gate.
pub(crate) async fn hevc_header_trace_available() -> Option<bool> {
    static VALUE: tokio::sync::OnceCell<Option<bool>> = tokio::sync::OnceCell::const_new();
    *VALUE
        .get_or_init(|| async {
            probe_ffmpeg(&["-hide_banner", "-bsfs"])
                .await
                .ok()
                .map(|list| declares_bsf(&list, "trace_headers"))
        })
        .await
}

/// Classify a `-bsfs` listing, including the case where it never arrived.
///
/// A build that could not be asked is treated exactly like one that answered
/// "no": the remux path must not attempt a filter it has no evidence for.
fn dovi_from_probe(probe: Result<String, String>) -> bool {
    let found = match &probe {
        Ok(list) => declares_bsf(list, "dovi_rpu"),
        Err(e) => {
            tracing::warn!(error = %e, "could not probe ffmpeg for bitstream filters");
            false
        }
    };
    if found {
        tracing::info!(
            "ffmpeg has dovi_rpu: Dolby Vision sources remux to their HDR10 base \
             for browsers that cannot decode DV"
        );
    } else {
        tracing::warn!(
            "this ffmpeg has no dovi_rpu bitstream filter (added in 7.1), so a Dolby \
             Vision configuration cannot be removed from a remux — browsers that \
             don't decode DV (Chrome does not; Safari does) will be given a \
             re-encode instead of the source video. Upgrade ffmpeg to 7.1+ to \
             stream those files untouched"
        );
    }
    found
}

/// Can this ffmpeg strip a Dolby Vision configuration (`dovi_rpu`, added in
/// 7.1)?
///
/// **Asked of the binary, not of its version string.** The version heuristic
/// it replaces was a proxy for the capability, and a proxy is exactly what
/// nobody can check from the outside: a Dolby Vision film that played at
/// 1080p in Chrome and untouched in Safari could not be explained without
/// someone shelling into the server, because the one fact that decided it —
/// "does this build have dovi_rpu" — was inferred rather than observed. It is
/// observed now, once, at startup (PERF-PLAN §9: mechanism claims get
/// verified against a live probe).
pub async fn has_dovi_rpu() -> bool {
    *DOVI_RPU
        .get_or_init(|| async { dovi_from_probe(probe_ffmpeg(&["-hide_banner", "-bsfs"]).await) })
        .await
}

/// Can the software-decode/tonemapx route used for non-backward-compatible
/// Dolby Vision start on this build, with the software encoder?
///
/// The option declaration is necessary but not sufficient: the SIMD filter
/// and libx264 must also work together.
/// Probe one synthetic frame through the production graph. The real Profile 5
/// validation remains responsible for proving that RPU side data changes the
/// pixels; this boot probe gates whether the renderer can be attempted at all.
///
/// See [`has_dovi_reshape_with`] for the hardware-encode pairings. Software
/// decode is not negotiable on any of them — the HEVC decoder is what attaches
/// the RPU side data `apply_dovi=1` consumes, and an inherited hardware decode
/// drops it silently — but the *encoder* only has to accept filtered frames,
/// which is what the upload suffix is for.
pub async fn has_dovi_reshape() -> bool {
    *DOVI_RESHAPE
        .get_or_init(|| async {
            let help = probe_ffmpeg(&["-hide_banner", "-h", "filter=tonemapx"]).await;
            let declared = help
                .as_ref()
                .is_ok_and(|text| declares_filter_option(text, "apply_dovi"));
            if !declared {
                tracing::warn!(
                    "ffmpeg tonemapx has no apply_dovi option; non-compatible Dolby Vision transcodes will be refused"
                );
                return false;
            }
            let passed = probe_dovi_reshape_graph(Encoder::Software).await;
            if passed {
                tracing::info!("ffmpeg proved the Dolby Vision tonemapx renderer");
            } else {
                tracing::warn!(
                    "ffmpeg could not run the Dolby Vision tonemapx renderer; non-compatible Dolby Vision transcodes will be refused"
                );
            }
            passed
        })
        .await
}

/// Run one synthetic frame through the production Dolby Vision reshape graph,
/// optionally uploading the filtered frames to a hardware encoder first.
///
/// The encoder's device init, upload suffix and production argument builder
/// are all used here. Omitting the init used to make every QSV probe fail with
/// "A hardware device reference is required", even though the real session
/// supplied that device and the node could run the graph.
/// Raster of every hardware-encoder capability proof. A hardware encoder has
/// a minimum frame size (AMD VA-API refuses anything under 128x128; Intel
/// QSV's HEVC Main10 encoder fails a 64x64 frame with "Invalid FrameType"),
/// so a 64x64 proof fails on encoders that take every real delivery size:
/// it pinned Dolby Vision to x264 on AMD and refused QSV's 4K HDR10 rung and
/// Dolby Vision HDR10 encode on Intel. Proofs encode the 1080p delivery
/// raster sessions actually feed. Software proofs keep their small frame;
/// libx264/libx265 have no such floor.
const HARDWARE_ENCODE_PROOF_RASTER: (u32, u32) = (1920, 1080);

fn dovi_reshape_probe_args(encoder: Encoder) -> Vec<String> {
    let (width, height) = HARDWARE_ENCODE_PROOF_RASTER;
    // The production reshape chain itself, at the delivery raster, followed by
    // the encoder's own upload: the proof cannot drift from what sessions run.
    let reshape = Pipeline::DoviTonemapx
        .filters(Some(i64::from(width)), i64::from(height), Some("dovi"))
        .unwrap_or_default();
    let filter = format!(
        "{reshape}{}",
        encoder
            .filter_suffix_for(OutputGrade::Sdr)
            .map(|s| format!(",{s}"))
            .unwrap_or_default()
    );
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    args.extend(encoder.init_args());
    args.extend([
        "-f".to_owned(),
        "lavfi".to_owned(),
        "-i".to_owned(),
        format!("color=size={width}x{height}:rate=1:color=black"),
        "-frames:v".to_owned(),
        "1".to_owned(),
        "-vf".to_owned(),
        filter,
    ]);
    args.extend(encoder.encode_args(1_000, EffectiveRateControl::Vbr, false, None));
    args.extend(["-f", "null", "-"].into_iter().map(str::to_owned));
    args
}

async fn probe_dovi_reshape_graph(encoder: Encoder) -> bool {
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    command
        .args(dovi_reshape_probe_args(encoder))
        .kill_on_drop(true);
    tokio::time::timeout(
        Duration::from_secs(20),
        crate::process_control::status_job_owned(
            &mut command,
            crate::process_control::ChildWork::background("Dolby Vision reshape capability probe"),
        ),
    )
    .await
    .is_ok_and(|result| result.is_ok_and(|status| status.success()))
}

/// Can the Dolby Vision reshape run with this encoder on this build?
///
/// Why this exists: the reshape was pinned to software *encode* as well as
/// software decode, and only the decode half was ever load-bearing. The RPU
/// side data `apply_dovi=1` consumes is attached by the software HEVC decoder
/// and does not survive an inherited hardware decode — that constraint is
/// real and unchanged. The encoder merely has to accept the filter's output,
/// which is what `hwupload` is for. Pinning it to libx264 capped every
/// non-DV-capable client at the software Auto rung (720p) for a 4K Dolby
/// Vision source, even on a node with a perfectly good hardware encoder that
/// was doing nothing.
///
/// Nothing is assumed: each pairing is probed once, exactly like the software
/// route, and an unproved pairing falls back to software rather than failing a
/// session in front of a viewer.
pub async fn has_dovi_reshape_with(encoder: plurx_core::transcode::Encoder) -> bool {
    if encoder == Encoder::Software {
        return has_dovi_reshape().await;
    }
    // The filter chain is the same one the software route already proved; if
    // that failed, no upload target can rescue it.
    if !has_dovi_reshape().await {
        return false;
    }
    let key = encoder.label();
    let cell =
        DOVI_RESHAPE_HW.get_or_init(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let mut memo = cell.lock().await;
    if let Some(known) = memo.get(key) {
        return *known;
    }
    let passed = probe_dovi_reshape_graph(encoder).await;
    if passed {
        tracing::info!(
            encoder = key,
            "ffmpeg proved the Dolby Vision reshape with a hardware encoder"
        );
    } else {
        tracing::info!(
            encoder = key,
            "the Dolby Vision reshape cannot use this hardware encoder on this build; \
             falling back to the software route"
        );
    }
    memo.insert(key, passed);
    passed
}

/// The message `tonemapx` prints when its HDR passthrough mode is asked for
/// and the input is not Dolby Vision. It is not a failure of the build.
const PASSTHROUGH_NEEDS_DOVI: &str = "passthrough only works for Dolby Vision inputs";

/// Can the HDR10 rung — Profile 5 decode → `tonemapx` HDR passthrough →
/// libx265 Main10 — start on this build?
///
/// **This is deliberately not [`has_dovi_reshape`] with the transfer swapped,
/// and the difference is the whole probe.** Two things make the obvious
/// version wrong:
///
/// 1. **The output format has to move with the transfer.** `tonemapx` with
///    `transfer=smpte2084` and an 8-bit output format does not return an
///    error — it hits `Assertion 0 failed at libavfilter/vf_tonemapx.c:1475`
///    and calls `abort()` (SIGABRT, exit 134, measured). A boot probe that
///    kept `format=yuv420p` would kill the daemon's own capability check.
///    The graph therefore comes from `Pipeline::DoviPassthrough`, whose
///    transfer and pixel format are one `OutputGrade` and cannot be
///    separated.
/// 2. **No synthetic source has an RPU.** Passthrough is Dolby-Vision-input
///    only, so the filter is entitled to refuse a `lavfi` frame with
///    [`PASSTHROUGH_NEEDS_DOVI`]. That refusal says the *input* was wrong,
///    not that the build cannot do this, so it counts as a pass — the
///    per-source proof (`dovi_reshape_changes_pixels`) is what establishes a
///    real RPU, and it runs against the actual file.
///
/// What this probe does establish: the filter takes these options, the graph
/// builds at 10-bit without aborting, and `libx265` exists in this build and
/// accepts the production `-x265-params`. A death by signal is reported as a
/// failure and logged loudly, because that is finding (1) happening for real.
pub async fn has_dovi_passthrough() -> bool {
    *DOVI_PASSTHROUGH
        .get_or_init(|| async {
            let help = probe_ffmpeg(&["-hide_banner", "-h", "filter=tonemapx"]).await;
            if !help
                .as_ref()
                .is_ok_and(|text| declares_filter_option(text, "apply_dovi"))
            {
                tracing::warn!(
                    "ffmpeg tonemapx has no apply_dovi option; the Dolby Vision HDR10 rung will be refused"
                );
                return false;
            }
            let Some(filter) = Pipeline::DoviPassthrough.filters(Some(64), 64, Some("dolby_vision"))
            else {
                return false;
            };
            let mut command = tokio::process::Command::new(ffmpeg_bin());
            command
                .kill_on_drop(true)
                .args(["-hide_banner", "-loglevel", "error"])
                .args(["-f", "lavfi", "-i", "color=size=64x64:rate=1:color=black"])
                .args(["-frames:v", "1", "-vf"])
                .arg(&filter)
                .args(["-c:v", "libx265"])
                .args(["-x265-params", "hdr10=1:repeat-headers=1"])
                .args(["-f", "null", "-"]);
            let output = tokio::time::timeout(
                Duration::from_secs(20),
                crate::process_control::output_job_owned(&mut command, crate::process_control::ChildWork::background("Dolby Vision passthrough capability probe")),
            )
            .await;
            let Ok(Ok(output)) = output else {
                tracing::warn!(
                    "ffmpeg could not run the Dolby Vision HDR10 passthrough renderer; the HDR10 rung will be refused"
                );
                return false;
            };
            let stderr = String::from_utf8_lossy(&output.stderr);
            if output.status.success() {
                tracing::info!("ffmpeg proved the Dolby Vision HDR10 passthrough renderer");
                return true;
            }
            // `code()` is None only when a signal killed the process. That is
            // the abort() case, and it must never read as a soft refusal.
            if output.status.code().is_some() && stderr.contains(PASSTHROUGH_NEEDS_DOVI) {
                tracing::info!(
                    "ffmpeg proved the Dolby Vision HDR10 passthrough renderer (it declined the \
                     synthetic non-Dolby input, which is the documented behaviour)"
                );
                return true;
            }
            tracing::warn!(
                status = ?output.status,
                "ffmpeg could not run the Dolby Vision HDR10 passthrough renderer; the HDR10 rung \
                 will be refused: {}",
                stderr.trim()
            );
            false
        })
        .await
}

/// Can this build re-encode an ordinary HDR source without tone-mapping it —
/// a 10-bit scale straight into HEVC Main10 PQ?
///
/// The other passthrough rung, and the one that matters to almost every HDR
/// title: HDR10, HDR10+, and the HDR10 base of a stripped Dolby Vision file.
/// Until M4 every one of those tone-mapped to SDR whenever anything forced a
/// re-encode — a height cap, a bitrate cap, burned subtitles, the quality menu
/// (PLAYBACK-CAPS-V2-PLAN §2, edge E3).
///
/// **Simpler than [`has_dovi_passthrough`], and the simplicity is the point.**
/// That probe has to accept a documented refusal as a pass, because no
/// synthetic frame carries a Dolby RPU and the filter is entitled to decline
/// one. This graph reads no metadata at all — it scales and names a pixel
/// format — so a `lavfi` frame exercises the whole thing and a clean exit is a
/// real proof rather than an inference. The encode half is the same libx265
/// Main10 with the same PQ/BT.2020 flags, taken from the same
/// [`OutputGrade`], so a pass here is a pass for the bytes a session will
/// actually emit.
pub async fn has_hdr10_passthrough() -> bool {
    *HDR10_PASSTHROUGH
        .get_or_init(|| async {
            let Some(filter) = Pipeline::Hdr10Passthrough.filters(Some(64), 64, Some("hdr10"))
            else {
                return false;
            };
            let mut command = tokio::process::Command::new(ffmpeg_bin());
            command
                .kill_on_drop(true)
                .args(["-hide_banner", "-loglevel", "error"])
                .args(["-f", "lavfi", "-i", "color=size=64x64:rate=1:color=black"])
                .args(["-frames:v", "1", "-vf"])
                .arg(&filter)
                .args(Encoder::Software.encode_args_for(
                    OutputGrade::Hdr10,
                    20_000,
                    EffectiveRateControl::Vbr,
                    false,
                    None,
                ))
                .args(["-f", "null", "-"]);
            let output = tokio::time::timeout(
                Duration::from_secs(20),
                crate::process_control::output_job_owned(
                    &mut command,
                    crate::process_control::ChildWork::background(
                        "HDR10 passthrough capability probe",
                    ),
                ),
            )
            .await;
            let Ok(Ok(output)) = output else {
                tracing::warn!(
                    "ffmpeg could not run the HDR10 passthrough encode; every HDR transcode on \
                     this node will tone-map to SDR"
                );
                return false;
            };
            if output.status.success() {
                tracing::info!("ffmpeg proved the HDR10 passthrough encode");
                return true;
            }
            tracing::warn!(
                status = ?output.status,
                "ffmpeg could not run the HDR10 passthrough encode; every HDR transcode on this \
                 node will tone-map to SDR: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            false
        })
        .await
}

/// The hardware Main10 encode proof both QSV HDR10 rungs run: the encoder's
/// device init, its P010 upload and its production Main10 argument builder,
/// at [`HARDWARE_ENCODE_PROOF_RASTER`].
fn hardware_main10_probe_args(encoder: Encoder) -> Option<Vec<String>> {
    let upload = encoder.filter_suffix_for(OutputGrade::Hdr10)?;
    let (width, height) = HARDWARE_ENCODE_PROOF_RASTER;
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    args.extend(encoder.init_args());
    args.extend([
        "-f".to_owned(),
        "lavfi".to_owned(),
        "-i".to_owned(),
        format!("color=size={width}x{height}:rate=1:color=black"),
        "-frames:v".to_owned(),
        "1".to_owned(),
        "-vf".to_owned(),
        upload.to_owned(),
    ]);
    args.extend(encoder.encode_args_for(
        OutputGrade::Hdr10,
        20_000,
        EffectiveRateControl::Vbr,
        false,
        None,
    ));
    args.extend(["-f", "null", "-"].into_iter().map(str::to_owned));
    Some(args)
}

/// Can this node run the plain HDR10 rung's QSV half — P010 upload into
/// `hevc_qsv` Main10 with the PQ/BT.2020 flags?
///
/// A separate probe from [`has_dovi_passthrough_with`] even though the graph
/// is the same shape, because that one short-circuits on `tonemapx` declaring
/// `apply_dovi` — a jellyfin filter this route neither uses nor needs. Reusing
/// it would take the 4K HDR10 rung away from every QSV node running stock
/// ffmpeg, with nothing in the log naming a Dolby Vision filter as the reason.
pub async fn has_hdr10_passthrough_qsv() -> bool {
    if !has_hdr10_passthrough().await {
        return false;
    }
    *HDR10_PASSTHROUGH_QSV
        .get_or_init(|| async {
            let Some(args) = hardware_main10_probe_args(Encoder::Qsv) else {
                return false;
            };
            let mut command = tokio::process::Command::new(ffmpeg_bin());
            command.kill_on_drop(true).args(args);
            let passed = tokio::time::timeout(
                Duration::from_secs(20),
                crate::process_control::status_job_owned(
                    &mut command,
                    crate::process_control::ChildWork::background(
                        "HDR10 QSV passthrough capability probe",
                    ),
                ),
            )
            .await
            .is_ok_and(|result| result.is_ok_and(|status| status.success()));
            if passed {
                tracing::info!("ffmpeg proved the QSV Main10 encode for a plain HDR source");
            } else {
                tracing::warn!(
                    "ffmpeg could not run the QSV Main10 encode for a plain HDR source; the 4K \
                     HDR10 rung will be refused on this node"
                );
            }
            passed
        })
        .await
}

/// Plain PQ/BT.2020 scale and P010 upload into the VAAPI Main10 encoder.
/// This proof does not advertise Dolby RPU processing or 4K throughput.
pub async fn has_hdr10_passthrough_vaapi(
    policy: Option<plurx_core::transcode::VaapiHdrPolicy>,
) -> bool {
    *HDR10_PASSTHROUGH_VAAPI[usize::from(policy.is_some())]
        .get_or_init(|| async {
            let encoder = Encoder::Vaapi;
            let (width, height) = HARDWARE_ENCODE_PROOF_RASTER;
            let Some(filter) = Pipeline::Hdr10Passthrough.filters(
                Some(i64::from(width)),
                i64::from(height),
                Some("hdr10"),
            ) else {
                return false;
            };
            let Some(upload) = encoder.filter_suffix_for(OutputGrade::Hdr10) else {
                return false;
            };
            let mut command = tokio::process::Command::new(ffmpeg_bin());
            command
                .kill_on_drop(true)
                .args(["-hide_banner", "-loglevel", "error", "-filter_threads", "1"])
                .args(encoder.init_args())
                .args(["-f", "lavfi", "-i"])
                .arg(format!("color=size={width}x{height}:rate=24:color=black"))
                .args(["-frames:v", "24", "-vf"])
                .arg(format!("{filter},{upload}"))
                .args(encoder.encode_args_for(
                    OutputGrade::Hdr10,
                    20_000,
                    EffectiveRateControl::Vbr,
                    false,
                    None,
                ))
                .args(policy.into_iter().flat_map(|policy| policy.flags()))
                .args(["-bf", "0", "-f", "null", "-"]);
            tokio::time::timeout(
                Duration::from_secs(20),
                crate::process_control::status_job_owned(
                    &mut command,
                    crate::process_control::ChildWork::background(
                        "HDR10 VAAPI passthrough capability probe",
                    ),
                ),
            )
            .await
            .is_ok_and(|result| result.is_ok_and(|status| status.success()))
        })
        .await
}

/// Does the VA-API render device decode HEVC Main10 into GPU surfaces?
///
/// The Dolby Vision renderer decodes its layers there when it can, and that
/// decides the CPU it is debited (`dv_segment::RendererDecode`). The startup
/// decoder inventory cannot answer it: it decodes an 8-bit clip and reads a
/// `Reinit context` line FFmpeg's HEVC decoder never prints. Here a shipped
/// Main10 clip is decoded with `-hwaccel_output_format vaapi` into
/// `hwdownload`, which accepts only hardware frames, so success means the GPU
/// decoded it.
///
/// A success is kept for the process. A failure is not: it may be a device
/// still settling at boot, so the probe runs again on the next need after a
/// backoff (30 s, doubling, at most an hour). Until proved, the answer is
/// `false` and the renderer is debited its software shape. Warmed off the
/// boot path at startup; the completed DV window stays the authority.
pub async fn has_hevc_main10_vaapi_decode() -> bool {
    let due = |proof: &Main10DecodeProof| {
        !proof.proved
            && proof
                .retry_at
                .is_none_or(|at| std::time::Instant::now() >= at)
    };
    {
        let proof = HEVC_MAIN10_VAAPI_DECODE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if proof.proved || !due(&proof) {
            return proof.proved;
        }
    }
    let Ok(_probe) = HEVC_MAIN10_VAAPI_PROBE.try_lock() else {
        return false;
    };
    {
        let proof = HEVC_MAIN10_VAAPI_DECODE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !due(&proof) {
            return proof.proved;
        }
    }
    let proved = probe_hevc_main10_vaapi_decode().await;
    let mut proof = HEVC_MAIN10_VAAPI_DECODE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if proved {
        proof.proved = true;
        proof.retry_at = None;
    } else {
        proof.failures = proof.failures.saturating_add(1);
        proof.retry_at = Some(std::time::Instant::now() + main10_probe_backoff(proof.failures));
    }
    proved
}

/// The wait before the `failures`th retry of a failed Main10 decode probe.
fn main10_probe_backoff(failures: u32) -> Duration {
    Duration::from_secs(30)
        .saturating_mul(1 << failures.saturating_sub(1).min(7))
        .min(Duration::from_secs(3600))
}

async fn probe_hevc_main10_vaapi_decode() -> bool {
    let device = plurx_core::transcode::vaapi_device();
    if !std::path::Path::new(&device).exists() {
        return false;
    }
    let Ok(scratch) = tempfile::tempdir() else {
        return false;
    };
    let clip = scratch.path().join("main10.mkv");
    if tokio::fs::write(&clip, HEVC_MAIN10_PROBE_CLIP)
        .await
        .is_err()
    {
        return false;
    }
    let mut decode = tokio::process::Command::new(ffmpeg_bin());
    decode
        .args(["-hide_banner", "-loglevel", "error", "-hwaccel", "vaapi"])
        .args([
            "-hwaccel_device",
            &device,
            "-hwaccel_output_format",
            "vaapi",
            "-i",
        ])
        .arg(&clip)
        .args([
            "-vf",
            "hwdownload,format=p010le",
            "-frames:v",
            "1",
            "-f",
            "null",
            "-",
        ])
        .kill_on_drop(true);
    tokio::time::timeout(
        HEVC_MAIN10_PROBE_DEADLINE,
        crate::process_control::status_job_owned(
            &mut decode,
            crate::process_control::ChildWork::background("HEVC Main10 VA-API decode probe"),
        ),
    )
    .await
    .is_ok_and(|result| result.is_ok_and(|status| status.success()))
}

/// Can this node accept the software Dolby renderer's 10-bit frames and run
/// the measured QSV Main10 encode graph?
///
/// The synthetic frame cannot carry a Dolby RPU, so the passthrough filter's
/// documented refusal is proved separately by [`has_dovi_passthrough`]. This
/// probe establishes the other half that refusal cannot reach: QSV device
/// init, P010 conversion/upload, Main10 encode, bounded VBR and PQ/BT.2020
/// output flags. A real source's RPU mutation is still proved per file.
pub async fn has_dovi_passthrough_with(encoder: Encoder) -> bool {
    if encoder == Encoder::Software {
        return has_dovi_passthrough().await;
    }
    if encoder != Encoder::Qsv || !has_dovi_passthrough().await {
        return false;
    }
    *DOVI_PASSTHROUGH_QSV
        .get_or_init(|| async {
            let Some(args) = hardware_main10_probe_args(encoder) else {
                return false;
            };
            let mut command = tokio::process::Command::new(ffmpeg_bin());
            command.kill_on_drop(true).args(args);
            let passed = tokio::time::timeout(
                Duration::from_secs(20),
                crate::process_control::status_job_owned(&mut command, crate::process_control::ChildWork::background("Dolby Vision passthrough capability probe")),
            )
                .await
                .is_ok_and(|result| result.is_ok_and(|status| status.success()));
            if passed {
                tracing::info!(
                    encoder = encoder.label(),
                    "ffmpeg proved the Dolby Vision HDR10 renderer's hardware encode half"
                );
            } else {
                tracing::warn!(
                    encoder = encoder.label(),
                    "ffmpeg could not run the Dolby Vision HDR10 hardware encode graph; using the measured software rung"
                );
            }
            passed
        })
        .await
}

async fn dovi_probe_output(
    file: &MediaFile,
    apply: bool,
    class: crate::process_control::ChildClass,
) -> Result<Vec<String>, String> {
    let seek = file
        .duration_ms
        .map(|duration| (duration / 5).saturating_sub(1_000) as f64 / 1_000.0)
        .unwrap_or(0.0)
        .max(0.0);
    let (width, height) = output_size(file, 720)
        .ok_or_else(|| "Dolby Vision pixel probe has no valid output size".to_owned())?;
    let filter = Pipeline::DoviTonemapx
        .filters(Some(width), height, Some("dolby_vision"))
        .ok_or_else(|| "Dolby Vision renderer produced no filter graph".to_owned())?
        .replace("apply_dovi=1", &format!("apply_dovi={}", u8::from(apply)));
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    command
        .kill_on_drop(true)
        .args(["-hide_banner", "-loglevel", "error"])
        .args(["-ss", &format!("{seek:.3}"), "-i"])
        .arg(&file.path)
        .args(["-map", "0:v:0", "-frames:v", "3", "-an", "-vf"])
        .arg(filter)
        .args(["-f", "framemd5", "-"]);
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        crate::process_control::output_job_owned(
            &mut command,
            crate::process_control::ChildWork::new(class, "Dolby Vision pixel probe"),
        ),
    )
    .await
    .map_err(|_| "Dolby Vision pixel probe timed out".to_owned())?
    .map_err(|error| format!("starting Dolby Vision pixel probe: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Dolby Vision pixel probe exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let frames: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(str::to_owned)
        .collect();
    (!frames.is_empty())
        .then_some(frames)
        .ok_or_else(|| "Dolby Vision pixel probe produced no frame hashes".to_owned())
}

/// Prove, on the requested Profile 5 source, that the decoder exports RPU side
/// data through the production graph and that tonemapx changes pixels when
/// Dolby Vision application is enabled. A mere option probe cannot make that
/// claim because a frame with no DOVI metadata makes the option a no-op.
///
/// `class` is the caller's: a session start waiting on the proof passes
/// realtime, a background pass background.
pub async fn dovi_reshape_changes_pixels(
    file: &MediaFile,
    class: crate::process_control::ChildClass,
) -> DoviPixelProof {
    let enabled = dovi_probe_output(file, true, class).await;
    let disabled = dovi_probe_output(file, false, class).await;
    let proof = dovi_pixel_verdict(enabled, disabled);
    match &proof {
        DoviPixelProof::Proved => {}
        DoviPixelProof::Refuted => {
            tracing::warn!(file = %file.path.display(), "Dolby Vision RPU mutation did not change sampled pixels");
        }
        DoviPixelProof::Inconclusive(reason) => {
            tracing::warn!(file = %file.path.display(), %reason, "could not prove Dolby Vision RPU pixel reshaping; not recording a verdict");
        }
    }
    proof
}

/// What one per-source Dolby Vision pixel proof established.
///
/// Three states, because a probe that did not finish is not evidence about
/// the source. Only `Proved` and `Refuted` are verdicts a node may remember;
/// `Inconclusive` — a timeout, a spawn failure, a non-zero exit, an empty
/// hash list — says the measurement did not happen, and remembering it as
/// "not Dolby Vision" turned one slow probe on a loaded node into a
/// permanent 501 for that title until the process restarted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoviPixelProof {
    /// Both probes ran and applying the RPU changed the sampled pixels.
    Proved,
    /// Both probes ran and the pixels were identical: the RPU is a no-op.
    Refuted,
    /// At least one probe did not produce frame hashes.
    Inconclusive(String),
}

impl DoviPixelProof {
    /// The cacheable verdict, if this is one.
    pub fn verdict(&self) -> Option<bool> {
        match self {
            Self::Proved => Some(true),
            Self::Refuted => Some(false),
            Self::Inconclusive(_) => None,
        }
    }
}

/// Map the enabled/disabled probe outputs to a proof. Pure, so the mapping
/// — and above all "an Err is never a verdict" — is tested without ffmpeg.
pub(crate) fn dovi_pixel_verdict(
    enabled: Result<Vec<String>, String>,
    disabled: Result<Vec<String>, String>,
) -> DoviPixelProof {
    match (enabled, disabled) {
        (Ok(enabled), Ok(disabled)) if enabled != disabled => DoviPixelProof::Proved,
        (Ok(_), Ok(_)) => DoviPixelProof::Refuted,
        (Err(enabled), Err(disabled)) => DoviPixelProof::Inconclusive(format!(
            "RPU-applied probe: {enabled}; RPU-ignored probe: {disabled}"
        )),
        (Err(error), Ok(_)) => DoviPixelProof::Inconclusive(format!("RPU-applied probe: {error}")),
        (Ok(_), Err(error)) => DoviPixelProof::Inconclusive(format!("RPU-ignored probe: {error}")),
    }
}

/// Scan `ffmpeg -h full` for the pacing options.
///
/// Matches the *declaration* — an indented line whose first token is the option
/// — not any mention of the name. A plain substring search reports `-readrate`
/// on an ffmpeg 4.x that has no such option, because `-re`'s own help line reads
/// "…equivalent to -readrate 1". Getting that wrong is not cosmetic: an
/// unrecognised option makes ffmpeg exit rather than warn, so every stream on
/// that build would fail to start.
fn parse_pacing_caps(help: &str) -> PacingCaps {
    let declared = |name: &str| {
        help.lines().any(|l| {
            // split_whitespace already skips the leading indent.
            l.split_whitespace().next().is_some_and(|tok| tok == name)
        })
    };
    PacingCaps {
        readrate: declared("-readrate"),
        initial_burst: declared("-readrate_initial_burst"),
    }
}

/// Classify a `-h full` listing, including the case where it never arrived.
fn pacing_from_probe(probe: Result<String, String>) -> PacingCaps {
    let caps = match &probe {
        Ok(help) => parse_pacing_caps(help),
        Err(e) => {
            tracing::warn!(error = %e, "could not probe ffmpeg for pacing support");
            PacingCaps::default()
        }
    };
    if !caps.readrate {
        tracing::warn!(
            "this ffmpeg has no -readrate; remux streams will run unpaced and can \
             saturate a client's link, and HLS sessions fall back to realtime pacing \
             (which cannot build a playback buffer). ffmpeg 6.1+ is recommended."
        );
    } else if !caps.initial_burst {
        // WARN, not info, since the publish gate raised the stakes: a
        // copy session's first playlist waits for COPY_PUBLISH_GATE_SECS
        // of media, and without a burst that cushion is produced at the
        // flat paced rate instead of at I/O speed — at the default 2x,
        // that alone is ~6+ seconds of every time-to-first-frame, and
        // it is invisible unless something names it.
        tracing::warn!(
            "this ffmpeg has -readrate but not -readrate_initial_burst (needs 6.1+; \
             jellyfin-ffmpeg7 has it): sessions are paced flat from the first byte, \
             so the copy path's publish gate fills at the paced rate instead of at \
             I/O speed and every play starts seconds slower than it needs to"
        );
    }
    caps
}

pub async fn pacing_caps() -> PacingCaps {
    *PACING
        .get_or_init(|| async {
            let help = probe_ffmpeg(&["-hide_banner", "-h", "full"]).await;
            let mut caps = pacing_from_probe(help);
            // Some builds (notably ffmpeg 8.0.1) declare the option but
            // silently ignore it. Check behaviourally when the declaration
            // looks promising.
            if caps.initial_burst {
                let measured = classify_burst(caps, probe_burst().await);
                if caps.initial_burst && !measured.initial_burst {
                    let ffmpeg_build = ffmpeg_build().await;
                    tracing::warn!(
                        "this ffmpeg build ({ffmpeg_build}) declares -readrate_initial_burst but does not honour it: \
                         sessions are paced flat from the first byte, so the copy path's publish gate \
                         fills at the paced rate instead of at I/O speed and every play starts seconds \
                         slower than it needs to"
                    );
                }
                caps = measured;
            }
            caps
        })
        .await
}

impl PacingCaps {
    /// Turn admin settings into flags this build actually has.
    ///
    /// `legacy_realtime_ok` decides what a pre-5.1 build gets. True for the
    /// copy path: it was paced with a bare `-re` before `-readrate` existed
    /// here, and an *unpaced* copy floods the session directory with a whole
    /// 4K film — realtime is the lesser evil. False for transcode, which has
    /// never been paced at all: capping an encoder at 1x on an old build
    /// would be a new regression dressed as a fallback, and a transcode that
    /// outruns realtime is bounded by the ahead-window suspend anyway.
    pub fn resolve(&self, rate: f64, burst: f64, legacy_realtime_ok: bool) -> Pacing {
        if rate <= 0.0 {
            return Pacing::unpaced();
        }
        if !self.readrate {
            return Pacing {
                legacy_re: legacy_realtime_ok,
                ..Pacing::unpaced()
            };
        }
        Pacing {
            readrate: Some(rate),
            initial_burst: self.initial_burst.then_some(burst).filter(|b| *b > 0.0),
            legacy_re: false,
        }
    }
}

/// Check whether this ffmpeg build actually honours `-readrate_initial_burst`.
///
/// Some builds (notably ffmpeg 8.0.1-3ubuntu2) declare the option in their
/// help text but silently ignore it at runtime, so a purely textual probe of
/// the declaration is insufficient. This runs a short behavioural test.
///
/// The test creates a 2-second `testsrc` fixture and runs ffmpeg with a 300 s
/// burst at 2x readrate. An honoured burst consumes the fixture at I/O speed
/// (sub-200 ms). An inert burst paces at the readrate and takes ~1 second of
/// wall time. A 600 ms threshold cleanly separates the two cases.
fn classify_burst(mut caps: PacingCaps, probe: Result<Duration, String>) -> PacingCaps {
    caps.initial_burst &= probe.is_ok_and(|elapsed| elapsed < Duration::from_millis(600));
    caps
}

fn burst_probe_args() -> [&'static str; 14] {
    [
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-readrate_initial_burst",
        "300",
        "-readrate",
        "2",
        "-i",
        "testsrc=duration=2:size=2x2:rate=1",
        "-f",
        "null",
        "-",
    ]
}

async fn probe_burst() -> Result<Duration, String> {
    let args = burst_probe_args();
    let start = std::time::Instant::now();
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    command.args(args);
    match crate::process_control::output_job_owned(
        &mut command,
        crate::process_control::ChildWork::background("ffmpeg read-rate burst probe"),
    )
    .await
    {
        Ok(out) if out.status.success() => Ok(start.elapsed()),
        Ok(out) => Err(format!("exited with {}", out.status)),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests;
