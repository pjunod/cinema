//! The Dolby Vision levels pass: reads each DV title's selected video stream
//! once and records which display-metadata levels it carries
//! (`plurx_dv_levels_v1`), for the Playback info `In the file` row.
//!
//! A reporter. It never touches the title scan, its scanner digest, its demand
//! or any job row, and nothing it records reaches a decision. It shares the
//! title lane (one child at a time, yielding to ordinary probes) and runs only
//! on the node holding the cluster-jobs singleton, the node that already runs
//! title scans and probes, so each title is read by one node. A duplicate read
//! after a leadership change is harmless: the graft is idempotent and fenced
//! to the source revision it read.

use std::{future::Future, pin::Pin, time::Duration};

use plurx_core::domain::MediaFile;
use plurx_core::error::StoreError;
use plurx_core::scan::dv_levels::{AnnexBSplitter, DvLevelsAccumulator, DvLevelsRecord};
use tokio_util::sync::CancellationToken;

use crate::source_input::SourceInput;
use crate::state::AppState;

use super::dv_title::{selected_video_index, source_read, SourceRead, TitleScanError};

/// The pass streams the whole selected video stream, so it gets the title
/// scanner's wall time.
const WALL_TIME: Duration = Duration::from_secs(6 * 60 * 60);
/// No RPU is larger; anything longer is a slice and is dropped as it streams.
const MAX_HELD_NAL: usize = 64 * 1024;
const READ_CHUNK: usize = 1 << 20;

pub(super) struct LevelsTask {
    pub(super) work: Pin<Box<dyn Future<Output = bool> + Send>>,
    cancel: CancellationToken,
    probe_pipeline: String,
    probe_after: Option<plurx_core::store::background_jobs::CandidateCursor>,
}

impl LevelsTask {
    /// Starts at most one read, after the title and F+ lanes found nothing to
    /// run. `after` walks the DV sources lacking a record across ticks.
    pub(super) async fn start(
        state: &AppState,
        after: &mut i64,
    ) -> Result<Option<Self>, StoreError> {
        if !state
            .jobs
            .execution_authority()
            .may_run_cluster_jobs()
            .await
        {
            return Ok(None);
        }
        let Some(file) = state
            .store
            .files_missing_dv_levels(*after, 1)
            .await?
            .into_iter()
            .next()
        else {
            *after = 0;
            return Ok(None);
        };
        let Some(probe_pipeline) = plurx_core::scan::probe::pipeline_digest().await else {
            return Ok(None);
        };
        *after = file.id;
        let cancel = CancellationToken::new();
        let work = Box::pin(run(state.clone(), file, cancel.clone()));
        Ok(Some(Self {
            work,
            cancel,
            probe_pipeline,
            probe_after: None,
        }))
    }

    /// The lowest-priority lane: any locally runnable ordinary probe takes it.
    pub(super) async fn should_yield(&mut self, state: &AppState) -> Result<bool, StoreError> {
        super::title_work::probe_preempts(state, None, &self.probe_pipeline, &mut self.probe_after)
            .await
    }

    pub(super) async fn stop(mut self) -> bool {
        self.cancel.cancel();
        // Reap the child before releasing the lane, as the other lanes do.
        self.work.as_mut().await
    }
}

async fn run(state: AppState, file: MediaFile, cancel: CancellationToken) -> bool {
    let read =
        plurx_core::process::bounded::cancellable(cancel.clone(), read_levels(&state, &file)).await;
    if cancel.is_cancelled() {
        // A yielded or stopped read says nothing about the title.
        return true;
    }
    let record = match read {
        Ok(record) => record,
        // Readable input the stream copy refused, or no display metadata: a
        // verdict on the content, recorded so the pass does not retry it.
        Err(TitleScanError::Failed(reason)) => DvLevelsRecord::unreadable(reason),
        // Contention, storage trouble or a changed source: a later pass reads
        // it again.
        Err(error) => {
            tracing::debug!(file_id = file.id, %error, "DV levels read deferred");
            return true;
        }
    };
    match state
        .store
        .merge_file_probe_dv_levels(file.id, file.size, file.mtime, &record)
        .await
    {
        Ok(true) => {}
        Ok(false) => tracing::debug!(file_id = file.id, "source changed before DV levels graft"),
        Err(error) => tracing::warn!(file_id = file.id, %error, "DV levels graft failed"),
    }
    true
}

