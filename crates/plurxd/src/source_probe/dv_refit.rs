//! The F+ helper run, `segment_decode_render --refit-stats SOURCE VIDEO_INDEX
//! --threads N [--resume FIRST FIRST_PTS]`, over the held source, as the
//! complete-title scan runs its scanner (`docs/streaming/FEL-RPU-REFIT-BUILD.md`
//! §3.2–3.3), and the node-local spool its statistics live in.
//!
//! The helper's stdout is written straight into the spool as it runs. A run
//! that is interrupted (a preemption, a lost lease, a crash) keeps the whole
//! scene lines it wrote, and the next attempt on this node resumes at the
//! start of the last of them (`dv_refit_title::resume_point`). A complete
//! run is kept compressed, so a fit-method change re-solves the title from
//! the spool without decoding it again. The spool is keyed by the source
//! identity and the helper's refit identity; it is listed and deleted from
//! the Developer card (`GET`/`DELETE /api/v1/admin/dv-refit/spool`).
use crate::vodencode::dv_runtime::ToolSet;
use plurx_core::{
    domain::MediaFile,
    transcode::{
        dv_refit::RefitScene,
        dv_refit_title::{self, HelperOutput, ResumePoint},
    },
};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

/// The helper decodes and composes every frame of both layers on the CPU.
pub(super) const WALL_TIME: Duration = Duration::from_secs(12 * 60 * 60);
/// Bound on one spool file, raw: a feature's scene lines with their
/// statistics are about 80 KB a scene.
pub(crate) const MAX_SPOOL_BYTES: u64 = 384 * 1024 * 1024;
/// What is kept of the helper's stderr: its last line is the reason.
const MAX_STDERR_BYTES: usize = 16 * 1024;
/// The helper's exit status for a refusal of the title's content
/// (`REFIT_EXIT_CONTENT` in `refit_mode.h`): not FEL, a full-resolution
/// EL, an unsupported active area, and the like. Every other refusal (I/O,
/// allocation, a decoder error, bad arguments) exits 1.
pub(super) const EXIT_CONTENT_REFUSAL: i32 = 3;
/// Frame threads per layer the helper decodes with (`--threads`). Measured
/// over the 311-frame Michael5400 window on nuc3 with other work running:
/// 1 thread 39 s at 1.0 core, 2 threads 21.9 s at 2.2 cores, 3 threads 16 s
/// at 3.2 cores, 4 threads 16.5 s at 3.1 cores (composition on the main
/// thread bounds it). Two buys most of the speed for the least CPU.
pub(super) const HELPER_THREADS: usize = 2;
/// The CPU threads a fit reserves with two helper threads: the measured
/// 2.2 cores, rounded up for the composing main thread's peaks.
pub(crate) const RESERVED_THREADS: usize = 3;
pub(super) const ACTIVITY: &str = "Dolby Vision F+ curve fit";
/// The spool directory under the node's runtime cache.
pub(crate) const SPOOL_DIR: &str = super::REFIT_SPOOL_DIR;

/// What the helper answered.
#[derive(Debug)]
pub(super) enum HelperRun {
    /// Per-scene statistics, parsed and checked, from a run or the spool.
    Stats(HelperOutput),
    /// A content refusal (exit 3): recorded, so the title is not decoded
    /// again until its identity changes.
    Refused(String),
}

/// Why a run produced no answer.
#[derive(Debug)]
pub(super) enum RunError {
    /// The run did not finish (spawn, signal, I/O or allocation failure,
    /// timeout, preemption, a changed source or helper): worth another
    /// attempt, which resumes from the spool.
    Transient(String),
    /// The helper finished but its output breaks the contract: running it
    /// again over the same bits would print the same thing.
    Malformed(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transient(reason) | Self::Malformed(reason) => f.write_str(reason),
        }
    }
}

/// How one finished child ended (decision D). Only the exit status decides;
/// stderr is read for the reason, never counted.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Exit {
    Success,
    /// Exit 3: the helper refused the title's content, for this reason.
    Refused(String),
    /// Anything else: retried.
    Failed(String),
}

/// The last non-empty stderr line, without the helper's prefix, bounded.
fn reason(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().rev().map(str::trim).find(|l| !l.is_empty())?;
    let mut line = line
        .strip_prefix("Segment refused: ")
        .unwrap_or(line)
        .to_owned();
    while line.len() > 1024 {
        line.pop();
    }
    Some(line)
}

