//! The ffmpeg binaries and what this particular build can do.
//!
//! Every delivery path shells out to the same two binaries, and two of them
//! (the progressive remux and the HLS sessions) need the same answer to the
//! same question: does this build understand the input-pacing flags? Probing
//! it in one place, once per process, is what keeps the answer consistent —
//! and keeps a stream from failing to start because one path guessed.

use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use plurx_core::domain::MediaFile;
use plurx_core::transcode::{
    output_size, EffectiveRateControl, Encoder, OutputGrade, Pacing, Pipeline,
};

/// An override wins only when it names something. An empty `PLURX_FFMPEG=` is
/// what a Compose file produces for an unset variable, and treating that as a
/// binary called "" would fail every spawn with a confusing ENOENT.
fn resolve_bin(override_value: Option<String>, fallback: &str) -> String {
    override_value
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

/// ffmpeg binary, overridable via `PLURX_FFMPEG` (jellyfin-ffmpeg / pinned path).
pub fn ffmpeg_bin() -> String {
    resolve_bin(std::env::var("PLURX_FFMPEG").ok(), "ffmpeg")
}

/// ffprobe binary, overridable via `PLURX_FFPROBE` (jellyfin-ffmpeg / pinned).
pub fn ffprobe_bin() -> String {
    resolve_bin(std::env::var("PLURX_FFPROBE").ok(), "ffprobe")
}

/// Pass held source/sidecar capabilities into reserved child FDs. Duplicate
/// every original before assigning any target: an original may itself be
/// fd 3 or fd 5. The post-fork closure performs only descriptor syscalls.
pub(crate) fn inherit_file_descriptors(
    command: &mut tokio::process::Command,
    files: &[(&std::fs::File, i32)],
) {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        assert!(files.len() <= 7 && files.iter().all(|(_, target)| (3..=9).contains(target)));
        let descriptors = files
            .iter()
            .map(|(file, target)| (file.as_raw_fd(), *target))
            .collect::<Vec<_>>();
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
                        return Err(std::io::Error::last_os_error());
                    }
                    libc::close(duplicate);
                }
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = (command, files);
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
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    #[cfg(test)]
    reaped: Option<tokio::sync::oneshot::Sender<()>>,
}

impl BoundedDiagnosticChild {
    #[cfg(test)]
    pub fn spawn(command: &mut tokio::process::Command) -> std::io::Result<Self> {
        let mut child = command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stderr = child.stderr.take();
        Ok(Self {
            child: Some(child),
            stdout: None,
            stderr,
            #[cfg(test)]
            reaped: None,
        })
    }

    /// Spawn a child whose media output is owned by the daemon. The caller
    /// must consume it with [`Self::output_to_bounded_file`]; no subprocess
    /// ever receives a cache pathname it can grow past the enforced bound.
    pub fn spawn_piped_output(command: &mut tokio::process::Command) -> std::io::Result<Self> {
        let mut child = command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        Ok(Self {
            child: Some(child),
            stdout,
            stderr,
            #[cfg(test)]
            reaped: None,
        })
    }

