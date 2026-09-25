//! The ffmpeg binaries and what this particular build can do.
//!
//! Every delivery path shells out to the same two binaries, and two of them
//! (the progressive remux and the HLS sessions) need the same answer to the
//! same question: does this build understand the input-pacing flags? Probing
//! it in one place, once per process, is what keeps the answer consistent —
//! and keeps a stream from failing to start because one path guessed.

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
    override_value
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

fn default_bin(name: &str) -> String {
    #[cfg(windows)]
    {
        if let Ok(executable) = std::env::current_exe() {
            if let Some(directory) = executable.parent() {
                let sibling = directory.join(format!("{name}.exe"));
                if sibling.is_file() {
                    return sibling.to_string_lossy().into_owned();
                }
            }
        }
    }
    name.to_owned()
}

/// ffmpeg binary, overridable via `PLURX_FFMPEG` (jellyfin-ffmpeg / pinned path).
pub fn ffmpeg_bin() -> String {
    resolve_bin(std::env::var("PLURX_FFMPEG").ok(), &default_bin("ffmpeg"))
}

/// ffprobe binary, overridable via `PLURX_FFPROBE` (jellyfin-ffmpeg / pinned).
pub fn ffprobe_bin() -> String {
    resolve_bin(std::env::var("PLURX_FFPROBE").ok(), &default_bin("ffprobe"))
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

    pub async fn output(mut self) -> std::io::Result<(std::process::ExitStatus, String)> {
        let stderr = self
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("extractor stderr was not piped"))?;
        let child = self.child.as_mut().expect("owned extraction child");
        let (status, diagnostics) = tokio::join!(child.wait(), drain_diagnostics(stderr));
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
        mut self,
        path: &std::path::Path,
        max_bytes: u64,
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
                let mut buffer = [0_u8; 64 * 1024];
                let exceeded = loop {
                    let read = stdout.read(&mut buffer).await?;
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

#[derive(Debug)]
pub(crate) struct EncodedExecutable {
    pub path: std::path::PathBuf,
    pub digest: String,
    object_version: String,
}

impl EncodedExecutable {
    pub async fn capture() -> Result<Self, String> {
        let path = resolve_executable_path(&ffmpeg_bin())
            .ok_or("cannot resolve the encoder executable")?;
        Self::capture_at(path).await
    }

    async fn capture_at(path: std::path::PathBuf) -> Result<Self, String> {
        let (digest, object_version) = hash_engine_object(&path).await?;
        Ok(Self {
            path,
            digest: hex::encode(digest),
            object_version,
        })
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
        (self.path.clone(), self.object_version.clone())
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

/// Probe the exact source capability retained by a recipe preparer. Comparing
/// this document with the scanner's document prevents a same-size,
/// same-second pathname replacement from pairing fresh bytes with stale
/// geometry, tracks, cadence, or color facts.
///
/// `work` is the caller's: a session start that is waiting on this probe
/// passes a realtime class, since the probe runs under a five-second bound
/// whose miss the viewer sees as a refusal.
pub(crate) async fn held_source_probe_json(
    source: &std::fs::File,
    work: crate::process_control::ChildWork,
) -> Result<String, String> {
    let document = held_source_probe_json_with_limits(
        source,
        ENGINE_PROBE_TIMEOUT,
        ENGINE_PROBE_MAX_BYTES,
        "engine probe",
        work,
    )
    .await?;
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
pub(crate) async fn held_source_index_probe_json(source: &std::fs::File) -> Result<String, String> {
    let document = held_source_probe_json_with_limits(
        source,
        Duration::from_secs(30),
        1024 * 1024,
        "index metadata probe",
        crate::process_control::ChildWork::background("fragment index metadata probe"),
    )
    .await?;
    // Stamped for the same reason the engine probe is: the two entry points
    // must not disagree about a field whose whole purpose is to be compared.
    Ok(stamped_with_this_reporter(document).await)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeldPacketProbeRequest {
    pub stream_index: u32,
    /// `None` means the prefix. A value means an approximate seek followed by
    /// an open-ended read to natural EOF.
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
        (Some(start), None) => Some(format!("{}%", start.max(0))),
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

async fn held_source_probe_json_with_limits(
    source: &std::fs::File,
    timeout: Duration,
    max_bytes: u64,
    label: &'static str,
    work: crate::process_control::ChildWork,
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
            bounded_command_output_with_limits(command, timeout, max_bytes, label, work).await?;
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
            bounded_command_output_with_limits(command, timeout, max_bytes, label, work).await?;
        String::from_utf8(output.stdout)
            .map_err(|error| format!("ffprobe returned non-UTF-8 JSON: {error}"))
    }
}

fn normalized_probe_document(raw: &str) -> Result<serde_json::Value, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(raw).map_err(|error| format!("invalid ffprobe JSON: {error}"))?;
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
/// active Fontconfig rule and discoverable font file is still rechecked
/// before spawning and publication.
#[derive(Debug, Clone)]
pub(crate) struct EncodedEngine {
    pub digest: String,
    process_identity: String,
    /// The encoder dependency closure. Statted under `kind="media"`.
    objects: Arc<[(std::path::PathBuf, String)]>,
    /// The Fontconfig rules and font files a text burn captured, kept apart
    /// from `objects` so their cost is charged under `kind="font"`. Empty
    /// for every recipe that does not burn text.
    font_objects: Arc<[(std::path::PathBuf, String)]>,
    font_digest: Option<String>,
}

impl EncodedEngine {
    pub async fn capture(text_burn: bool) -> Result<Self, String> {
        let media = FRAGMENT_INDEX_ENGINE
            .get_or_init(fragment_index_engine_inner)
            .await;
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
        let mut font_digest = None;
        if text_burn {
            // Fontconfig's closure is live configuration, unlike the process's
            // loaded media libraries. Probe it for every recipe capture so a
            // newly installed font or rule cannot reuse the old URI identity.
            let enumeration = font_render_engine_inner().await;
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
            let fonts = enumeration.engine;
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
            digest.update(fonts.digest.as_bytes());
            font_digest = Some(fonts.digest.clone());
            font_objects = fonts.objects.to_vec();
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
            font_digest,
        })
    }

    /// Re-attest every input which can change the bytes emitted under this
    /// recipe. Fontconfig must be enumerated again: checking only the files
    /// captured earlier detects replacements and removals, but not additions
    /// which change font resolution.
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
        self.is_current_charged(Some(executable.attestation_object()))
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
        let mut charges = EngineAttestationCharges::default();
        let (media_current, media_elapsed) =
            engine_objects_are_current_batch(executable, Arc::clone(&self.objects)).await;
        charges.add(
            EngineAttestationKind::Media,
            EngineAttestationPhase::Stat,
            media_elapsed,
        );
        if !media_current {
            charges.flush();
            return (false, charges);
        }
        let Some(expected_font_digest) = self.font_digest.as_deref() else {
            charges.flush();
            return (true, charges);
        };

        // Three font stat batches, one observation: the captured font
        // objects, the re-enumeration's own stat loop, and the freshly
        // enumerated closure.
        let (captured_current, captured_elapsed) =
            engine_objects_are_current_batch(None, Arc::clone(&self.font_objects)).await;
        charges.add(
            EngineAttestationKind::Font,
            EngineAttestationPhase::Stat,
            captured_elapsed,
        );
        let enumeration = font_render_engine_inner().await;
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
        let (closure_current, closure_elapsed) =
            font_closure_is_current(expected_font_digest, &enumeration.engine).await;
        charges.add(
            EngineAttestationKind::Font,
            EngineAttestationPhase::Stat,
            closure_elapsed,
        );
        charges.flush();
        (captured_current && closure_current, charges)
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
            digest.update(object_digest);
            objects.push((path.clone(), version));
        }
        Ok(Self {
            digest: hex::encode(digest.finalize()),
            process_identity: process.to_owned(),
            objects: objects.into(),
            font_objects: Vec::new().into(),
            font_digest: None,
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

/// Compare a freshly enumerated Fontconfig closure with the captured digest,
/// returning what the stat batch cost so the caller can charge it once for
/// the whole attestation.
async fn font_closure_is_current(
    expected_digest: &str,
    current: &FragmentIndexEngine,
) -> (bool, Duration) {
    if !current.usable {
        return (false, Duration::ZERO);
    }
    let (objects_current, elapsed) =
        engine_objects_are_current_batch(None, Arc::clone(&current.objects)).await;
    (
        objects_current && current.digest == expected_digest,
        elapsed,
    )
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

fn engine_objects_are_current(objects: &[(std::path::PathBuf, String)]) -> bool {
    objects.iter().all(|(path, expected)| {
        engine_path_version(path)
            .ok()
            .is_some_and(|current| &current == expected)
    })
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
async fn engine_objects_are_current_batch(
    extra: Option<(std::path::PathBuf, String)>,
    objects: Arc<[(std::path::PathBuf, String)]>,
) -> (bool, Duration) {
    let started = Instant::now();
    let current = tokio::task::spawn_blocking(move || {
        if let Some((path, expected)) = extra {
            if !engine_path_version(&path)
                .ok()
                .is_some_and(|current| current == expected)
            {
                return false;
            }
        }
        engine_objects_are_current(&objects)
    })
    .await
    .unwrap_or(false);
    (current, started.elapsed())
}

async fn fragment_index_engine_inner() -> FragmentIndexEngine {
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
        bounded_command_output(command).await
    };
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
        match bounded_command_output(command).await {
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
                object_digests.push(object_digest);
                objects.push((path, version));
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
    FragmentIndexEngine {
        digest: hex::encode(digest.finalize()),
        objects: objects.into(),
        usable,
    }
}

/// A Fontconfig enumeration with the two costs it incurred kept apart: the
/// `fc-list`/`fc-conflist` children and the stat loop over what they named.
/// The caller charges each series once for the whole attestation.
struct FontEnumeration {
    engine: FragmentIndexEngine,
    spawn_elapsed: Duration,
    stat_elapsed: Duration,
}

async fn font_render_engine_inner() -> FontEnumeration {
    let probe_started = Instant::now();
    let mut digest = Sha256::new();
    digest.update(b"plurx/font-render/engine-v1\0");
    let mut usable = true;
    let mut paths = Vec::new();

    let mut font_list = tokio::process::Command::new("fc-list");
    font_list.arg("--format=%{file}\n");
    match bounded_command_output_with_timeout(font_list, FONT_ENGINE_PROBE_TIMEOUT).await {
        Ok(output) => {
            digest.update((output.stdout.len() as u64).to_be_bytes());
            digest.update(&output.stdout);
            paths.extend(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(str::trim)
                    .filter(|path| path.starts_with('/'))
                    .map(std::path::PathBuf::from),
            );
        }
        Err(error) => {
            usable = false;
            digest.update(error.as_bytes());
        }
    }

    let configuration = tokio::process::Command::new("fc-conflist");
    match bounded_command_output_with_timeout(configuration, FONT_ENGINE_PROBE_TIMEOUT).await {
        Ok(output) => {
            digest.update((output.stdout.len() as u64).to_be_bytes());
            digest.update(&output.stdout);
            paths.extend(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter_map(|line| line.strip_prefix("+ "))
                    .filter_map(|line| line.split_once(": ").map(|(path, _)| path))
                    .filter(|path| path.starts_with('/'))
                    .map(std::path::PathBuf::from),
            );
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
    FontEnumeration {
        engine: FragmentIndexEngine {
            digest: hex::encode(digest.finalize()),
            objects: objects.into(),
            usable,
        },
        spawn_elapsed,
        stat_elapsed,
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

struct BoundedOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
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
    mut command: tokio::process::Command,
    timeout: Duration,
    max_bytes: u64,
    label: &'static str,
    work: crate::process_control::ChildWork,
) -> Result<BoundedOutput, String> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let (mut child, child_job) = crate::process_control::spawn_job_owned(&mut command, work)
        .map_err(|error| error.to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "engine probe has no stdout".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "engine probe has no stderr".to_owned())?;
    let collect = async move {
        let _child_job = child_job;
        let (stdout, stderr, status) = tokio::join!(
            read_bounded_with_limit(stdout, max_bytes, label),
            read_bounded_with_limit(stderr, max_bytes, label),
            child.wait()
        );
        let status = status.map_err(|error| error.to_string())?;
        if !status.success() {
            return Err(format!("engine probe exited {status}"));
        }
        Ok(BoundedOutput {
            stdout: stdout?,
            stderr: stderr?,
        })
    };
    tokio::time::timeout(timeout, collect)
        .await
        .map_err(|_| format!("{label} timed out after {} seconds", timeout.as_secs()))?
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
    Ok((object.finalize().to_vec(), version))
}

fn engine_path_version(path: &std::path::Path) -> Result<String, String> {
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
async fn probe_dovi_reshape_graph(encoder: Encoder) -> bool {
    let filter = format!(
        "tonemapx=tonemap=bt2390:transfer=bt709:matrix=bt709:primaries=bt709:range=tv:format=yuv420p:apply_dovi=1,scale=64:64,format=yuv420p{}",
        encoder
            .filter_suffix_for(OutputGrade::Sdr)
            .map(|s| format!(",{s}"))
            .unwrap_or_default()
    );
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    command
        .args(["-hide_banner", "-loglevel", "error"])
        .args(encoder.init_args())
        .args(["-f", "lavfi", "-i", "color=size=64x64:rate=1:color=black"])
        .args(["-frames:v", "1", "-vf", &filter])
        .args(encoder.encode_args(1_000, EffectiveRateControl::Vbr, false, None))
        .args(["-f", "null", "-"])
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
            let encoder = Encoder::Qsv;
            let Some(upload) = encoder.filter_suffix_for(OutputGrade::Hdr10) else {
                return false;
            };
            let mut command = tokio::process::Command::new(ffmpeg_bin());
            command
                .kill_on_drop(true)
                .args(["-hide_banner", "-loglevel", "error"])
                .args(encoder.init_args())
                .args(["-f", "lavfi", "-i", "color=size=64x64:rate=1:color=black"])
                .args(["-frames:v", "1", "-vf", upload])
                .args(encoder.encode_args_for(
                    OutputGrade::Hdr10,
                    20_000,
                    EffectiveRateControl::Vbr,
                    false,
                    None,
                ))
                .args(["-f", "null", "-"]);
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
            let Some(upload) = encoder.filter_suffix_for(OutputGrade::Hdr10) else {
                return false;
            };
            let filter = upload.to_owned();
            let mut command = tokio::process::Command::new(ffmpeg_bin());
            command
                .kill_on_drop(true)
                .args(["-hide_banner", "-loglevel", "error"])
                .args(encoder.init_args())
                .args(["-f", "lavfi", "-i", "color=size=64x64:rate=1:color=black"])
                .args(["-frames:v", "1", "-vf", &filter])
                .args(encoder.encode_args_for(
                    OutputGrade::Hdr10,
                    20_000,
                    EffectiveRateControl::Vbr,
                    false,
                    None,
                ))
                .args(["-f", "null", "-"]);
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
) -> bool {
    let enabled = dovi_probe_output(file, true, class).await;
    let disabled = dovi_probe_output(file, false, class).await;
    match (enabled, disabled) {
        (Ok(enabled), Ok(disabled)) if enabled != disabled => true,
        (Ok(_), Ok(_)) => {
            tracing::warn!(file = %file.path.display(), "Dolby Vision RPU mutation did not change sampled pixels");
            false
        }
        (enabled, disabled) => {
            tracing::warn!(file = %file.path.display(), ?enabled, ?disabled, "could not prove Dolby Vision RPU pixel reshaping");
            false
        }
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
mod tests {
    /// Admission as a boolean. Production reads the richer comparison; these
    /// regressions are about the verdict, which must stay identical.
    fn probes_describe_same_input(stored: &str, held: &str) -> Result<bool, String> {
        Ok(super::compare_probe_documents(stored, held)?.same)
    }

    /// The Dolby Vision proof consumes non-comment frame hashes from stdout.
    /// Keep the fixture independent of private media while exercising the
    /// same ffmpeg output shape and the production process helper.
    #[tokio::test]
    #[ignore = "needs ffmpeg"]
    async fn dovi_probe_output_receives_frame_hashes() {
        let mut command = tokio::process::Command::new(super::ffmpeg_bin());
        command
            .args(["-hide_banner", "-loglevel", "error"])
            .args(["-f", "lavfi", "-i", "testsrc=size=64x64:rate=1:duration=2"])
            .args(["-frames:v", "2", "-an", "-f", "framemd5", "-"]);
        let output = crate::process_control::output_job_owned(
            &mut command,
            crate::process_control::ChildWork::background("test"),
        )
        .await
        .expect("framemd5 probe output");
        assert!(
            output.status.success(),
            "framemd5 probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let hashes: Vec<_> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .map(str::to_owned)
            .collect();
        assert!(!hashes.is_empty(), "framemd5 stdout had no frame hashes");
    }

    /// A production file on `media1`: 4K HEVC Dolby Vision Profile 8 in MP4
    /// with E-AC-3 Atmos, scanned by the reporter the daemon used then —
    /// which is not the one it probes the held descriptor with now.
    fn legacy_reporter_probe() -> serde_json::Value {
        serde_json::from_str(
            r#"{
            "streams": [
                {"index": 0, "codec_type": "video", "codec_name": "hevc",
                 "profile": "Main 10", "codec_tag_string": "hvc1",
                 "width": 3840, "height": 2076, "coded_width": 3840, "coded_height": 2076,
                 "pix_fmt": "yuv420p10le", "level": 153, "color_range": "tv",
                 "color_space": "bt2020nc", "color_transfer": "smpte2084",
                 "color_primaries": "bt2020", "chroma_location": "topleft",
                 "field_order": "progressive", "has_b_frames": 2,
                 "extradata_size": 110, "nal_length_size": "4",
                 "r_frame_rate": "24000/1001", "avg_frame_rate": "24000/1001",
                 "time_base": "1/24000", "start_pts": 0, "start_time": "0.000000",
                 "closed_captions": 0, "film_grain": 0, "refs": 1,
                 "disposition": {"default": 1, "forced": 0, "attached_pic": 0,
                                 "hearing_impaired": 0, "visual_impaired": 0},
                 "tags": {"language": "eng", "handler_name": "VideoHandler",
                          "vendor_id": "[0][0][0][0]"},
                 "side_data_list": [{"side_data_type": "DOVI configuration record",
                                     "dv_version_major": 1, "dv_version_minor": 0,
                                     "dv_profile": 8, "dv_level": 6,
                                     "rpu_present_flag": 1, "el_present_flag": 0,
                                     "bl_present_flag": 1,
                                     "dv_bl_signal_compatibility_id": 1}]},
                {"index": 1, "codec_type": "audio", "codec_name": "eac3",
                 "codec_tag_string": "ec-3", "channels": 6, "sample_rate": "48000",
                 "channel_layout": "5.1(side)", "sample_fmt": "fltp",
                 "time_base": "1/48000", "start_pts": 0, "start_time": "0.000000",
                 "disposition": {"default": 1, "forced": 0, "attached_pic": 0,
                                 "hearing_impaired": 0, "visual_impaired": 0},
                 "tags": {"language": "eng", "handler_name": "SoundHandler",
                          "vendor_id": "[0][0][0][0]"}}
            ],
            "chapters": [{"id": 0, "time_base": "1/1000", "start": 0,
                          "start_time": "0.000000", "end": 600000,
                          "end_time": "600.000000"}],
            "format": {"filename": "/media/movies/Harbor Lights (2024)/Harbor Lights (2024) WEBDL-2160p.mp4",
                       "format_name": "mov,mp4,m4a,3gp,3g2,mj2", "nb_streams": 2,
                       "duration": "11361.520000", "size": "81604378624",
                       "bit_rate": "2231402"}
        }"#,
        )
        .expect("fixture")
    }

    /// The same bytes through the reporter the daemon probes with now: every
    /// one of these differences was measured on `media1` on 2026-09-17 between
    /// FFprobe 5.1.9 and jellyfin-ffmpeg 8.1.2 over one unchanged file.
    fn current_reporter_probe() -> serde_json::Value {
        let mut current = legacy_reporter_probe();
        current["format"]["filename"] = serde_json::json!("/dev/fd/3");
        current["format"]["nb_stream_groups"] = serde_json::json!(0);
        current["format"]["duration"] = serde_json::json!("11361.477000");
        current["format"]["bit_rate"] = serde_json::json!("2231440");
        for stream in current["streams"].as_array_mut().expect("streams") {
            let fields = stream.as_object_mut().expect("stream");
            for dropped in ["closed_captions", "film_grain", "refs"] {
                fields.remove(dropped);
            }
            fields.insert("view_ids_available".to_owned(), serde_json::json!(""));
            fields.insert("view_pos_available".to_owned(), serde_json::json!(""));
            stream["disposition"]["multilayer"] = serde_json::json!(0);
            stream["disposition"]["non_diegetic"] = serde_json::json!(0);
            stream["tags"]
                .as_object_mut()
                .expect("tags")
                .remove("vendor_id");
        }
        current["streams"][0]["side_data_list"][0]["dv_md_compression"] = serde_json::json!("none");
        current["streams"][1]["profile"] = serde_json::json!("Dolby Digital Plus + Dolby Atmos");
        current["streams"][1]["initial_padding"] = serde_json::json!(0);
        current["streams"][1]["mime_codec_string"] = serde_json::json!("ec-3");
        current["streams"][1]["dmix_mode"] = serde_json::json!("0");
        current["streams"][1]["start_pts"] = serde_json::json!(-5);
        current["streams"][1]["start_time"] = serde_json::json!("-0.005000");
        current["streams"][1]["tags"]["name"] = serde_json::json!("GROUP DDP5.1 Atmos");
        plurx_core::scan::probe::stamp_reporter(&mut current, "ffprobe version 8.1.2-Jellyfin");
        current
    }

    /// The production failure this exists to stop: three refusals on `media1`
    /// inside four minutes, on a file nothing had touched, because the scan
    /// and the held-descriptor probe came from different FFprobe builds. 5,061
    /// of the 5,955 files in that library carry a scan from the older one, so
    /// this is not one title.
    #[test]
    fn a_different_reporter_admits_its_schema_drift_over_the_same_bytes() {
        let legacy = legacy_reporter_probe().to_string();
        let current = current_reporter_probe().to_string();
        let comparison = super::compare_probe_documents(&legacy, &current).expect("compare");
        assert!(comparison.same, "{:?}", comparison.differences);
        assert!(comparison.admitted_on_reporter_drift);
        assert!(comparison.differences.is_empty());
        // Symmetric between two named builds, because a replicated catalog can
        // be newer than the node reading it.
        let mut legacy_named: serde_json::Value = serde_json::from_str(&legacy).expect("fixture");
        plurx_core::scan::probe::stamp_reporter(
            &mut legacy_named,
            "ffprobe version 5.1.9-0+deb12u1",
        );
        let reversed =
            super::compare_probe_documents(&current, &legacy_named.to_string()).expect("compare");
        assert!(reversed.same, "{:?}", reversed.differences);
        assert!(reversed.admitted_on_reporter_drift);
        let forward =
            super::compare_probe_documents(&legacy_named.to_string(), &current).expect("compare");
        assert!(forward.same, "{:?}", forward.differences);
    }

    /// The reason the comparison exists is unchanged: a same-size, same-second
    /// replacement must not pair fresh bytes with stale geometry, tracks,
    /// cadence or color. Every one of these is still a refusal across the same
    /// reporter drift that the case above admits whole.
    #[test]
    fn a_different_reporter_still_refuses_every_measured_media_change() {
        let legacy = legacy_reporter_probe().to_string();
        let drifted = current_reporter_probe();
        let control =
            super::compare_probe_documents(&legacy, &drifted.to_string()).expect("control");
        assert!(control.same);
        // The premise of every case below: this pair reaches the narrower
        // comparison. Without it the loop would only prove that refusal still
        // works on the whole-document path.
        assert!(control.admitted_on_reporter_drift);
        for (pointer, replacement) in [
            ("/streams/0/width", serde_json::json!(1920)),
            ("/streams/0/height", serde_json::json!(1080)),
            ("/streams/0/pix_fmt", serde_json::json!("yuv420p")),
            ("/streams/0/codec_name", serde_json::json!("h264")),
            ("/streams/0/profile", serde_json::json!("Main")),
            ("/streams/0/level", serde_json::json!(120)),
            ("/streams/0/color_transfer", serde_json::json!("bt709")),
            ("/streams/0/color_primaries", serde_json::json!("bt709")),
            ("/streams/0/r_frame_rate", serde_json::json!("30000/1001")),
            ("/streams/0/avg_frame_rate", serde_json::json!("30000/1001")),
            ("/streams/0/time_base", serde_json::json!("1/30000")),
            ("/streams/0/field_order", serde_json::json!("tt")),
            ("/streams/0/codec_tag_string", serde_json::json!("dvh1")),
            ("/streams/0/disposition/default", serde_json::json!(0)),
            (
                "/streams/0/side_data_list/0/dv_profile",
                serde_json::json!(5),
            ),
            (
                "/streams/0/side_data_list/0/bl_present_flag",
                serde_json::json!(0),
            ),
            ("/streams/1/codec_type", serde_json::json!("data")),
            ("/streams/1/codec_name", serde_json::json!("ac3")),
            ("/streams/1/channels", serde_json::json!(8)),
            ("/streams/1/channel_layout", serde_json::json!("7.1")),
            ("/streams/1/sample_rate", serde_json::json!("44100")),
            ("/streams/1/sample_fmt", serde_json::json!("s16")),
            ("/streams/1/tags/language", serde_json::json!("fra")),
            ("/streams/1/disposition/forced", serde_json::json!(1)),
            ("/format/size", serde_json::json!("81604378625")),
            ("/format/nb_streams", serde_json::json!(3)),
            ("/format/format_name", serde_json::json!("matroska,webm")),
            ("/format/duration", serde_json::json!("11000.000000")),
            ("/chapters/0/end", serde_json::json!(660000)),
        ] {
            let mut held = drifted.clone();
            *held.pointer_mut(pointer).expect(pointer) = replacement;
            let comparison =
                super::compare_probe_documents(&legacy, &held.to_string()).expect(pointer);
            assert!(!comparison.same, "{pointer} is a measured media fact");
            assert!(!comparison.admitted_on_reporter_drift);
        }
        let mut fewer = drifted.clone();
        fewer["streams"].as_array_mut().expect("streams").pop();
        assert!(
            !super::compare_probe_documents(&legacy, &fewer.to_string())
                .expect("track count")
                .same
        );
        let mut chaptered = drifted.clone();
        chaptered["chapters"]
            .as_array_mut()
            .expect("chapters")
            .push(
                serde_json::json!({"id": 1, "time_base": "1/1000", "start": 600000,
                                     "start_time": "600.000000", "end": 1200000,
                                     "end_time": "1200.000000"}),
            );
        assert!(
            !super::compare_probe_documents(&legacy, &chaptered.to_string())
                .expect("added chapter")
                .same
        );
    }

    /// Every fact the projection names is compared strictly, including its
    /// absence. A replacement that simply does not report a property is not
    /// evidence that the property is unknown — the stored scan measured it.
    #[test]
    fn a_fact_only_one_document_reports_is_a_refusal_not_an_unknown() {
        let legacy = legacy_reporter_probe().to_string();
        for pointer in [
            "/streams/0/color_range",
            "/streams/0/color_transfer",
            "/streams/0/color_primaries",
            "/streams/0/pix_fmt",
            "/streams/0/level",
            "/streams/0/profile",
            "/streams/0/extradata_size",
            "/streams/0/field_order",
            "/streams/1/channel_layout",
            "/streams/1/channels",
            "/streams/1/sample_rate",
            "/streams/1/tags/language",
            "/format/duration",
            "/format/size",
            "/format/format_name",
        ] {
            let mut held = current_reporter_probe();
            let (parent, field) = pointer.rsplit_once('/').expect("pointer");
            held.pointer_mut(parent)
                .expect(parent)
                .as_object_mut()
                .expect(parent)
                .remove(field)
                .unwrap_or_else(|| panic!("{pointer} is in the fixture"));
            assert!(
                !super::compare_probe_documents(&legacy, &held.to_string())
                    .expect(pointer)
                    .same,
                "{pointer} reported only by the scan must refuse"
            );
            let mut stored = legacy_reporter_probe();
            stored
                .pointer_mut(parent)
                .expect(parent)
                .as_object_mut()
                .expect(parent)
                .remove(field);
            assert!(
                !super::compare_probe_documents(
                    &stored.to_string(),
                    &current_reporter_probe().to_string()
                )
                .expect(pointer)
                .same,
                "{pointer} reported only by the held probe must refuse"
            );
        }
    }

    /// Side data is carried whole and keyed by type, so a record this
    /// projection never heard of still refuses when it appears or disappears.
    /// HDR10+ is the case that matters: `detect_hdr_format` classifies a
    /// source from a dynamic-metadata record alone, and an HDR10+ title
    /// replaced by a plain HDR10 one differs in nothing else.
    #[test]
    fn a_side_data_record_that_appears_or_disappears_refuses() {
        let legacy_base = legacy_reporter_probe();
        for record in [
            serde_json::json!({"side_data_type": "HDR Dynamic Metadata SMPTE2094-40 (HDR10+)",
                               "application version": 1}),
            serde_json::json!({"side_data_type": "Mastering display metadata",
                               "max_luminance": "1000/1", "min_luminance": "1/10000"}),
            serde_json::json!({"side_data_type": "Content light level metadata",
                               "max_content": 1000, "max_average": 400}),
            serde_json::json!({"side_data_type": "Display Matrix", "rotation": -90}),
            serde_json::json!({"side_data_type": "Stereo 3D", "type": "side by side"}),
        ] {
            let mut stored = legacy_base.clone();
            stored["streams"][0]["side_data_list"]
                .as_array_mut()
                .expect("side data")
                .push(record.clone());
            assert!(
                !super::compare_probe_documents(
                    &stored.to_string(),
                    &current_reporter_probe().to_string()
                )
                .expect("compare")
                .same,
                "a dropped {record} must refuse"
            );
            let mut held = current_reporter_probe();
            held["streams"][0]["side_data_list"]
                .as_array_mut()
                .expect("side data")
                .push(record.clone());
            assert!(
                !super::compare_probe_documents(&legacy_base.to_string(), &held.to_string())
                    .expect("compare")
                    .same,
                "an added {record} must refuse"
            );
        }
        // A Dolby Vision record whose delivery facts moved is still a refusal
        // even though the record type is unchanged.
        let mut held = current_reporter_probe();
        held["streams"][0]["side_data_list"][0]["dv_bl_signal_compatibility_id"] =
            serde_json::json!(4);
        assert!(
            !super::compare_probe_documents(&legacy_base.to_string(), &held.to_string())
                .expect("compare")
                .same
        );
    }

    /// A stream list that is not an array cannot be projected, so the
    /// whole-document verdict has to stand rather than a comparison of
    /// nothing at all.
    #[test]
    fn a_malformed_stream_list_keeps_the_whole_document_verdict() {
        let legacy = legacy_reporter_probe().to_string();
        for broken in [
            serde_json::json!({}),
            serde_json::Value::Null,
            serde_json::json!("two"),
        ] {
            let mut held = current_reporter_probe();
            held["streams"] = broken.clone();
            let comparison =
                super::compare_probe_documents(&legacy, &held.to_string()).expect("compare");
            assert!(!comparison.same, "streams {broken} must refuse");
            assert!(!comparison.admitted_on_reporter_drift);
        }
        let mut held = current_reporter_probe();
        held.as_object_mut().expect("document").remove("streams");
        assert!(
            !super::compare_probe_documents(&legacy, &held.to_string())
                .expect("compare")
                .same
        );
    }

    /// The tolerance is relative, so it cannot swallow a quarter of a short
    /// clip, and it is exercised at its own boundary.
    #[test]
    fn the_duration_tolerance_is_relative_and_exact_at_its_boundary() {
        let legacy = legacy_reporter_probe();
        let stored_seconds = 11361.520_f64;
        let tolerance = (stored_seconds * super::FACT_DURATION_DRIFT_FRACTION).clamp(
            super::FACT_DURATION_TOLERANCE_MIN_SECONDS,
            super::FACT_DURATION_TOLERANCE_MAX_SECONDS,
        );
        assert!((tolerance - 1.1361520).abs() > f64::EPSILON || tolerance <= 1.0);
        for (delta, admitted) in [
            (0.043_f64, true),
            (tolerance, true),
            (tolerance + 0.001, false),
            (300.0, false),
        ] {
            let mut held = current_reporter_probe();
            held["format"]["duration"] =
                serde_json::json!(format!("{:.6}", stored_seconds - delta));
            assert_eq!(
                super::compare_probe_documents(&legacy.to_string(), &held.to_string())
                    .expect("compare")
                    .same,
                admitted,
                "duration delta {delta}"
            );
        }
        // A four-second clip gets the floor, not a quarter of itself.
        let mut short_stored = legacy_reporter_probe();
        short_stored["format"]["duration"] = serde_json::json!("4.000000");
        let mut short_held = current_reporter_probe();
        short_held["format"]["duration"] = serde_json::json!("3.100000");
        assert!(
            !super::compare_probe_documents(&short_stored.to_string(), &short_held.to_string())
                .expect("compare")
                .same,
            "a 0.9s change in a 4s clip is not reporter drift"
        );
        short_held["format"]["duration"] = serde_json::json!("3.980000");
        assert!(
            super::compare_probe_documents(&short_stored.to_string(), &short_held.to_string())
                .expect("compare")
                .same,
            "20ms on a 4s clip is inside the floor"
        );
        // A duration neither side can state is not silently uncompared.
        for unparsable in [
            serde_json::json!("N/A"),
            serde_json::json!("nan"),
            serde_json::json!("inf"),
        ] {
            let mut held = current_reporter_probe();
            held["format"]["duration"] = unparsable.clone();
            assert!(
                !super::compare_probe_documents(&legacy.to_string(), &held.to_string())
                    .expect("compare")
                    .same,
                "duration {unparsable} must refuse"
            );
        }
    }

    /// The held probe naming itself is what proves a different reporter. A
    /// probe that could not name itself proves nothing, and must not be read
    /// as a difference — that direction weakens the gate.
    #[test]
    fn an_unnamed_held_probe_is_not_proof_of_a_different_reporter() {
        let mut stored = legacy_reporter_probe();
        plurx_core::scan::probe::stamp_reporter(&mut stored, "ffprobe version 8.1.2-Jellyfin");
        let mut held: serde_json::Value = current_reporter_probe();
        held.as_object_mut()
            .expect("document")
            .remove(plurx_core::scan::probe::REPORTER_FIELD);
        // Identical bytes, one stamped scan, one probe that could not say who
        // it was: the whole-document comparison stands, drift and all.
        let comparison =
            super::compare_probe_documents(&stored.to_string(), &held.to_string()).expect("same");
        assert!(!comparison.same);
        assert!(!comparison.admitted_on_reporter_drift);
    }

    /// The refusal sentence names the stamp rather than calling it unknown,
    /// because on a library scanned before this landed it is the most common
    /// difference an operator will read.
    #[test]
    fn the_reporter_stamp_is_nameable_in_a_refusal() {
        let mut stored = legacy_reporter_probe();
        stored["streams"][0]["width"] = serde_json::json!(1920);
        let mut held = legacy_reporter_probe();
        plurx_core::scan::probe::stamp_reporter(&mut held, "ffprobe version 8.1.2-Jellyfin");
        held["streams"][1]["index"] = serde_json::json!(7);
        let rendered = super::compare_probe_documents(&stored.to_string(), &held.to_string())
            .expect("compare")
            .rendered_differences();
        assert!(rendered.contains("/plurx_probe_reporter"), "{rendered}");
        assert!(!rendered.contains("<unknown-field>"), "{rendered}");
    }

    /// The relaxation is bought by evidence and nothing else. Two documents
    /// from the same reporter are still compared whole, so a field that build
    /// reports on both sides still refuses when it moves.
    #[test]
    fn the_same_reporter_still_demands_the_whole_document() {
        let mut stored = legacy_reporter_probe();
        plurx_core::scan::probe::stamp_reporter(&mut stored, "ffprobe version 8.1.2-Jellyfin");
        let mut held = stored.clone();
        held["streams"][0]["refs"] = serde_json::json!(2);
        let comparison =
            super::compare_probe_documents(&stored.to_string(), &held.to_string()).expect("same");
        assert!(!comparison.same);
        assert!(!comparison.admitted_on_reporter_drift);
        assert_eq!(comparison.rendered_differences(), "/streams/0/refs value");
        let mut unstamped = legacy_reporter_probe();
        unstamped["streams"][0]["refs"] = serde_json::json!(2);
        assert!(
            !super::compare_probe_documents(
                &legacy_reporter_probe().to_string(),
                &unstamped.to_string()
            )
            .expect("two unknown reporters")
            .same,
            "an unproved reporter difference is not a reporter difference"
        );
    }

    /// Sub-second duration drift over unchanged bytes was measured; a real
    /// re-encode is not sub-second.
    #[test]
    fn duration_drift_is_bounded_by_a_second() {
        let legacy = legacy_reporter_probe().to_string();
        for (duration, admitted) in [
            ("11361.477000", true),
            ("11362.400000", true),
            ("11363.000000", false),
            ("11360.000000", false),
        ] {
            let mut held = current_reporter_probe();
            held["format"]["duration"] = serde_json::json!(duration);
            assert_eq!(
                super::compare_probe_documents(&legacy, &held.to_string())
                    .expect(duration)
                    .same,
                admitted,
                "duration {duration}"
            );
        }
    }

    /// The fact comparison pairs streams by position, so an unproved pairing
    /// gets no relaxation at all: the whole-document verdict stands.
    #[test]
    fn an_unpairable_stream_list_keeps_the_whole_document_verdict() {
        let legacy = legacy_reporter_probe().to_string();
        for broken in [
            serde_json::json!(5),
            serde_json::Value::Null,
            serde_json::json!(-1),
            serde_json::json!("1"),
        ] {
            let mut held = current_reporter_probe();
            held["streams"][1]["index"] = broken.clone();
            assert!(
                !super::compare_probe_documents(&legacy, &held.to_string())
                    .expect("compare")
                    .same,
                "index {broken} is not a pairing"
            );
        }
        let mut held = current_reporter_probe();
        held["streams"][1]
            .as_object_mut()
            .expect("stream")
            .remove("index");
        assert!(
            !super::compare_probe_documents(&legacy, &held.to_string())
                .expect("compare")
                .same
        );
    }

    /// A reporter that cannot name the codec is not describing the stream the
    /// other one described, so the spine is never treated as unknown.
    #[test]
    fn a_one_sided_codec_identity_is_a_refusal_not_an_unknown() {
        let legacy = legacy_reporter_probe().to_string();
        for field in ["codec_name", "codec_type"] {
            let mut held = current_reporter_probe();
            held["streams"][1]
                .as_object_mut()
                .expect("stream")
                .remove(field);
            let comparison =
                super::compare_probe_documents(&legacy, &held.to_string()).expect(field);
            assert!(!comparison.same, "{field} omitted on one side");
        }
    }

    /// reference film G's track arrangement: 4K HEVC, a default TrueHD Atmos track, a
    /// second E-AC-3 track, and two AC-3 tracks. Only the two Atmos-capable
    /// codecs have a derived profile a newer reporter can add.
    fn atmos_capable_probe(
        truehd_profile: Option<serde_json::Value>,
        eac3_profile: Option<serde_json::Value>,
    ) -> String {
        let mut document = serde_json::json!({
            "streams": [
                {"index": 0, "codec_type": "video", "codec_name": "hevc",
                 "width": 3840, "height": 2160, "r_frame_rate": "24000/1001"},
                {"index": 1, "codec_type": "audio", "codec_name": "truehd",
                 "channels": 8, "sample_rate": "48000", "channel_layout": "7.1"},
                {"index": 2, "codec_type": "audio", "codec_name": "eac3",
                 "channels": 6, "sample_rate": "48000", "channel_layout": "5.1(side)"},
                {"index": 3, "codec_type": "audio", "codec_name": "ac3", "channels": 6},
                {"index": 4, "codec_type": "audio", "codec_name": "ac3", "channels": 2}
            ],
            "chapters": [],
            "format": {"filename": "/media/library/w.mkv", "nb_streams": 5,
                       "duration": "9360.000000", "size": "77309411328"}
        });
        if let Some(profile) = truehd_profile {
            document["streams"][1]["profile"] = profile;
        }
        if let Some(profile) = eac3_profile {
            document["streams"][2]["profile"] = profile;
        }
        document.to_string()
    }

    fn eac3_atmos() -> serde_json::Value {
        serde_json::json!("Dolby Digital Plus + Dolby Atmos")
    }

    fn truehd_atmos() -> serde_json::Value {
        serde_json::json!("Dolby TrueHD + Dolby Atmos")
    }

    /// The reported incident. A July scan omitted `profile` on the E-AC-3
    /// track; the playback node's newer FFprobe derives the Atmos name from
    /// the extension type A flag. Nothing about the file changed, so the
    /// encoded session must be admitted — in either direction, because a
    /// replicated catalog can be newer than the node that reads it.
    #[test]
    fn held_probe_comparison_admits_the_legacy_eac3_atmos_profile_omission() {
        let legacy = atmos_capable_probe(None, None);
        let current = atmos_capable_probe(Some(truehd_atmos()), Some(eac3_atmos()));
        assert!(probes_describe_same_input(&legacy, &current).expect("legacy scan admitted"));
        assert!(probes_describe_same_input(&current, &legacy).expect("newer catalog admitted"));
        let eac3_only = atmos_capable_probe(None, Some(eac3_atmos()));
        assert!(probes_describe_same_input(&legacy, &eac3_only).expect("E-AC-3 alone"));
        assert!(probes_describe_same_input(&eac3_only, &legacy).expect("E-AC-3 alone reversed"));
        let truehd_only = atmos_capable_probe(Some(truehd_atmos()), None);
        assert!(probes_describe_same_input(&legacy, &truehd_only).expect("TrueHD alone"));
        assert!(probes_describe_same_input(&current, &current).expect("identical reports"));
    }

    /// The exception admits one measured omission and nothing more. Two
    /// reported profiles are always compared, and "missing" is not null, an
    /// empty string, a number, or a name nobody measured.
    #[test]
    fn held_probe_comparison_refuses_profile_differences_outside_the_exception() {
        let legacy = atmos_capable_probe(None, None);
        let atmos = atmos_capable_probe(None, Some(eac3_atmos()));
        let plain = atmos_capable_probe(None, Some(serde_json::json!("Dolby Digital Plus")));
        assert!(!probes_describe_same_input(&atmos, &plain).expect("reported disagreement"));
        assert!(!probes_describe_same_input(&plain, &atmos).expect("reported disagreement"));
        for reported in [
            serde_json::Value::Null,
            serde_json::json!(""),
            serde_json::json!(5),
            serde_json::json!("Dolby Digital Plus + Atmos"),
            truehd_atmos(),
        ] {
            let held = atmos_capable_probe(None, Some(reported.clone()));
            assert!(
                !probes_describe_same_input(&legacy, &held).expect("compare probes"),
                "{reported} must not read as the measured omission"
            );
            assert!(
                !probes_describe_same_input(&held, &legacy).expect("compare probes"),
                "{reported} must not read as the measured omission, reversed"
            );
        }
    }

    /// The pairing is by codec and by an explicit, equal, non-negative integer
    /// index. An Atmos name on any other codec, or a stream list that moved,
    /// buys nothing.
    #[test]
    fn held_probe_comparison_confines_the_atmos_exception_to_its_codecs_and_indices() {
        let base: serde_json::Value =
            serde_json::from_str(&atmos_capable_probe(None, None)).expect("fixture");
        for (at, codec_name, profile) in [
            (3usize, "ac3", "Dolby Digital Plus + Dolby Atmos"),
            (3, "aac", "Dolby Digital Plus + Dolby Atmos"),
            (3, "eac3", "Dolby TrueHD + Dolby Atmos"),
            (0, "hevc", "Dolby TrueHD + Dolby Atmos"),
        ] {
            let mut stored = base.clone();
            stored["streams"][at]["codec_name"] = serde_json::json!(codec_name);
            let mut held = stored.clone();
            held["streams"][at]["profile"] = serde_json::json!(profile);
            assert!(
                !probes_describe_same_input(&stored.to_string(), &held.to_string())
                    .expect("compare probes"),
                "{codec_name} must not earn the Atmos exception"
            );
        }
        for broken in [
            serde_json::Value::Null,
            serde_json::json!(-1),
            serde_json::json!("2"),
            serde_json::json!(2.0),
        ] {
            let mut stored = base.clone();
            let mut held = base.clone();
            held["streams"][2]["profile"] = eac3_atmos();
            stored["streams"][2]["index"] = broken.clone();
            held["streams"][2]["index"] = broken.clone();
            assert!(
                !probes_describe_same_input(&stored.to_string(), &held.to_string())
                    .expect("compare probes"),
                "index {broken} is not an explicit pairing"
            );
        }
        let mut unindexed = base.clone();
        unindexed["streams"][2]
            .as_object_mut()
            .expect("stream")
            .remove("index");
        let mut unindexed_held = unindexed.clone();
        unindexed_held["streams"][2]["profile"] = eac3_atmos();
        assert!(
            !probes_describe_same_input(&unindexed.to_string(), &unindexed_held.to_string())
                .expect("compare probes")
        );
        let mut renumbered = base.clone();
        renumbered["streams"][2]["profile"] = eac3_atmos();
        renumbered["streams"][2]["index"] = serde_json::json!(5);
        assert!(!probes_describe_same_input(
            &atmos_capable_probe(None, None),
            &renumbered.to_string()
        )
        .expect("compare probes"));
        let mut reordered = base.clone();
        reordered["streams"][2]["profile"] = eac3_atmos();
        let streams = reordered["streams"].as_array_mut().expect("streams");
        streams.swap(1, 2);
        assert!(!probes_describe_same_input(
            &atmos_capable_probe(None, None),
            &reordered.to_string()
        )
        .expect("compare probes"));
    }

    /// Everything the encoded recipe actually depends on still refuses, with
    /// the admitted omission present at the same time.
    #[test]
    fn held_probe_comparison_still_refuses_media_changes_beside_the_atmos_omission() {
        let legacy = atmos_capable_probe(None, None);
        let current: serde_json::Value = serde_json::from_str(&atmos_capable_probe(
            Some(truehd_atmos()),
            Some(eac3_atmos()),
        ))
        .expect("fixture");
        assert!(probes_describe_same_input(&legacy, &current.to_string()).expect("control"));
        for (pointer, replacement) in [
            ("/streams/2/channels", serde_json::json!(8)),
            ("/streams/2/sample_rate", serde_json::json!("44100")),
            ("/streams/2/channel_layout", serde_json::json!("7.1")),
            ("/streams/2/codec_name", serde_json::json!("ac3")),
            ("/streams/2/codec_type", serde_json::json!("data")),
            ("/streams/0/width", serde_json::json!(1920)),
            ("/streams/0/height", serde_json::json!(1080)),
            ("/streams/0/r_frame_rate", serde_json::json!("30000/1001")),
            ("/streams/0/codec_name", serde_json::json!("h264")),
            ("/format/duration", serde_json::json!("9000.000000")),
            ("/format/size", serde_json::json!("77309411329")),
        ] {
            let mut held = current.clone();
            *held.pointer_mut(pointer).expect(pointer) = replacement;
            assert!(
                !probes_describe_same_input(&legacy, &held.to_string()).expect("compare probes"),
                "{pointer} is a measured media fact"
            );
        }
        let mut fewer = current.clone();
        fewer["streams"].as_array_mut().expect("streams").pop();
        assert!(!probes_describe_same_input(&legacy, &fewer.to_string()).expect("track count"));
        let mut chaptered = current.clone();
        chaptered["chapters"] = serde_json::json!([
            {"id": 0, "start_time": "0.000000", "end_time": "60.000000"}
        ]);
        assert!(
            !probes_describe_same_input(&legacy, &chaptered.to_string()).expect("added chapter")
        );
    }

    /// A refusal has to be diagnosable without dumping a probe into a log.
    #[test]
    fn held_probe_comparison_names_the_normalized_field_that_refused() {
        let legacy = atmos_capable_probe(None, None);
        let mut held: serde_json::Value = serde_json::from_str(&atmos_capable_probe(
            Some(truehd_atmos()),
            Some(eac3_atmos()),
        ))
        .expect("fixture");
        held["streams"][0]["width"] = serde_json::json!(1920);
        let comparison =
            super::compare_probe_documents(&legacy, &held.to_string()).expect("compare probes");
        assert!(!comparison.same);
        assert!(!comparison.truncated);
        assert_eq!(
            comparison.differences,
            vec![super::ProbeDifference {
                path: "/streams/0/width".to_string(),
                kind: super::ProbeDifferenceKind::Value,
            }]
        );
        assert_eq!(comparison.rendered_differences(), "/streams/0/width value");
        // Removing the deployed exception is what this whole change is about:
        // with it in place the E-AC-3 profile is not a difference at all.
        let admitted = super::compare_probe_documents(
            &legacy,
            &atmos_capable_probe(Some(truehd_atmos()), Some(eac3_atmos())),
        )
        .expect("compare probes");
        assert!(admitted.same);
        assert!(admitted.differences.is_empty());
        // The four kinds are distinguishable.
        let mut typed = held.clone();
        typed["streams"][0]["width"] = serde_json::json!("3840");
        let kinds = super::compare_probe_documents(&legacy, &typed.to_string())
            .expect("compare probes")
            .differences;
        assert_eq!(kinds[0].kind, super::ProbeDifferenceKind::Type);
        let mut shorter = held.clone();
        shorter["streams"].as_array_mut().expect("streams").pop();
        assert_eq!(
            super::compare_probe_documents(&legacy, &shorter.to_string())
                .expect("compare probes")
                .differences[0],
            super::ProbeDifference {
                path: "/streams".to_string(),
                kind: super::ProbeDifferenceKind::Length,
            }
        );
        let mut absent = held.clone();
        absent["streams"][0]
            .as_object_mut()
            .expect("stream")
            .remove("width");
        assert_eq!(
            super::compare_probe_documents(&legacy, &absent.to_string())
                .expect("compare probes")
                .differences[0]
                .kind,
            super::ProbeDifferenceKind::Missing
        );
    }

    /// The diagnostic is written to an operator log, so it may never carry a
    /// title, a pathname, a private container tag name, or an unbounded list.
    #[test]
    fn held_probe_comparison_diagnostics_stay_bounded_and_carry_no_private_text() {
        let base: serde_json::Value =
            serde_json::from_str(&atmos_capable_probe(None, None)).expect("fixture");
        let mut stored = base.clone();
        stored["format"]["tags"] = serde_json::json!({
            "title": "reference film G (2024) Private Cut",
            "COMPANY_NAME": "a private label"
        });
        let mut held = base.clone();
        held["format"]["tags"] = serde_json::json!({
            "title": "reference film G (2024) Other Cut",
            "COMPANY_NAME": "a different private label"
        });
        let comparison = super::compare_probe_documents(&stored.to_string(), &held.to_string())
            .expect("compare probes");
        assert!(!comparison.same);
        assert!(!comparison.differences.is_empty());
        for difference in &comparison.differences {
            assert_eq!(difference.path, "/format/tags/<field>", "{difference:?}");
        }
        let rendered = comparison.rendered_differences();
        for secret in [
            "reference film G",
            "title",
            "COMPANY_NAME",
            "private",
            "/media/",
        ] {
            assert!(!rendered.contains(secret), "{rendered} leaked {secret}");
        }
        // An unknown field a future FFprobe adds is named by category only.
        let mut many_stored = base.clone();
        let mut many_held = base.clone();
        for at in 0..12 {
            let key = format!("some_future_field_{at}");
            many_stored["streams"][0][key.as_str()] = serde_json::json!(at);
            many_held["streams"][0][key.as_str()] = serde_json::json!(at + 1);
        }
        let many = super::compare_probe_documents(&many_stored.to_string(), &many_held.to_string())
            .expect("compare probes");
        assert_eq!(many.differences.len(), super::PROBE_DIFFERENCE_LIMIT);
        assert!(many.truncated);
        for difference in &many.differences {
            assert_eq!(difference.path, "/streams/0/<unknown-field>");
        }
        assert!(many.rendered_differences().ends_with("…more"));
        // A pathological nesting depth cannot grow a log line without bound.
        let mut deep_leaf = serde_json::json!("leaf");
        let mut deep_other = serde_json::json!("other leaf");
        for _ in 0..40 {
            deep_leaf = serde_json::json!({"a_private_nested_name": deep_leaf});
            deep_other = serde_json::json!({"a_private_nested_name": deep_other});
        }
        let mut deep_stored = base.clone();
        deep_stored["format"]["tags"] = serde_json::json!({"chain": deep_leaf});
        let mut deep_held = base.clone();
        deep_held["format"]["tags"] = serde_json::json!({"chain": deep_other});
        let deep_comparison =
            super::compare_probe_documents(&deep_stored.to_string(), &deep_held.to_string())
                .expect("compare probes");
        assert!(!deep_comparison.same);
        assert!(!deep_comparison.differences.is_empty());
        for difference in &deep_comparison.differences {
            assert!(
                difference.path.chars().count() <= super::PROBE_PATH_MAX_CHARS,
                "{} is unbounded",
                difference.path
            );
            assert!(!difference.path.contains("a_private_nested_name"));
            assert!(!difference.path.contains("chain"));
        }
    }

    use super::*;

    #[tokio::test]
    async fn encoded_executable_refuses_same_size_mtime_replacement() {
        let base = crate::test_tempdir().expect("engine identity");
        let path = base.path().join("encoder");
        tokio::fs::write(&path, b"encoder-a")
            .await
            .expect("first engine");
        let modified = std::fs::metadata(&path)
            .expect("metadata")
            .modified()
            .expect("mtime");
        let engine = EncodedExecutable::capture_at(path.clone())
            .await
            .expect("capture engine");
        // Exactly the composition `recipe_engine_is_current` uses: the
        // executable is attested inside the engine's blocking batch.
        let closure = EncodedEngine::capture_test_objects(&[], "process-a")
            .await
            .expect("engine closure");
        assert!(closure.is_current_with_executable(&engine).await);
        let replacement = base.path().join("replacement");
        tokio::fs::write(&replacement, b"encoder-b")
            .await
            .expect("new engine");
        std::fs::File::options()
            .write(true)
            .open(&replacement)
            .expect("replacement handle")
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .expect("preserve mtime");
        std::fs::rename(replacement, &path).expect("atomic engine replacement");
        assert!(!closure.is_current_with_executable(&engine).await);
        assert_ne!(
            engine.digest,
            EncodedExecutable::capture_at(path)
                .await
                .expect("new capture")
                .digest
        );
    }

    #[tokio::test]
    async fn encoded_engine_refuses_dependency_replacement_and_isolates_processes() {
        let base = crate::test_tempdir().expect("engine closure");
        let dependency = base.path().join("libcodec");
        tokio::fs::write(&dependency, b"codec-a")
            .await
            .expect("dependency");
        let first =
            EncodedEngine::capture_test_objects(std::slice::from_ref(&dependency), "process-a")
                .await
                .expect("first engine");
        let other_process =
            EncodedEngine::capture_test_objects(std::slice::from_ref(&dependency), "process-b")
                .await
                .expect("other process");
        assert_ne!(first.digest, other_process.digest);
        assert!(first.is_current().await);

        let replacement = base.path().join("replacement");
        tokio::fs::write(&replacement, b"codec-b")
            .await
            .expect("replacement dependency");
        std::fs::rename(replacement, dependency).expect("replace dependency");
        assert!(!first.is_current().await);
    }

    #[tokio::test]
    async fn a_touched_engine_object_is_not_current_after_the_move() {
        let base = crate::test_tempdir().expect("engine closure");
        let first = base.path().join("libcodec-a");
        let second = base.path().join("libcodec-b");
        tokio::fs::write(&first, b"codec-a").await.expect("first");
        tokio::fs::write(&second, b"codec-b").await.expect("second");
        let engine = EncodedEngine::capture_test_objects(&[first.clone(), second], "process-a")
            .await
            .expect("engine");
        assert!(engine.is_current().await);

        std::fs::File::options()
            .write(true)
            .open(first)
            .expect("open first")
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .expect("touch first");
        assert!(!engine.is_current().await);
    }

    #[test]
    fn a_current_engine_is_current_on_the_blocking_pool() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .max_blocking_threads(1)
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let base = crate::test_tempdir().expect("engine closure");
            let dependency = base.path().join("libcodec");
            tokio::fs::write(&dependency, b"codec")
                .await
                .expect("dependency");
            let engine =
                EncodedEngine::capture_test_objects(std::slice::from_ref(&dependency), "process-a")
                    .await
                    .expect("engine");

            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                started_tx.send(()).expect("announce blocking task");
                release_rx.recv().expect("release blocking task");
            });
            started_rx.recv().expect("blocking task started");
            let check = tokio::spawn(async move { engine.is_current().await });
            tokio::time::sleep(Duration::from_millis(25)).await;
            assert!(
                !check.is_finished(),
                "the currentness check must queue behind the occupied blocking pool"
            );
            release_tx.send(()).expect("release blocker");
            blocker.await.expect("blocking task");
            assert!(tokio::time::timeout(Duration::from_secs(1), check)
                .await
                .expect("currentness deadline")
                .expect("currentness task"));
        });
    }

    /// A stale encoder executable must not be detectable without the
    /// blocking pool. While the pool is occupied the whole attestation —
    /// executable included — has to queue; an inline `std::fs::metadata` on
    /// the executable would answer `false` from the runtime thread and the
    /// check would finish immediately.
    #[test]
    fn a_stale_executable_is_detected_on_the_blocking_pool() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .max_blocking_threads(1)
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let base = crate::test_tempdir().expect("engine identity");
            let path = base.path().join("encoder");
            tokio::fs::write(&path, b"encoder-a")
                .await
                .expect("first engine");
            let executable = EncodedExecutable::capture_at(path.clone())
                .await
                .expect("capture engine");
            let closure = EncodedEngine::capture_test_objects(&[], "process-a")
                .await
                .expect("engine closure");
            tokio::fs::write(&path, b"encoder-b-longer")
                .await
                .expect("replace engine");

            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                started_tx.send(()).expect("announce blocking task");
                release_rx.recv().expect("release blocking task");
            });
            started_rx.recv().expect("blocking task started");
            let check =
                tokio::spawn(async move { closure.is_current_with_executable(&executable).await });
            tokio::time::sleep(Duration::from_millis(25)).await;
            assert!(
                !check.is_finished(),
                "the executable must be attested on the blocking pool, not inline on a runtime worker"
            );
            release_tx.send(()).expect("release blocker");
            blocker.await.expect("blocking task");
            assert!(
                !tokio::time::timeout(Duration::from_secs(1), check)
                    .await
                    .expect("currentness deadline")
                    .expect("currentness task"),
                "a replaced executable must withdraw the recipe"
            );
        });
    }

    /// One attestation charges each series once, and a burn recipe's media
    /// dependency closure is charged under `media` even though the recipe is
    /// a font one. Observing per internal batch made the burn path emit three
    /// `font,stat` observations per check and none under `media`.
    #[tokio::test]
    async fn one_attestation_charges_each_series_once_by_what_it_stats() {
        let base = crate::test_tempdir().expect("engine closure");
        let dependency = base.path().join("libcodec");
        let font = base.path().join("font-a");
        tokio::fs::write(&dependency, b"codec")
            .await
            .expect("dependency");
        tokio::fs::write(&font, b"font").await.expect("font");

        let media = EncodedEngine::capture_test_objects(std::slice::from_ref(&dependency), "p")
            .await
            .expect("media engine");
        assert_eq!(
            media.is_current_charged(None).await.1.charged(),
            vec![("media", "stat", 1)],
            "a non-burn check stats the dependency closure and nothing else"
        );

        let fonts = EncodedEngine::capture_test_objects(std::slice::from_ref(&font), "p")
            .await
            .expect("font objects");
        let burn = EncodedEngine {
            digest: media.digest.clone(),
            process_identity: media.process_identity.clone(),
            objects: Arc::clone(&media.objects),
            font_objects: Arc::clone(&fonts.objects),
            font_digest: Some("captured-font-closure".to_owned()),
        };
        assert_eq!(
            burn.is_current_charged(None).await.1.charged(),
            vec![
                ("font", "spawn", 1),
                ("font", "stat", 3),
                ("media", "stat", 1)
            ],
            "a burn check charges its dependency closure to media, and folds its three \
             font stat batches into one observation"
        );
    }

    #[tokio::test]
    async fn font_object_versions_are_computed_in_one_blocking_task() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let base = crate::test_tempdir().expect("font closure");
        let paths = [base.path().join("font-a"), base.path().join("font-b")];
        for path in &paths {
            tokio::fs::write(path, b"font").await.expect("font object");
        }
        let tasks = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&tasks);
        let (versions, _elapsed) =
            font_object_versions_observed(paths.into_iter().collect(), move || {
                observed.fetch_add(1, Ordering::Relaxed);
            })
            .await;
        assert!(versions.errors.is_empty(), "{:?}", versions.errors);
        assert_eq!(versions.objects.len(), 2);
        assert_eq!(tasks.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn engine_attestation_metrics_render_all_four_series() {
        let rendered = engine_attestation_prometheus();
        assert!(rendered.contains("# TYPE plurx_engine_attestation_seconds histogram"));
        for kind in ["media", "font"] {
            for phase in ["spawn", "stat"] {
                assert!(rendered.contains(&format!(
                    "plurx_engine_attestation_seconds_count{{kind=\"{kind}\",phase=\"{phase}\"}}"
                )));
            }
        }
    }

    #[tokio::test]
    async fn a_changed_fontconfig_closure_invalidates_the_retained_recipe() {
        let captured = FragmentIndexEngine {
            digest: "font-closure-a".to_owned(),
            objects: Vec::new().into(),
            usable: true,
        };
        let added_font = FragmentIndexEngine {
            digest: "font-closure-b".to_owned(),
            objects: Vec::new().into(),
            usable: true,
        };

        assert!(font_closure_is_current("font-closure-a", &captured).await.0);
        assert!(
            !font_closure_is_current("font-closure-a", &added_font)
                .await
                .0
        );
    }

    #[test]
    fn font_inventory_keeps_a_load_tolerant_probe_budget() {
        assert!(FONT_ENGINE_PROBE_TIMEOUT >= Duration::from_secs(30));
        assert!(FONT_ENGINE_PROBE_TIMEOUT > ENGINE_PROBE_TIMEOUT);
    }

    #[tokio::test]
    async fn text_renderer_attests_active_font_rules_and_files() {
        plurx_core::testfixtures::require_ffmpeg();
        let engine = EncodedEngine::capture(true)
            .await
            .expect("text renderer attestation");
        assert!(engine.is_current().await);
    }

    #[test]
    fn held_probe_comparison_accepts_empty_chapters_but_detects_chapter_changes() {
        // Production file 1323 has no chapters. Its older scan omitted the
        // chapters key, while the held-source probe explicitly reports [].
        let scanned = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920}],"format":{"duration":"3660.0"}}"#;
        let mut held: serde_json::Value = serde_json::from_str(scanned).expect("fixture");
        held["chapters"] = serde_json::json!([]);
        let without_chapters = held.to_string();
        assert!(probes_describe_same_input(scanned, &held.to_string()).expect("empty chapters"));
        assert!(probes_describe_same_input(&held.to_string(), scanned).expect("omitted chapters"));

        held["chapters"] = serde_json::json!([{
            "id": 0, "time_base": "1/1000", "start": 0, "end": 60000
        }]);
        let with_chapter = held.to_string();
        // Production file 2 has 24 chapters, but its old scan measured none.
        assert!(probes_describe_same_input(scanned, &with_chapter).expect("unmeasured chapters"));
        assert!(probes_describe_same_input(&with_chapter, scanned).expect("unmeasured chapters"));
        assert!(
            !probes_describe_same_input(&without_chapters, &with_chapter).expect("added chapter")
        );
        assert!(
            !probes_describe_same_input(&with_chapter, &without_chapters).expect("removed chapter")
        );
        held["chapters"][0]["end"] = serde_json::json!(61000);
        assert!(
            !probes_describe_same_input(&with_chapter, &held.to_string())
                .expect("changed chapter timing")
        );
        held["chapters"] = serde_json::json!(null);
        assert!(
            !probes_describe_same_input(scanned, &held.to_string()).expect("malformed chapters")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_scan_without_chapters_matches_the_real_held_source_probe() {
        plurx_core::testfixtures::require_ffmpeg();
        let directory = crate::test_tempdir().expect("source fixture");
        let path = directory.path().join("no-chapters.wav");
        // One second of mono 8 kHz PCM, with no chapter metadata.
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&16036u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.resize(16044, 0);
        std::fs::write(&path, wav).expect("write source");
        let metadata = directory.path().join("chapters.txt");
        std::fs::write(
            &metadata,
            ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1000\ntitle=Chapter one\n",
        )
        .expect("chapter metadata");
        let chaptered = directory.path().join("with-chapters.mka");
        let mut muxer = tokio::process::Command::new(ffmpeg_bin());
        muxer
            .args(["-v", "error", "-i"])
            .arg(&path)
            .arg("-i")
            .arg(&metadata)
            .args(["-map_metadata", "1", "-c", "copy"])
            .arg(&chaptered);
        bounded_command_output(muxer)
            .await
            .expect("chapter fixture");
        for (path, chapter_count) in [(&path, 0), (&chaptered, 1)] {
            let mut scanner = tokio::process::Command::new(ffprobe_bin());
            scanner
                .args([
                    "-v",
                    "error",
                    "-print_format",
                    "json",
                    "-show_format",
                    "-show_streams",
                ])
                .arg(path);
            let scanned = bounded_command_output(scanner)
                .await
                .expect("legacy scanner probe");
            let scanned = String::from_utf8(scanned.stdout).expect("probe JSON");
            let stored: serde_json::Value = serde_json::from_str(&scanned).expect("stored probe");
            assert!(stored.get("chapters").is_none());
            let source = std::fs::File::open(path).expect("hold source");
            let held = held_source_probe_json(
                &source,
                crate::process_control::ChildWork::background("test fixture probe"),
            )
            .await
            .expect("descriptor-bound probe");
            let current: serde_json::Value = serde_json::from_str(&held).expect("held probe");
            assert_eq!(
                current["chapters"].as_array().expect("chapter array").len(),
                chapter_count
            );
            assert!(probes_describe_same_input(&scanned, &held).expect("legacy source accepted"));
        }
    }

    #[test]
    fn held_probe_comparison_ignores_descriptor_and_optional_schema_drift() {
        let scanned = r#"{"streams":[{"codec_type":"video","width":1920,"closed_captions":0,"film_grain":0,"refs":1}],"format":{"filename":"/media/a.mkv","duration":"60.0"}}"#;
        let held = r#"{"streams":[{"codec_type":"video","width":1920}],"format":{"filename":"/dev/fd/3","duration":"60.0"}}"#;
        assert!(probes_describe_same_input(scanned, held).expect("compare probes"));
        let replacement = held.replace("1920", "1280");
        assert!(!probes_describe_same_input(scanned, &replacement).expect("detect stale probe"));
        let reported_change = scanned.replace("\"refs\":1", "\"refs\":2");
        assert!(!probes_describe_same_input(scanned, &reported_change)
            .expect("compare reported codec facts"));
    }

    #[test]
    fn channel_playback_repair_ignores_measured_optional_audio_report_omissions() {
        let scanned = r#"{
          "streams":[
            {"index":0,"codec_type":"video","codec_name":"hevc","width":3840,"height":2160},
            {"index":1,"codec_type":"audio","codec_name":"eac3","channels":6,
             "dmix_mode":"ltrt","loro_cmixlev":"-3.000000","loro_surmixlev":"-3.000000",
             "ltrt_cmixlev":"-3.000000","ltrt_surmixlev":"-3.000000",
             "mime_codec_string":"ec-3"}
          ],
          "format":{"filename":"/media/leave-the-world-behind.mkv","duration":"8460.000000"}
        }"#;
        let held = r#"{
          "streams":[
            {"index":0,"codec_type":"video","codec_name":"hevc","width":3840,"height":2160},
            {"index":1,"codec_type":"audio","codec_name":"eac3","channels":6}
          ],
          "format":{"filename":"/dev/fd/3","duration":"8460.000000"}
        }"#;

        assert!(
            probes_describe_same_input(scanned, held).expect("compare the measured schema drift"),
            "optional FFmpeg report availability is not a source replacement"
        );
        assert!(
            !probes_describe_same_input(scanned, &held.replace("3840", "1920"))
                .expect("changed geometry remains significant")
        );
        assert!(
            !probes_describe_same_input(scanned, &held.replace("\"index\":1", "\"index\":2"))
                .expect("changed stream identity remains significant")
        );
        let video_report = scanned.replace(
            "\"height\":2160",
            "\"height\":2160,\"mime_codec_string\":\"hvc1.2.4.L153.B0\"",
        );
        assert!(probes_describe_same_input(&video_report, held)
            .expect("MIME codec labels are optional for video as well as audio"));
    }

    #[test]
    fn held_probe_comparison_accepts_added_video_mime_label_but_detects_media_changes() {
        // File 5405 was scanned before FFmpeg reported a video MIME label.
        // The held-descriptor probe adds avc1.640029 to the same H.264 bytes.
        let scanned = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","width":1920,"height":1080,"level":41}],"format":{"duration":"5542.000000"}}"#;
        let held = scanned.replace(
            "\"level\":41",
            "\"level\":41,\"mime_codec_string\":\"avc1.640029\"",
        );
        assert!(probes_describe_same_input(scanned, &held).expect("compare probes"));
        assert!(probes_describe_same_input(&held, scanned).expect("compare probes"));
        for replacement in [
            held.replace("h264", "hevc"),
            held.replace("1920", "1280"),
            held.replace("\"index\":0", "\"index\":1"),
            held.replace("\"level\":41", "\"level\":40"),
            held.replace("5542.000000", "5543.000000"),
        ] {
            assert!(!probes_describe_same_input(scanned, &replacement).expect("compare probes"));
        }
        assert!(
            !probes_describe_same_input(&held, &held.replace("avc1.640029", "avc1.640028"))
                .expect("compare probes")
        );
    }

    #[test]
    fn held_probe_comparison_accepts_measured_ffprobe_defaults_but_not_nondefaults() {
        let scanned = serde_json::json!({
            "format": {"duration": "7920.0"},
            "streams": [
                {"index": 0, "codec_type": "video", "codec_name": "hevc", "disposition": {"default": 1},
                 "side_data_list": [{"side_data_type": "DOVI configuration record", "dv_profile": 8}]},
                {"index": 1, "codec_type": "audio", "codec_name": "truehd", "channels": 8, "disposition": {"default": 1}}
            ]
        });
        let mut held = scanned.clone();
        held["format"]["nb_stream_groups"] = serde_json::json!(0);
        held["streams"][0]["view_ids_available"] = serde_json::json!("");
        held["streams"][0]["view_pos_available"] = serde_json::json!("");
        held["streams"][0]["side_data_list"][0]["dv_md_compression"] = serde_json::json!("none");
        held["streams"][1]["initial_padding"] = serde_json::json!(0);
        held["streams"][1]["profile"] = serde_json::json!("Dolby TrueHD + Dolby Atmos");
        for stream in held["streams"].as_array_mut().expect("streams") {
            stream["disposition"]["multilayer"] = serde_json::json!(0);
            stream["disposition"]["non_diegetic"] = serde_json::json!(0);
        }
        let scanned_json = scanned.to_string();
        let held_json = held.to_string();
        assert!(probes_describe_same_input(&scanned_json, &held_json).expect("new probe schema"));
        assert!(probes_describe_same_input(&held_json, &scanned_json).expect("old probe schema"));
        for (pointer, value) in [
            ("/format/nb_stream_groups", serde_json::json!(1)),
            ("/streams/0/view_ids_available", serde_json::json!("1")),
            ("/streams/0/disposition/multilayer", serde_json::json!(1)),
            ("/streams/0/disposition/non_diegetic", serde_json::json!(1)),
            (
                "/streams/0/side_data_list/0/dv_md_compression",
                serde_json::json!("limited"),
            ),
            (
                "/streams/0/side_data_list/0/dv_profile",
                serde_json::json!(5),
            ),
            ("/streams/1/initial_padding", serde_json::json!(1024)),
            (
                "/streams/1/profile",
                serde_json::json!("unrecognized profile"),
            ),
            ("/streams/1/channels", serde_json::json!(6)),
        ] {
            let mut changed = held.clone();
            *changed.pointer_mut(pointer).expect("measured field") = value;
            assert!(
                !probes_describe_same_input(&scanned_json, &changed.to_string())
                    .expect("changed media"),
                "{pointer}"
            );
        }
    }

    #[tokio::test]
    async fn producer_diagnostics_drain_but_retain_only_the_bounded_tail() {
        let input = [vec![b'x'; 24 * 1024], b"terminal filter error".to_vec()].concat();
        let tail = drain_diagnostics(input.as_slice()).await;
        assert_eq!(tail.len(), 8 * 1024);
        assert!(tail.ends_with("terminal filter error"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_extraction_drains_noisy_child_and_reaps_nonzero_exit() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args([
            "-c",
            "i=0; while [ $i -lt 4096 ]; do printf '0123456789abcdef0123456789abcdef' >&2; i=$((i + 1)); done; printf 'terminal extractor error' >&2; printf 'discarded stdout'; exit 7",
        ]);
        let mut owner = BoundedDiagnosticChild::spawn(
            &mut command,
            crate::process_control::ChildWork::background("test"),
        )
        .expect("noisy child");
        let (reaped_tx, reaped_rx) = tokio::sync::oneshot::channel();
        owner.reaped = Some(reaped_tx);
        let (status, tail) = tokio::time::timeout(Duration::from_secs(5), owner.output())
            .await
            .expect("fully drain without a stderr pipe deadlock")
            .expect("wait for noisy child");
        assert_eq!(status.code(), Some(7));
        assert_eq!(tail.len(), 8 * 1024);
        assert!(tail.ends_with("terminal extractor error"));
        reaped_rx.await.expect("nonzero exit still reaped");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_output_kills_and_reaps_at_the_physical_disk_cap() {
        let base = crate::test_tempdir().expect("bounded output");
        let output = base.path().join("sidecar");
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args([
            "-c",
            "while :; do printf '0123456789abcdef'; printf 'extracting' >&2; done",
        ]);
        let mut owner = BoundedDiagnosticChild::spawn_piped_output(
            &mut command,
            crate::process_control::ChildWork::background("test"),
        )
        .expect("piped child");
        let (reaped_tx, reaped_rx) = tokio::sync::oneshot::channel();
        owner.reaped = Some(reaped_tx);
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            owner.output_to_bounded_file(&output, 4_096),
        )
        .await
        .expect("cap must stop the child")
        .expect_err("oversized output");
        assert!(error.to_string().contains("disk bound"), "{error}");
        assert_eq!(
            std::fs::metadata(output).expect("bounded file").len(),
            4_096
        );
        reaped_rx.await.expect("oversized child reaped");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_output_reaps_the_child_when_the_destination_cannot_be_created() {
        let base = crate::test_tempdir().expect("bounded output");
        let output = base.path().join("existing-sidecar");
        tokio::fs::write(&output, b"owned by another extractor")
            .await
            .expect("pre-existing sidecar");
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args([
            "-c",
            "while :; do printf 'blocked stdout'; printf 'extracting' >&2; done",
        ]);
        let mut owner = BoundedDiagnosticChild::spawn_piped_output(
            &mut command,
            crate::process_control::ChildWork::background("test"),
        )
        .expect("piped child");
        let (reaped_tx, reaped_rx) = tokio::sync::oneshot::channel();
        owner.reaped = Some(reaped_tx);
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            owner.output_to_bounded_file(&output, 4_096),
        )
        .await
        .expect("destination error must stop the child")
        .expect_err("create_new refuses a pre-existing sidecar");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        reaped_rx.await.expect("failed output child reaped");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_bounded_extraction_transfers_exact_child_to_reaper() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "while :; do printf 'waiting extractor' >&2; done"]);
        let mut owner = BoundedDiagnosticChild::spawn(
            &mut command,
            crate::process_control::ChildWork::background("test"),
        )
        .expect("noisy pending child");
        let pid = owner
            .child
            .as_ref()
            .expect("owned child")
            .id()
            .expect("PID");
        let (reaped_tx, reaped_rx) = tokio::sync::oneshot::channel();
        owner.reaped = Some(reaped_tx);
        let output = tokio::spawn(owner.output());
        tokio::task::yield_now().await;
        output.abort();
        let _ = output.await;
        tokio::time::timeout(Duration::from_secs(5), reaped_rx)
            .await
            .expect("cancellation reaper settles")
            .expect("successful exact child wait");
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    /// A Compose file with an unset variable hands the process `PLURX_FFMPEG=`,
    /// and spawning a binary named "" fails with an ENOENT that names nothing.
    #[test]
    fn an_empty_override_is_not_a_binary_name() {
        assert_eq!(resolve_bin(None, "ffmpeg"), "ffmpeg");
        assert_eq!(resolve_bin(Some(String::new()), "ffprobe"), "ffprobe");
        assert_eq!(
            resolve_bin(Some("/opt/jellyfin-ffmpeg/ffmpeg".to_owned()), "ffmpeg"),
            "/opt/jellyfin-ffmpeg/ffmpeg"
        );
    }

    /// Older builds print help and listings to stderr, so a probe that read
    /// stdout alone would report every capability as absent on them.
    #[test]
    fn a_probe_reads_both_streams() {
        assert_eq!(merged_output(b"out\n", b"err\n"), "out\nerr\n");
        // Invalid UTF-8 is replaced rather than dropped: a lossy byte must not
        // take the rest of the listing with it.
        assert!(merged_output(&[0xff, b'\n'], b"dovi_rpu\n").ends_with("dovi_rpu\n"));
    }

    /// `-bsfs` lists exactly one filter per line. A substring search would
    /// claim `dovi_rpu` on any build whose listing merely mentions it — and
    /// then every Dolby Vision remux would fail at session start.
    #[test]
    fn a_bitstream_filter_is_matched_on_a_whole_line() {
        let listing = "Bitstream filters:\n  h264_mp4toannexb\n  dovi_rpu\n  hevc_metadata\n";
        assert!(declares_bsf(listing, "dovi_rpu"));
        assert!(!declares_bsf(listing, "av1_metadata"));
        // The failure the whole-line rule exists for.
        assert!(!declares_bsf(
            "  hevc_metadata (see also dovi_rpu)\n",
            "dovi_rpu"
        ));
    }

    /// A build that could not be asked must be classified exactly like one
    /// that answered "no". Reporting the capability on a failed probe would
    /// send every DV remux at a bitstream filter that is not there.
    #[test]
    fn an_unprobeable_ffmpeg_has_no_dovi_filter() {
        assert!(dovi_from_probe(Ok("  dovi_rpu\n".to_owned())));
        assert!(!dovi_from_probe(Ok("  hevc_metadata\n".to_owned())));
        assert!(!dovi_from_probe(Err(
            "No such file or directory (os error 2)".to_owned()
        )));
        // Truncated output — the spawn succeeded but the listing was cut off
        // mid-name. Half a filter name is not a filter.
        assert!(!dovi_from_probe(Ok(
            "Bitstream filters:\n  dovi_r".to_owned()
        )));
    }

    #[test]
    fn a_dolby_vision_renderer_option_is_matched_as_a_declaration() {
        assert!(declares_filter_option(
            "   apply_dovi <boolean> ..FV....... Apply Dolby Vision metadata if possible (default true)\n",
            "apply_dovi"
        ));
        assert!(!declares_filter_option(
            "   tonemap <int> ..FV....... used with apply_dovi\n",
            "apply_dovi"
        ));
        assert!(!declares_filter_option(
            "   apply_dovi_legacy <boolean> ..FV.......\n",
            "apply_dovi"
        ));
    }

    /// Same rule for pacing: an ffmpeg that could not be probed gets the
    /// pre-5.1 answer, because passing `-readrate` to a build without it is a
    /// hard exit rather than a warning.
    #[test]
    fn an_unprobeable_ffmpeg_gets_the_conservative_pacing_answer() {
        let modern =
            pacing_from_probe(Ok("  -readrate x\n  -readrate_initial_burst y\n".to_owned()));
        assert!(modern.readrate && modern.initial_burst);

        let failed = pacing_from_probe(Err("Permission denied (os error 13)".to_owned()));
        assert!(!failed.readrate, "a failed probe must not claim -readrate");
        assert!(!failed.initial_burst);
        // And the conservative answer must survive into the flags: nothing is
        // emitted for a transcode, `-re` for the copy path.
        assert!(failed.resolve(2.0, 90.0, false).args().is_empty());
    }

    /// The 5.1–6.0 middle build, classified through the probe rather than the
    /// parser: it keeps `-readrate` and loses only the burst clause. Worth
    /// pinning separately because this is the build where the publish gate
    /// fills at the paced rate — the flags must still carry the rate, since
    /// degrading the whole thing to `-re` would unpace every session on it.
    #[test]
    fn a_build_with_readrate_but_no_burst_keeps_its_rate() {
        let caps = pacing_from_probe(Ok(
            "  -readrate speed     read input at specified rate\n".to_owned()
        ));
        assert!(caps.readrate, "5.1+ declares -readrate");
        assert!(!caps.initial_burst, "the burst clause arrives in 6.1");
        assert_eq!(
            caps.resolve(2.0, 90.0, true).args(),
            vec!["-readrate", "2.00"],
            "the rate survives; only the burst is dropped"
        );
    }

    #[test]
    fn pacing_caps_come_from_the_help_text() {
        // ffmpeg 6.1+: both flags.
        let modern = "  -re                 read input at native frame rate\n  \
                      -readrate speed     read input at specified rate\n  \
                      -readrate_initial_burst seconds  initial burst\n";
        let caps = parse_pacing_caps(modern);
        assert!(caps.readrate);
        assert!(caps.initial_burst);

        // ffmpeg 5.1–6.0: rate limiting but no burst.
        let caps = parse_pacing_caps("  -readrate speed     read input at specified rate\n");
        assert!(caps.readrate);
        assert!(!caps.initial_burst);

        // Older: neither. Must not be fooled by the substring in -re's help.
        let caps = parse_pacing_caps(
            "  -re                 read input at native frame rate; equivalent to -readrate 1\n",
        );
        assert!(!caps.readrate);
        assert!(!caps.initial_burst);
    }

    #[test]
    fn burst_probe_classifies_honoured_inert_and_failed_measurements() {
        let declared = PacingCaps {
            readrate: true,
            initial_burst: true,
        };
        let honoured = classify_burst(declared, Ok(Duration::from_millis(599)));
        assert!(honoured.initial_burst);
        assert_eq!(
            honoured.resolve(2.0, 90.0, true).args(),
            vec!["-readrate_initial_burst", "90.0", "-readrate", "2.00"]
        );
        for probe in [
            Ok(Duration::from_millis(600)),
            Ok(Duration::from_secs(1)),
            Err("probe failed".to_owned()),
        ] {
            let corrected = classify_burst(declared, probe);
            assert!(!corrected.initial_burst);
            assert_eq!(
                corrected.resolve(2.0, 90.0, true).args(),
                vec!["-readrate", "2.00"]
            );
        }
    }

    #[test]
    fn burst_probe_applies_pacing_to_the_input() {
        let args = burst_probe_args();
        let input = args
            .iter()
            .position(|arg| *arg == "-i")
            .expect("the probe must declare its synthetic input");
        for option in ["-readrate_initial_burst", "-readrate"] {
            let option_index = args
                .iter()
                .position(|arg| *arg == option)
                .unwrap_or_else(|| panic!("the probe must pass {option}"));
            assert!(
                option_index < input,
                "{option} is an input option and must precede -i: {args:?}"
            );
        }
    }

    #[test]
    fn resolve_matches_the_build_and_the_caller() {
        let modern = PacingCaps {
            readrate: true,
            initial_burst: true,
        };
        assert_eq!(
            modern.resolve(2.0, 90.0, true).args(),
            vec!["-readrate_initial_burst", "90.0", "-readrate", "2.00"]
        );
        // Rate 0 means "unpaced" — emit nothing, whatever the build supports.
        assert!(modern.resolve(0.0, 90.0, true).args().is_empty());

        // 5.1–6.0: the rate lands, the burst clause is dropped.
        let rate_only = PacingCaps {
            readrate: true,
            initial_burst: false,
        };
        assert_eq!(
            rate_only.resolve(2.5, 90.0, true).args(),
            vec!["-readrate", "2.50"]
        );

        // Pre-5.1 splits by caller: copy degrades to realtime, transcode to
        // nothing (it was never paced, so `-re` would be a new cap).
        let ancient = PacingCaps::default();
        assert_eq!(ancient.resolve(2.0, 90.0, true).args(), vec!["-re"]);
        assert!(ancient.resolve(2.0, 90.0, false).args().is_empty());
    }

    #[test]
    fn replacing_an_attested_engine_object_withdraws_the_identity() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("ffmpeg");
        let replacement = directory.path().join("replacement");
        std::fs::write(&path, b"engine-a").expect("write original");
        std::fs::write(&replacement, b"engine-b").expect("write replacement");
        let expected = engine_path_version(&path).expect("object version");
        let objects = vec![(path.clone(), expected)];
        assert!(engine_objects_are_current(&objects));
        std::fs::remove_file(&path).expect("unlink original");
        std::fs::rename(replacement, &path).expect("install replacement");
        assert!(!engine_objects_are_current(&objects));
    }

    #[tokio::test]
    async fn process_fragment_engine_baseline_detects_an_actual_object_change() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("libavcodec");
        let replacement = directory.path().join("replacement");
        std::fs::write(&path, b"engine-baseline-a").expect("write baseline");
        std::fs::write(&replacement, b"engine-baseline-b").expect("write replacement");
        let expected = engine_path_version(&path).expect("object version");
        let baseline = FragmentIndexEngine {
            digest: "process-baseline".to_owned(),
            objects: vec![(path.clone(), expected)].into(),
            usable: true,
        };

        assert!(fragment_index_engine_snapshot_is_current(&baseline).await);
        std::fs::remove_file(&path).expect("unlink original");
        std::fs::rename(replacement, &path).expect("install replacement");
        assert!(!fragment_index_engine_snapshot_is_current(&baseline).await);
    }

    /// The pacing answer comes from `ffmpeg -h full`, and that listing has been
    /// over a megabyte on every build since 6.1 (measured: 1,005,926 bytes on
    /// 4.4.2, 1,165,847 on 6.1.1, more on 8.x). Read the size class that broke
    /// through the real subprocess path, not through a hand-made string: with
    /// the old 1 MiB bound this returns Err, `pacing_from_probe` maps Err to
    /// the conservative answer, and every remux stream runs unpaced while HLS
    /// falls back to realtime pacing — with nothing in the logs but "could not
    /// probe ffmpeg".
    #[tokio::test]
    async fn a_help_listing_larger_than_a_megabyte_survives_the_probe_bound() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg(
            "yes '  -pad <int>  ..FV....... filler line standing in for a real option' \
             | head -n 70000; \
             printf '  -readrate speed\\n  -readrate_initial_burst seconds\\n'",
        );
        let out = bounded_command_output(command)
            .await
            .expect("a help listing of the real size must not trip the bound");
        assert!(
            out.stdout.len() as u64 > 1024 * 1024,
            "the fixture has to be the size class that broke: {} bytes",
            out.stdout.len()
        );

        let caps = pacing_from_probe(Ok(merged_output(&out.stdout, &out.stderr)));
        assert!(
            caps.readrate,
            "the declaration is in the listing; only our own bound could hide it"
        );
        assert!(caps.initial_burst);
    }

    /// The bound still bites — it is a runaway guard, and raising it must not
    /// have quietly turned it off. One byte over is refused, and the refusal
    /// says what it refused against.
    #[tokio::test]
    async fn output_past_the_bound_is_still_refused_and_names_the_bound() {
        let over = tokio::io::repeat(b'x').take(ENGINE_PROBE_MAX_BYTES + 1);
        let error = read_bounded(over)
            .await
            .expect_err("one byte past the bound is refused");
        assert!(
            error.contains(&ENGINE_PROBE_MAX_BYTES.to_string()),
            "the error must name the bound it hit: {error}"
        );

        let exact = read_bounded(tokio::io::repeat(b'x').take(ENGINE_PROBE_MAX_BYTES))
            .await
            .expect("the bound itself is fine");
        assert_eq!(exact.len() as u64, ENGINE_PROBE_MAX_BYTES);
    }

    #[tokio::test]
    #[ignore = "nightly runner capability contract"]
    async fn nightly_runner_has_ffmpeg_readrate() {
        let caps = pacing_caps().await;
        eprintln!(
            "nightly ffmpeg capability: binary={} readrate={} initial_burst={}",
            ffmpeg_bin(),
            caps.readrate,
            caps.initial_burst,
        );
        assert!(
            caps.readrate,
            "nightly arbitration coverage requires ffmpeg 5.1+ (-readrate)"
        );
    }
}