pub(super) fn classify(
    output: &Result<plurx_core::process::bounded::Output, std::io::Error>,
) -> Exit {
    use std::os::unix::process::ExitStatusExt;
    let output = match output {
        Ok(output) => output,
        Err(error) => return Exit::Failed(format!("F+ helper: {error}")),
    };
    if output.status.success() {
        return Exit::Success;
    }
    let why = reason(&output.stderr);
    match (output.status.code(), output.status.signal()) {
        (Some(EXIT_CONTENT_REFUSAL), _) => Exit::Refused(
            why.unwrap_or_else(|| "the helper refused the title without a reason".into()),
        ),
        (_, Some(signal)) => Exit::Failed(format!("F+ helper ended by signal {signal}")),
        (code, None) => Exit::Failed(format!(
            "F+ helper failed (exit {}){}",
            code.unwrap_or(-1),
            why.map(|why| format!(": {why}")).unwrap_or_default()
        )),
    }
}

/// One source identity's spool: `<key>.jsonl.gz` once a run completed,
/// `<key>.partial` for the whole scene lines of interrupted runs, and
/// `<key>.run` while the helper writes.
#[derive(Debug, Clone)]
pub(crate) struct Spool {
    root: PathBuf,
    key: String,
}

/// One spool entry, as the Developer card lists it.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SpoolEntry {
    pub key: String,
    pub complete: bool,
    pub bytes: u64,
    pub modified_ms: i64,
}

impl Spool {
    pub(crate) fn root(runtime_cache: &Path) -> PathBuf {
        runtime_cache.join(SPOOL_DIR)
    }

    /// Keyed by what the statistics depend on (decision G): the source, the
    /// selected stream and the helper's refit identity. Not the fit version:
    /// a new fit method re-solves the same statistics.
    pub(crate) fn new(
        root: PathBuf,
        file_id: i64,
        source_size: i64,
        source_mtime: i64,
        video_index: i64,
        helper_identity: &str,
    ) -> Self {
        use sha2::{Digest, Sha256};
        let key = hex::encode(Sha256::digest(
            serde_json::to_vec(&(
                "dv-refit-spool-v1",
                file_id,
                source_size,
                source_mtime,
                video_index,
                helper_identity,
            ))
            .expect("spool key"),
        ));
        Self { root, key }
    }