#[cfg(unix)]
async fn read_levels(state: &AppState, file: &MediaFile) -> Result<DvLevelsRecord, TitleScanError> {
    use crate::producer_spawn::{ChildStdio, ChildStdios, Descriptors};

    let document = state
        .store
        .get_file_probe_json(file.id)
        .await
        .map_err(|error| TitleScanError::SourceInterrupted(error.to_string()))?
        .ok_or_else(|| TitleScanError::SourceInterrupted("probe document withdrawn".into()))?;
    let document: serde_json::Value =
        serde_json::from_str(&document).map_err(|error| error.to_string())?;
    let index = selected_video_index(&document).ok_or("unknown selected video stream")?;
    let source = crate::fragment_index_cluster::open_source_playback_fence(file, None)
        .await
        .map_err(|error| TitleScanError::SourceInterrupted(error.to_string()))?;
    let input = match crate::background_jobs::source_budget() {
        Some(budget) => Some(
            SourceInput::open_background_held(
                crate::fragment_index_cluster::SourceFence::clone_held(
                    &source.handle,
                    source.object_version(),
                )
                .map_err(|error| TitleScanError::SourceInterrupted(error.to_string()))?,
                crate::http::stream::file_content_type(&file.path),
                budget,
            )
            .await
            .map_err(|error| TitleScanError::SourceInterrupted(error.to_string()))?,
        ),
        None => None,
    };
    let descriptor = if cfg!(target_os = "linux") {
        "/proc/self/fd/3"
    } else {
        "/dev/fd/3"
    };
    let (reader, writer) =
        std::io::pipe().map_err(|error| TitleScanError::SourceInterrupted(error.to_string()))?;
    let writer: std::fs::File = std::os::fd::OwnedFd::from(writer).into();
    let mut command = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin());
    command.args(stream_copy_args(
        input.as_ref().map_or(descriptor, SourceInput::url),
        index,
    ));
    let manifest =
        Descriptors::from_files(input.is_none().then_some(&source.handle), None, None, false)
            .prepare(
                &mut command,
                ChildStdios {
                    stdin: ChildStdio::Null,
                    stdout: ChildStdio::File(&writer),
                    stderr: ChildStdio::Piped,
                },
            )
            .map_err(|error| TitleScanError::SourceInterrupted(error.to_string()))?;
    // The reader drains the pipe while the child runs; it sees EOF once the
    // child has exited and the parent's write end below is dropped.
    let folding = tokio::task::spawn_blocking(move || fold(reader));
    let output = plurx_core::process::bounded::output_command_bundle_env_to_file(
        &mut command,
        WALL_TIME,
        64 * 1024,
        // Stdout is a pipe, which no file-size limit applies to; ffmpeg
        // writes no regular file here, so any such write is refused early.
        64 * 1024,
        plurx_core::process::ChildWork::background("Dolby Vision metadata levels read"),
        &[],
        writer,
        Some(manifest),
    );
    let output = if input.is_some() {
        plurx_core::process::source_lifetime::mediated(output).await
    } else {
        output.await
    };
    // The command holds the pipe's write end until it is dropped.
    drop(command);
    let read = source_read(input.as_ref());
    if let Some(input) = input {
        input
            .close_and_wait()
            .await
            .map_err(|error| TitleScanError::SourceInterrupted(error.to_string()))?;
    }
    let accumulator = folding
        .await
        .map_err(|error| TitleScanError::SourceInterrupted(error.to_string()))?
        .map_err(TitleScanError::SourceInterrupted)?;
    verdict(output, read)?;
    if !source.unchanged() || source.reopen(file).await.is_err() {
        return Err(TitleScanError::SourceInterrupted(
            "source changed during the DV levels read".into(),
        ));
    }
    match accumulator.finish() {
        record @ DvLevelsRecord::Recorded { .. } => Ok(record),
        DvLevelsRecord::Unreadable { reason, .. } => Err(TitleScanError::Failed(reason)),
    }
}

