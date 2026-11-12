//! The F+ helper run: `segment_decode_render --refit-stats SOURCE VIDEO_INDEX`
//! over the held source, as the complete-title scan runs its scanner
//! (`docs/streaming/FEL-RPU-REFIT-BUILD.md` §3.2–3.3).
use crate::vodencode::dv_runtime::ToolSet;
use plurx_core::{domain::MediaFile, transcode::dv_refit_title::HelperOutput};
use std::time::Duration;

/// The helper decodes and composes every frame of both layers on the CPU.
pub(super) const WALL_TIME: Duration = Duration::from_secs(12 * 60 * 60);
/// A feature's scene lines with their statistics: about 80 KB a scene.
pub(super) const MAX_STDOUT_BYTES: usize = 384 * 1024 * 1024;
/// The helper refuses with one line; anything longer is not a refusal.
const MAX_REFUSAL_BYTES: usize = 1024;
pub(super) const ACTIVITY: &str = "Dolby Vision F+ curve fit";

/// What the helper answered.
#[derive(Debug)]
pub(super) enum HelperRun {
    /// Per-scene statistics, parsed and checked.
    Stats(HelperOutput),
    /// A deliberate refusal: a nonzero exit with one stderr line. It is
    /// recorded, so the title is not decoded again until its identity changes.
    Refused(String),
}

/// Why a run produced no answer.
#[derive(Debug)]
pub(super) enum RunError {
    /// The run did not finish (spawn, signal, timeout, a changed source or
    /// helper): worth another attempt.
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

/// Reads one finished run's exit and output.
pub(super) fn classify(
    output: Result<plurx_core::process::bounded::Output, std::io::Error>,
) -> Result<HelperRun, RunError> {
    use std::os::unix::process::ExitStatusExt;
    let output = output.map_err(|error| RunError::Transient(format!("F+ helper: {error}")))?;
    if output.status.success() {
        if output.stdout.len() >= MAX_STDOUT_BYTES {
            return Err(RunError::Malformed(format!(
                "F+ helper output exceeds {MAX_STDOUT_BYTES} bytes"
            )));
        }
        return plurx_core::transcode::dv_refit_title::parse(&output.stdout)
            .map(HelperRun::Stats)
            .map_err(RunError::Malformed);
    }
    if let Some(signal) = output.status.signal() {
        return Err(RunError::Transient(format!(
            "F+ helper ended by signal {signal}"
        )));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr.strip_suffix('\n').unwrap_or(&stderr);
    if line.trim().is_empty() || line.contains('\n') || line.len() > MAX_REFUSAL_BYTES {
        return Err(RunError::Transient(format!(
            "F+ helper failed ({}) without a one-line refusal",
            output.status
        )));
    }
    Ok(HelperRun::Refused(line.trim().to_owned()))
}

/// Runs `program --refit-stats SOURCE INDEX` under the bounded child owner,
/// labelled for Activity, with only the bundle's declared environment.
/// `setup` installs held descriptors.
pub(super) async fn spawn(
    program: &std::path::Path,
    environment: &[(String, std::ffi::OsString)],
    source: &str,
    index: u8,
    setup: impl FnOnce(&mut tokio::process::Command),
) -> Result<plurx_core::process::bounded::Output, std::io::Error> {
    let mut command = tokio::process::Command::new(program);
    command.args(["--refit-stats", source, &index.to_string()]);
    setup(&mut command);
    plurx_core::process::bounded::output_command_bundle_env(
        &mut command,
        WALL_TIME,
        MAX_STDOUT_BYTES,
        plurx_core::process::ChildWork::background(ACTIVITY),
        environment,
    )
    .await
}

/// The whole run over the held source, as the title scan reads it: the
/// source fence, the budgeted background input when one is configured (the
/// held descriptor otherwise), and the helper and source checked unchanged
/// after the child is reaped.
pub(super) async fn run(
    file: &MediaFile,
    document: &str,
    tools: &ToolSet,
) -> Result<HelperRun, RunError> {
    use std::os::fd::AsRawFd;
    let transient = RunError::Transient;
    let source = crate::fragment_index_cluster::open_source_playback_fence(file, None)
        .await
        .map_err(transient)?;
    let document: serde_json::Value =
        serde_json::from_str(document).map_err(|e| RunError::Malformed(e.to_string()))?;
    let index = super::dv_title::selected_video_index(&document)
        .ok_or_else(|| RunError::Malformed("unknown selected video stream".into()))?;
    if !tools.is_current().await {
        return Err(transient("DV helper identity changed".into()));
    }
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
        input
            .close_and_wait()
            .await
            .map_err(|error| transient(format!("joining F+ source input: {error}")))?;
    }
    if !tools.is_current().await {
        return Err(transient("DV helper identity changed".into()));
    }
    if !source.unchanged() || source.reopen(file).await.is_err() {
        return Err(transient("source changed during the F+ fit".into()));
    }
    classify(output)
}