    fn path(&self, suffix: &str) -> PathBuf {
        self.root.join(format!("{}{suffix}", self.key))
    }

    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(Spool) -> T + Send + 'static,
    ) -> Result<T, String> {
        let spool = self.clone();
        tokio::task::spawn_blocking(move || work(spool))
            .await
            .map_err(|e| format!("F+ spool: {e}"))
    }

    fn read_bounded(path: &Path) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_SPOOL_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_SPOOL_BYTES {
            return Err(std::io::Error::other("F+ spool exceeds its bound"));
        }
        Ok(bytes)
    }

    fn write_atomically(&self, suffix: &str, bytes: &[u8], gzip: bool) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let temp = self.path(&format!("{suffix}.tmp"));
        {
            let file = std::fs::File::create(&temp)?;
            if gzip {
                let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
                encoder.write_all(bytes)?;
                encoder.finish()?.sync_all()?;
            } else {
                let mut file = file;
                file.write_all(bytes)?;
                file.sync_all()?;
            }
        }
        std::fs::rename(&temp, self.path(suffix))
    }

    /// The complete run's stdout, when one is spooled and reads back whole.
    pub(super) async fn complete(&self) -> Option<Vec<u8>> {
        self.blocking(|spool| {
            let file = std::fs::File::open(spool.path(".jsonl.gz")).ok()?;
            let mut bytes = Vec::new();
            flate2::read::GzDecoder::new(file)
                .take(MAX_SPOOL_BYTES + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            (bytes.len() as u64 <= MAX_SPOOL_BYTES).then_some(bytes)
        })
        .await
        .ok()
        .flatten()
    }

    /// Keeps a complete run, compressed, in place of its partial lines.
    pub(super) async fn keep_complete(&self, text: String) -> Result<(), String> {
        self.blocking(move |spool| {
            spool.write_atomically(".jsonl.gz", text.as_bytes(), true)?;
            let _ = std::fs::remove_file(spool.path(".partial"));
            let _ = std::fs::remove_file(spool.path(".run"));
            Ok::<_, std::io::Error>(())
        })
        .await?
        .map_err(|e| format!("F+ spool: {e}"))
    }

    /// The whole scene lines interrupted runs kept, from the title's start.
    pub(super) async fn partial(&self) -> Vec<(String, RefitScene)> {
        self.blocking(|spool| {
            Self::read_bounded(&spool.path(".partial"))
                .map(|bytes| dv_refit_title::complete_scenes(&bytes))
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    }

    pub(super) async fn keep_partial(&self, lines: Vec<String>) -> Result<(), String> {
        self.blocking(move |spool| {
            let mut text = lines.join("\n");
            text.push('\n');
            if lines.is_empty() {
                let _ = std::fs::remove_file(spool.path(".partial"));
            } else {
                spool.write_atomically(".partial", text.as_bytes(), false)?;
            }
            let _ = std::fs::remove_file(spool.path(".run"));
            Ok::<_, std::io::Error>(())
        })
        .await?
        .map_err(|e| format!("F+ spool: {e}"))
    }

    /// A fresh file for the helper's stdout.
    pub(super) async fn open_run(&self) -> Result<std::fs::File, String> {
        self.blocking(|spool| {
            std::fs::create_dir_all(&spool.root)?;
            std::fs::File::create(spool.path(".run"))
        })
        .await?
        .map_err(|e| format!("F+ spool: {e}"))
    }

    pub(super) async fn run_output(&self) -> Vec<u8> {
        self.blocking(|spool| Self::read_bounded(&spool.path(".run")).unwrap_or_default())
            .await
            .unwrap_or_default()
    }

    pub(super) async fn discard(&self) {
        let _ = self
            .blocking(|spool| {
                for suffix in [".jsonl.gz", ".partial", ".run"] {
                    let _ = std::fs::remove_file(spool.path(suffix));
                }
            })
            .await;
    }
}

fn spool_name(name: &str) -> Option<(&str, bool)> {
    let (key, complete) = if let Some(key) = name.strip_suffix(".jsonl.gz") {
        (key, true)
    } else if let Some(key) = name
        .strip_suffix(".partial")
        .or_else(|| name.strip_suffix(".run"))
        .or_else(|| name.strip_suffix(".tmp").map(|n| n.trim_end_matches(".jsonl.gz").trim_end_matches(".partial")))
    {
        (key, false)
    } else {
        return None;
    };
    (key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())).then_some((key, complete))
}

/// The spool's entries on this node, one per file.
pub(crate) fn list(root: &Path) -> Vec<SpoolEntry> {
    let Ok(dir) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut entries: Vec<SpoolEntry> = dir
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let (key, complete) = spool_name(&name)?;
            let meta = entry.metadata().ok().filter(std::fs::Metadata::is_file)?;
            let modified_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as i64);
            Some(SpoolEntry {
                key: key.to_owned(),
                complete,
                bytes: meta.len(),
                modified_ms,
            })
        })
        .collect();
    entries.sort_by(|a, b| a.key.cmp(&b.key).then(b.complete.cmp(&a.complete)));
    entries
}