/// The selected video stream as an Annex B elementary stream on stdout: a
/// stream copy, so no picture is decoded.
fn stream_copy_args(input: &str, index: u8) -> Vec<String> {
    [
        "-nostdin",
        "-v",
        "error",
        "-i",
        input,
        "-map",
        &format!("0:{index}"),
        "-c:v",
        "copy",
        "-bsf:v",
        "hevc_mp4toannexb",
        "-f",
        "hevc",
        "pipe:1",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Every RPU in the stream, folded as it arrives.
fn fold(mut reader: impl std::io::Read) -> Result<DvLevelsAccumulator, String> {
    let mut splitter = AnnexBSplitter::default();
    let mut accumulator = DvLevelsAccumulator::default();
    let mut buffer = vec![0; READ_CHUNK];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => splitter.push(&buffer[..read], MAX_HELD_NAL, &mut |nal| {
                accumulator.observe_nal(nal)
            }),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    splitter.finish(&mut |nal| accumulator.observe_nal(nal));
    Ok(accumulator)
}

/// A failed source read outranks the stream copy's own exit, as it does for
/// the title scan: levels folded from truncated input are never recorded.
#[cfg(unix)]
fn verdict(
    output: std::io::Result<plurx_core::process::bounded::Output>,
    read: SourceRead,
) -> Result<(), TitleScanError> {
    match (read, output) {
        (SourceRead::StorageFailed, _) => Err(TitleScanError::SourceReadFailed(
            "source storage read failed during the DV levels read".into(),
        )),
        (SourceRead::Interrupted, _) => Err(TitleScanError::SourceInterrupted(
            "source service interrupted the DV levels read".into(),
        )),
        (SourceRead::Clean, Ok(output)) if output.status.success() => Ok(()),
        (SourceRead::Clean, Ok(output)) => {
            let tail = super::dv_title::stderr_tail(&output.stderr);
            Err(TitleScanError::Failed(if tail.is_empty() {
                format!("the video stream copy failed ({})", output.status)
            } else {
                format!(
                    "the video stream copy failed ({}; stderr: {tail})",
                    output.status
                )
            }))
        }
        (SourceRead::Clean, Err(error)) => Err(TitleScanError::SourceInterrupted(format!(
            "the video stream copy did not run: {error}"
        ))),
    }
}

#[cfg(not(unix))]
async fn read_levels(_: &AppState, _: &MediaFile) -> Result<DvLevelsRecord, TitleScanError> {
    Err(TitleScanError::SourceInterrupted(
        "the DV levels read needs a Unix descriptor handoff".into(),
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The pass's own command line, run by the real FFmpeg over a Profile 8
    /// fixture, folds to the levels its RPUs carry.
    #[test]
    fn the_stream_copy_of_a_dolby_vision_file_records_its_levels() {
        let ffmpeg = crate::ffmpeg::ffmpeg_bin();
        if std::process::Command::new(&ffmpeg)
            .arg("-version")
            .output()
            .is_err()
        {
            eprintln!("skipping: ffmpeg not installed");
            return;
        }
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../plurx-core/tests/fixtures/dv-runtime/authored.mp4"
        );
        let mut child = std::process::Command::new(&ffmpeg)
            .args(stream_copy_args(fixture, 0))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("ffmpeg");
        let accumulator = fold(child.stdout.take().expect("stdout")).expect("fold");
        assert!(child.wait().expect("ffmpeg exits").success());
        let DvLevelsRecord::Recorded {
            levels,
            rpus,
            unparsed,
            ..
        } = accumulator.finish()
        else {
            panic!("a Dolby Vision fixture records levels");
        };
        assert!(
            rpus > 0 && unparsed == 0,
            "{rpus} RPUs, {unparsed} unparsed"
        );
        assert!(levels.contains(&1), "{levels:?}");
    }

    #[test]
    fn a_stream_without_rpus_is_unreadable_not_empty() {
        let accumulator = fold(&[0u8, 0, 0, 1, 0x40, 0x01, 0xaa][..]).expect("fold");
        assert!(matches!(
            accumulator.finish(),
            DvLevelsRecord::Unreadable { .. }
        ));
    }
}