    #[cfg(test)]
    pub async fn output(mut self) -> std::io::Result<(std::process::ExitStatus, String)> {
        let stderr = self
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("extractor stderr was not piped"))?;
        let child = self.child.as_mut().expect("owned extraction child");
        let (status, diagnostics) = tokio::join!(child.wait(), drain_diagnostics(stderr));
        let status = status?;
        self.child.take(); // Successful wait, including nonzero exit, proves reap.
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
        let _ = child.start_kill();
        #[cfg(test)]
        let reaped = self.reaped.take();
        tokio::spawn(async move {
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

    pub fn is_current(&self) -> bool {
        std::fs::metadata(&self.path)
            .ok()
            .and_then(|metadata| engine_object_version(&metadata).ok())
            .is_some_and(|version| version == self.object_version)
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
pub(crate) async fn held_source_probe_json(source: &std::fs::File) -> Result<String, String> {
    #[cfg(not(unix))]
    {
        let _ = source;
        return Err("descriptor-bound source probing is unavailable on this platform".to_owned());
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
        let output = bounded_command_output(command).await?;
        String::from_utf8(output.stdout)
            .map_err(|error| format!("ffprobe returned non-UTF-8 JSON: {error}"))
    }
}

fn normalized_probe_document(raw: &str) -> Result<Vec<u8>, String> {
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
    serde_json::to_vec(&value).map_err(|error| error.to_string())
}

pub(crate) fn probes_describe_same_input(stored: &str, held: &str) -> Result<bool, String> {
    Ok(normalized_probe_document(stored)? == normalized_probe_document(held)?)
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
static FONT_RENDER_ENGINE: tokio::sync::OnceCell<FragmentIndexEngine> =
    tokio::sync::OnceCell::const_new();
static ENCODED_PROCESS_IDENTITY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

const ENGINE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
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

#[derive(Clone)]
struct FragmentIndexEngine {
    digest: String,
    objects: Vec<(std::path::PathBuf, String)>,
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
    objects: Vec<(std::path::PathBuf, String)>,
}

impl EncodedEngine {
    pub async fn capture(text_burn: bool) -> Result<Self, String> {
        let media = FRAGMENT_INDEX_ENGINE
            .get_or_init(fragment_index_engine_inner)
            .await;
        if !media.usable || !engine_objects_are_current(&media.objects) {
            return Err("the encoder dependency closure could not be attested".to_owned());
        }
        let process = ENCODED_PROCESS_IDENTITY.get_or_init(|| uuid::Uuid::new_v4().to_string());
        let mut digest = Sha256::new();
        digest.update(b"plurx/encoded-vod/engine-v1\0");
        digest.update(media.digest.as_bytes());
        digest.update(process.as_bytes());
        let mut objects = media.objects.clone();
        if text_burn {
            let fonts = FONT_RENDER_ENGINE
                .get_or_init(font_render_engine_inner)
                .await;
            if !fonts.usable || !engine_objects_are_current(&fonts.objects) {
                return Err(
                    "Fontconfig rules and resolved font files could not be attested".to_owned(),
                );
            }
            digest.update(fonts.digest.as_bytes());
            objects.extend(fonts.objects.clone());
        }
        objects.sort_by(|left, right| left.0.cmp(&right.0));
        objects.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
        Ok(Self {
            digest: hex::encode(digest.finalize()),
            objects,
        })
    }

    pub fn is_current(&self) -> bool {
        engine_objects_are_current(&self.objects)
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
            objects,
        })
    }
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
    engine.usable && engine_objects_are_current(&engine.objects)
}

fn engine_objects_are_current(objects: &[(std::path::PathBuf, String)]) -> bool {
    objects.iter().all(|(path, expected)| {
        std::fs::metadata(path)
            .ok()
            .and_then(|metadata| engine_object_version(&metadata).ok())
            .is_some_and(|current| &current == expected)
    })
}

async fn fragment_index_engine_inner() -> FragmentIndexEngine {
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
        objects,
        usable,
    }
}

async fn font_render_engine_inner() -> FragmentIndexEngine {
    let mut digest = Sha256::new();
    digest.update(b"plurx/font-render/engine-v1\0");
    let mut usable = true;
    let mut paths = Vec::new();

    let mut font_list = tokio::process::Command::new("fc-list");
    font_list.arg("--format=%{file}\n");
    match bounded_command_output(font_list).await {
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
    match bounded_command_output(configuration).await {
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

    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        usable = false;
        digest.update(b"no font inputs discovered");
    }
    let mut objects = Vec::new();
    let mut object_digests = Vec::new();
    for path in paths {
        match std::fs::metadata(&path)
            .map_err(|error| format!("stat {}: {error}", path.display()))
            .and_then(|metadata| engine_object_version(&metadata))
        {
            Ok(version) => {
                let mut identity = Sha256::new();
                identity.update(path.as_os_str().as_encoded_bytes());
                identity.update(version.as_bytes());
                object_digests.push(identity.finalize().to_vec());
                objects.push((path, version));
            }
            Err(error) => {
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
        objects,
        usable,
    }
}

struct BoundedOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn bounded_command_output(
    mut command: tokio::process::Command,
) -> Result<BoundedOutput, String> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "engine probe has no stdout".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "engine probe has no stderr".to_owned())?;
    let collect = async move {
        let (stdout, stderr, status) =
            tokio::join!(read_bounded(stdout), read_bounded(stderr), child.wait());
        let status = status.map_err(|error| error.to_string())?;
        if !status.success() {
            return Err(format!("engine probe exited {status}"));
        }
        Ok(BoundedOutput {
            stdout: stdout?,
            stderr: stderr?,
        })
    };
    tokio::time::timeout(ENGINE_PROBE_TIMEOUT, collect)
        .await
        .map_err(|_| "engine probe timed out".to_owned())?
}

async fn read_bounded(input: impl AsyncRead + Unpin) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    input
        .take(ENGINE_PROBE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > ENGINE_PROBE_MAX_BYTES {
        // Name the bound. The last time this fired it read as "could not probe
        // ffmpeg", which sent three people looking for a missing binary.
        return Err(format!(
            "engine probe exceeded its output bound of {ENGINE_PROBE_MAX_BYTES} bytes"
        ));
    }
    Ok(bytes)
}

async fn hash_engine_object(path: &std::path::Path) -> Result<(Vec<u8>, String), String> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|error| format!("stat {}: {error}", path.display()))?;
    if metadata.len() > ENGINE_OBJECT_MAX_BYTES {
        return Err(format!(
            "engine object {} exceeds size bound",
            path.display()
        ));
    }
    let version = engine_object_version(&metadata)?;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| format!("open {}: {error}", path.display()))?;
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
    let after = file
        .metadata()
        .await
        .map_err(|error| format!("re-stat {}: {error}", path.display()))?;
    if engine_object_version(&after)? != version {
        return Err(format!(
            "engine object {} changed while hashing",
            path.display()
        ));
    }
    Ok((object.finalize().to_vec(), version))
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

#[cfg(not(unix))]
fn engine_object_version(metadata: &std::fs::Metadata) -> Result<String, String> {
    let modified = metadata
        .modified()
        .map_err(|error| error.to_string())?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?;
    Ok(format!("{}:{}", metadata.len(), modified.as_nanos()))
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
    tokio::time::timeout(Duration::from_secs(20), command.status())
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
            let output = tokio::time::timeout(Duration::from_secs(20), command.output()).await;
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
            let output = tokio::time::timeout(Duration::from_secs(20), command.output()).await;
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
            let passed = tokio::time::timeout(Duration::from_secs(20), command.status())
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
            let passed = tokio::time::timeout(Duration::from_secs(20), command.status())
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

async fn dovi_probe_output(file: &MediaFile, apply: bool) -> Result<Vec<String>, String> {
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
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
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
pub async fn dovi_reshape_changes_pixels(file: &MediaFile) -> bool {
    let enabled = dovi_probe_output(file, true).await;
    let disabled = dovi_probe_output(file, false).await;
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
    match tokio::process::Command::new(ffmpeg_bin())
        .args(args)
        .output()
        .await
    {
        Ok(out) if out.status.success() => Ok(start.elapsed()),
        Ok(out) => Err(format!("exited with {}", out.status)),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
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
        assert!(engine.is_current());
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
        assert!(!engine.is_current());
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
        assert!(first.is_current());

        let replacement = base.path().join("replacement");
        tokio::fs::write(&replacement, b"codec-b")
            .await
            .expect("replacement dependency");
        std::fs::rename(replacement, dependency).expect("replace dependency");
        assert!(!first.is_current());
    }

    #[tokio::test]
    async fn text_renderer_attests_active_font_rules_and_files() {
        plurx_core::testfixtures::require_ffmpeg();
        let engine = EncodedEngine::capture(true)
            .await
            .expect("text renderer attestation");
        assert!(engine.is_current());
    }

    #[test]
    fn held_probe_comparison_ignores_only_the_descriptor_spelling() {
        let scanned = r#"{"streams":[{"codec_type":"video","width":1920}],"format":{"filename":"/media/a.mkv","duration":"60.0"}}"#;
        let held = r#"{"streams":[{"codec_type":"video","width":1920}],"format":{"filename":"/dev/fd/3","duration":"60.0"}}"#;
        assert!(probes_describe_same_input(scanned, held).expect("compare probes"));
        let replacement = held.replace("1920", "1280");
        assert!(!probes_describe_same_input(scanned, &replacement).expect("detect stale probe"));
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
        let mut owner = BoundedDiagnosticChild::spawn(&mut command).expect("noisy child");
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
        let mut owner =
            BoundedDiagnosticChild::spawn_piped_output(&mut command).expect("piped child");
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
        let mut owner =
            BoundedDiagnosticChild::spawn_piped_output(&mut command).expect("piped child");
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
        let mut owner = BoundedDiagnosticChild::spawn(&mut command).expect("noisy pending child");
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
        let expected = engine_object_version(&std::fs::metadata(&path).expect("metadata"))
            .expect("object version");
        let objects = vec![(path.clone(), expected)];
        assert!(engine_objects_are_current(&objects));
        std::fs::remove_file(&path).expect("unlink original");
        std::fs::rename(replacement, &path).expect("install replacement");
        assert!(!engine_objects_are_current(&objects));
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