/// Deletes every spool file on this node; the files a running fit is
/// writing go too, and that fit restarts its title. Returns how many went.
pub(crate) fn clear(root: &Path) -> std::io::Result<usize> {
    let Ok(dir) = std::fs::read_dir(root) else {
        return Ok(0);
    };
    let mut removed = 0;
    for entry in dir.filter_map(Result::ok) {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if spool_name(&name).is_some() && entry.metadata().is_ok_and(|m| m.is_file()) {
            std::fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Runs the helper under the bounded child owner, labelled for Activity,
/// with only the bundle's declared environment and its stdout written into
/// `stdout`. `setup` installs held descriptors.
pub(super) async fn spawn(
    program: &Path,
    environment: &[(String, std::ffi::OsString)],
    source: &str,
    index: u8,
    resume: Option<ResumePoint>,
    stdout: std::fs::File,
    setup: impl FnOnce(&mut tokio::process::Command),
) -> Result<plurx_core::process::bounded::Output, std::io::Error> {
    let mut command = tokio::process::Command::new(program);
    command.args([
        "--refit-stats",
        source,
        &index.to_string(),
        "--threads",
        &HELPER_THREADS.to_string(),
    ]);
    if let Some(at) = resume {
        command.args([
            "--resume",
            &at.first_frame.to_string(),
            &at.first_pts.to_string(),
        ]);
    }
    setup(&mut command);
    plurx_core::process::bounded::output_command_bundle_env_to_file(
        &mut command,
        WALL_TIME,
        MAX_STDERR_BYTES,
        plurx_core::process::ChildWork::background(ACTIVITY),
        environment,
        stdout,
    )
    .await
}

/// Solving and parsing are CPU work over every scene: off the runtime.
async fn off_runtime<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| format!("F+ parse: {e}"))?
}

/// The run's answer once the child has been reaped: the joined title on
/// success (kept complete in the spool), the refusal (spool dropped), or a
/// failure whose whole scene lines are kept for the next attempt to resume.
pub(super) async fn settle_child(
    spool: &Spool,
    kept: Vec<(String, RefitScene)>,
    resume: Option<(usize, ResumePoint)>,
    exit: Exit,
) -> Result<HelperRun, RunError> {
    let keep = resume.map_or(0, |(keep, _)| keep);
    let mut prefix_lines: Vec<String> = Vec::with_capacity(keep);
    let mut prefix: Vec<RefitScene> = Vec::with_capacity(keep);
    for (line, scene) in kept.into_iter().take(keep) {
        prefix_lines.push(line);
        prefix.push(scene);
    }
    let bytes = spool.run_output().await;
    match exit {
        Exit::Success => {
            let at = resume.map(|(_, at)| at);
            let parsed = {
                let prefix = prefix.clone();
                let bytes = bytes.clone();
                off_runtime(move || {
                    let output = match at {
                        Some(at) => dv_refit_title::parse_resumed(prefix.clone(), at, &bytes)?,
                        None => dv_refit_title::parse(&bytes)?,
                    };
                    let mut lines = prefix_lines;
                    lines.extend(
                        dv_refit_title::continue_scenes(prefix.last(), &bytes)
                            .into_iter()
                            .map(|(line, _)| line),
                    );
                    if lines.len() != output.scenes.len() {
                        return Err("the helper's scene lines disagree with its run".into());
                    }
                    let text = dv_refit_title::spool_text(&lines, &output.summary)?;
                    Ok((output, text))
                })
                .await
            };
            match parsed {
                Ok((output, text)) => {
                    if let Err(error) = spool.keep_complete(text).await {
                        tracing::warn!(target: "plurxd::dv", %error, "F+ spool not kept");
                    }
                    Ok(HelperRun::Stats(output))
                }
                Err(error) => {
                    spool.discard().await;
                    Err(RunError::Malformed(error))
                }
            }
        }
        Exit::Refused(reason) => {
            spool.discard().await;
            Ok(HelperRun::Refused(reason))
        }
        Exit::Failed(reason) => {
            let lines = off_runtime(move || {
                let mut lines = prefix_lines;
                lines.extend(
                    dv_refit_title::continue_scenes(prefix.last(), &bytes)
                        .into_iter()
                        .map(|(line, _)| line),
                );
                Ok(lines)
            })
            .await
            .map_err(RunError::Transient)?;
            if let Err(error) = spool.keep_partial(lines).await {
                tracing::warn!(target: "plurxd::dv", %error, "F+ progress not kept");
            }
            Err(RunError::Transient(reason))
        }
    }
}

/// The whole run over the held source, as the title scan reads it: a
/// complete spool re-solves without decoding; otherwise the helper resumes
/// from the spool's whole scene lines (or starts), reading the source
/// fence, the budgeted background input when one is configured (the held
/// descriptor otherwise), and the helper and source are checked unchanged
/// after the child is reaped.
pub(super) async fn run(
    file: &MediaFile,
    index: u8,
    tools: &ToolSet,
    spool: &Spool,
) -> Result<HelperRun, RunError> {
    use std::os::fd::AsRawFd;
    let transient = RunError::Transient;
    if let Some(bytes) = spool.complete().await {
        match off_runtime(move || dv_refit_title::parse(&bytes)).await {
            Ok(output) => return Ok(HelperRun::Stats(output)),
            Err(error) => {
                tracing::warn!(target: "plurxd::dv", %error, "F+ spool unreadable; decoding again");
                spool.discard().await;
            }
        }
    }
    let source = crate::fragment_index_cluster::open_source_playback_fence(file, None)
        .await
        .map_err(transient)?;
    if !tools.is_current().await {
        return Err(transient("DV helper identity changed".into()));
    }
    let kept = spool.partial().await;
    let scenes: Vec<RefitScene> = kept.iter().map(|(_, s)| s.clone()).collect();
    let resume = dv_refit_title::resume_point(&scenes);
    let stdout = spool.open_run().await.map_err(transient)?;
    let input = match crate::background_jobs::source_budget() {
        Some(budget) => Some(
            crate::source_input::SourceInput::open_background_held(
                crate::fragment_index_cluster::SourceFence::clone_held(
                    &source.handle,
                    source.object_version(),
                )
                .map_err(transient)?,
                crate::http::stream::file_content_type(&file.path),
                budget,
            )
            .await
            .map_err(transient)?,
        ),
        None => None,
    };
    let descriptor = "/proc/self/fd/3";
    let held = source.handle.as_raw_fd();
    let direct = input.is_none();
    let output = spawn(
        &tools.renderer,
        &tools.renderer_env,
        input.as_ref().map_or(descriptor, |input| input.url()),
        index,
        resume.map(|(_, at)| at),
        stdout,
        |command| {
            if direct {
                crate::ffmpeg::inherit_raw_file_descriptors_with_cwd(command, &[(held, 3)], None);
            }
        },
    );
    let output = if input.is_some() {
        plurx_core::process::source_lifetime::mediated(output).await
    } else {
        output.await
    };
    if let Some(input) = input {
        if let Err(error) = input.close_and_wait().await {
            // The run's lines are still whole; keep them for the next attempt.
            let _ = settle_child(spool, kept, resume, Exit::Failed(String::new())).await;
            return Err(transient(format!("joining F+ source input: {error}")));
        }
    }
    if !source.unchanged() || source.reopen(file).await.is_err() {
        // What the helper read may straddle two sources: keep nothing.
        spool.discard().await;
        return Err(transient("source changed during the F+ fit".into()));
    }
    let exit = classify(&output);
    let tools_current = tools.is_current().await;
    let exit = match exit {
        // Another helper binary with the same refit identity prints the
        // same lines (decision G), so the run's progress is kept, but its
        // verdict is not taken from a binary that changed under it.
        Exit::Success | Exit::Refused(_) if !tools_current => {
            Exit::Failed("DV helper identity changed".into())
        }
        exit => exit,
    };
    settle_child(spool, kept, resume, exit).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn output(status: i32, stderr: &str) -> Result<plurx_core::process::bounded::Output, std::io::Error> {
        Ok(plurx_core::process::bounded::Output {
            status: std::process::ExitStatus::from_raw(status),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    /// Decision D: the exit status alone classifies a run. Exit 3 is a
    /// content refusal, stored as refused; exit 1, a signal or a spawn
    /// error is retried; the number of stderr lines decides nothing.
    #[test]
    fn the_exit_status_classifies_a_run() {
        assert_eq!(classify(&output(0, "noise\nmore noise\n")), Exit::Success);
        assert_eq!(
            classify(&output(3 << 8, "Segment refused: full-resolution enhancement layer\n")),
            Exit::Refused("full-resolution enhancement layer".into())
        );
        assert_eq!(
            classify(&output(3 << 8, "a decoder warning\nSegment refused: not FEL\n\n")),
            Exit::Refused("not FEL".into()),
            "the last line, however many there are"
        );
        assert!(matches!(
            classify(&output(1 << 8, "Segment refused: cannot allocate\n")),
            Exit::Failed(reason) if reason.contains("cannot allocate")
        ));
        assert!(matches!(classify(&output(1 << 8, "")), Exit::Failed(_)));
        assert!(matches!(classify(&output(9, "")), Exit::Failed(reason) if reason.contains("signal 9")));
        assert!(matches!(
            classify(&Err(std::io::Error::other("spawn"))),
            Exit::Failed(_)
        ));
        assert!(matches!(
            classify(&output(3 << 8, &"x".repeat(5000))),
            Exit::Refused(reason) if reason.len() == 1024
        ));
    }

    #[test]
    fn the_spool_lists_and_clears_only_its_own_files() {
        let dir = tempfile::tempdir().expect("scratch");
        let root = dir.path().join(SPOOL_DIR);
        std::fs::create_dir_all(&root).expect("root");
        let key = "a".repeat(64);
        std::fs::write(root.join(format!("{key}.jsonl.gz")), b"x").expect("complete");
        std::fs::write(root.join(format!("{key}.partial")), b"xy").expect("partial");
        std::fs::write(root.join("unrelated.txt"), b"keep").expect("other");
        let entries = list(&root);
        assert_eq!(
            entries.iter().map(|e| (e.complete, e.bytes)).collect::<Vec<_>>(),
            [(true, 1), (false, 2)]
        );
        assert_eq!(clear(&root).expect("clear"), 2);
        assert!(list(&root).is_empty());
        assert!(root.join("unrelated.txt").exists());
        assert_eq!(clear(&dir.path().join("missing")).expect("absent"), 0);
    }
}
