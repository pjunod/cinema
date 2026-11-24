//! Full-title facts acquired only by the persistent, admitted title job.
use crate::{dv_tools::ToolSet, source_input::SourceInput};
use plurx_core::{
    domain::MediaFile,
    scan::dv_title::{DvTitleStats, TitleEligibility},
};

/// Why a complete-title scan produced no aggregate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TitleScanError {
    /// The source service withdrew a read the scanner was waiting on (an
    /// exhausted pause, a revoked or unavailable budget grant). The storage
    /// was not at fault, so this is contention, like a scheduler preemption.
    SourceInterrupted(String),
    /// The storage itself failed a read (an I/O error, or the object ended
    /// early). The scanner saw truncated input, so its result is not a
    /// verdict on the content, but rereading may fail the same way.
    SourceReadFailed(String),
    /// The scanner read the whole selected input cleanly and refused its
    /// content for a reason in [`CONTENT_REFUSALS`], and the source and the
    /// scanner were still current afterwards. Rereading the same bytes with
    /// the same scanner reaches the same refusal, so this is a verdict for
    /// that probe revision and scanner digest, not a transient failure.
    Refused(String),
    /// The scanner, its aggregate or its prerequisites failed for a reason
    /// that is not a verdict on the content: a launch error, a signal or
    /// deadline, an allocation or I/O setup refusal, or a changed source.
    Failed(String),
}

impl std::fmt::Display for TitleScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceInterrupted(reason)
            | Self::SourceReadFailed(reason)
            | Self::Refused(reason)
            | Self::Failed(reason) => f.write_str(reason),
        }
    }
}

/// The `scan_title` refusals that judge the selected video's bytes: its
/// configuration, its bitstream, its demuxed packets, and whether its
/// coverage and endpoint can be proven complete. Each is reached only after
/// reads that succeeded, and depends on nothing but those bytes, so the same
/// file and scanner refuse the same way on every attempt.
///
/// Refusals left out are about the environment, not the content: the source
/// capability, I/O and demux setup, allocation, usage, the RPU timeline
/// sidecar, `source open`, `complete demux EOF` (a direct read's I/O error
/// surfaces there) and `aggregate output`. Two more classes are left out:
/// `corrupt demux packet` is set by libavformat on a short sample read, which
/// a scanner-side loopback failure causes without the bridge seeing one; and
/// `selected stream exists` and `selected HEVC video` judge the stream index
/// the daemon chose, which neither the probe revision nor the scanner digest
/// covers. They, and any reason a newer scanner adds, keep the charged retry.
pub(super) const CONTENT_REFUSALS: &[&str] = &[
    "a PPS lets slices withhold pictures from output",
    "bounded access-unit NAL count",
    "complete NAL length",
    "NAL inside its access unit",
    "first slice in a picture missing",
    "slice names an unknown PPS",
    "two first slices in one picture",
    "pictures discarded at a new sequence",
    "title begins at a random access point",
    "observed complete display-frame timing",
    "bounded title display-frame count",
    "length-prefixed HEVC with a version-1 configuration",
    "complete configuration array",
    "complete configuration NAL length",
    "configuration NAL inside its record",
    "bounded picture size",
    "complete declared frame coverage",
    "contiguous decoded title coverage",
    "observed declared video origin",
    "bounded declared video end",
    "complete selected video duration",
    "selected video duration tag",
    "complete selected video duration tag",
    "independent complete selected-video endpoint unavailable",
];

/// `scan_title`'s `need()` prints this prefix, then its reason, and exits 1.
const REFUSAL_PREFIX: &str = "DV title scan refused: ";

/// The content refusal a scanner that exited 1 reported, if its last refusal
/// line names one exactly.
fn content_refusal(output: &plurx_core::process::bounded::Output) -> Option<&'static str> {
    if output.status.code() != Some(1) {
        return None;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr.lines().rev().find_map(|line| {
        line.find(REFUSAL_PREFIX)
            .map(|at| line[at + REFUSAL_PREFIX.len()..].trim())
    })?;
    CONTENT_REFUSALS
        .iter()
        .copied()
        .find(|reason| *reason == line)
}

impl From<String> for TitleScanError {
    fn from(reason: String) -> Self {
        Self::Failed(reason)
    }
}

impl From<&str> for TitleScanError {
    fn from(reason: &str) -> Self {
        Self::Failed(reason.to_owned())
    }
}

/// What the source bridge observed while the scanner read through it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceRead {
    /// Every served read succeeded, or there was no bridge.
    Clean,
    /// A served read failed for a service reason: budget, grant or pause.
    Interrupted,
    /// A served read failed in the storage itself.
    StorageFailed,
}

#[cfg(unix)]
fn source_read(input: Option<&SourceInput>) -> SourceRead {
    match input {
        Some(input) if input.source_physical_read_failed() => SourceRead::StorageFailed,
        Some(input) if input.source_read_failed() => SourceRead::Interrupted,
        _ => SourceRead::Clean,
    }
}

/// Bounded stderr carried into a failure reason: the final bytes, where a
/// scanner prints its fatal error, on one line, with the bridge's private
/// capability URL redacted.
const STDERR_TAIL_BYTES: usize = 512;

fn stderr_tail(stderr: &[u8]) -> String {
    let start = stderr.len().saturating_sub(STDERR_TAIL_BYTES);
    let line = String::from_utf8_lossy(&stderr[start..])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    crate::scratch_put::redact(&line)
}

/// Classify the scanner's exit before its aggregate is trusted. A failed
/// source read outranks every scanner outcome, including an apparent
/// success: an aggregate computed over truncated input is never published.
/// Over clean reads, a refusal naming a [`CONTENT_REFUSALS`] reason is a
/// verdict; every other non-zero exit, signal or launch error is not.
pub(super) fn scanner_verdict(
    output: std::io::Result<plurx_core::process::bounded::Output>,
    read: SourceRead,
) -> Result<Vec<u8>, TitleScanError> {
    let describe = |output: &std::io::Result<plurx_core::process::bounded::Output>| match output {
        Ok(output) => {
            let tail = stderr_tail(&output.stderr);
            if tail.is_empty() {
                output.status.to_string()
            } else {
                format!("{}; stderr: {tail}", output.status)
            }
        }
        Err(error) => error.to_string(),
    };
    match read {
        SourceRead::StorageFailed => Err(TitleScanError::SourceReadFailed(format!(
            "source storage read failed during title scan ({})",
            describe(&output)
        ))),
        SourceRead::Interrupted => Err(TitleScanError::SourceInterrupted(format!(
            "source service interrupted the title scan ({})",
            describe(&output)
        ))),
        SourceRead::Clean => match output {
            Ok(output) if output.status.success() => Ok(output.stdout),
            Ok(output) if content_refusal(&output).is_some() => Err(TitleScanError::Refused(
                format!("complete-title DV scan refused ({})", describe(&Ok(output))),
            )),
            failed => Err(TitleScanError::Failed(format!(
                "complete-title DV scan failed ({})",
                describe(&failed)
            ))),
        },
    }
}

use std::{path::Path, time::Duration};

/// Launch from the held source or its budgeted bridge under one owned child.
pub(crate) async fn launch_scanner(
    scanner: &Path,
    index: &str,
    held: &std::fs::File,
    input: Option<&SourceInput>,
    runtime_cache: &Path,
    wall_time: Duration,
    work: plurx_core::process::ChildWork,
) -> std::io::Result<plurx_core::process::bounded::Output> {
    use crate::producer_spawn::{ChildStdio, ChildStdios, Descriptors};

    let mut command = tokio::process::Command::new(scanner);
    command.current_dir(runtime_cache);
    let descriptor = if cfg!(target_os = "linux") {
        "/proc/self/fd/3"
    } else {
        "/dev/fd/3"
    };
    command.args([input.map_or(descriptor, SourceInput::url), index]);
    let manifest = Descriptors::from_files(input.is_none().then_some(held), None, None, false)
        .prepare(
            &mut command,
            ChildStdios {
                stdin: ChildStdio::Null,
                stdout: ChildStdio::Piped,
                stderr: ChildStdio::Piped,
            },
        )?;
    let output = plurx_core::process::bounded::output_command_clean_env_with_descriptors(
        &mut command,
        wall_time,
        64 * 1024,
        work,
        manifest,
    );
    if input.is_some() {
        plurx_core::process::source_lifetime::mediated(output).await
    } else {
        output.await
    }
}

#[cfg(unix)]
pub(super) async fn scan(
    file: &MediaFile,
    document: &str,
    tools: &ToolSet,
    runtime_cache: &Path,
) -> Result<DvTitleStats, TitleScanError> {
    let source = crate::fragment_index_cluster::open_source_playback_fence(file, None).await?;
    let document: serde_json::Value = serde_json::from_str(document).map_err(|e| e.to_string())?;
    let index = selected_video_index(&document).ok_or("unknown selected video stream")?;
    if !tools.is_current().await {
        return Err("DV scanner identity changed".into());
    }
    let input = match crate::background_jobs::source_budget() {
        Some(budget) => Some(
            crate::source_input::SourceInput::open_background_held(
                crate::fragment_index_cluster::SourceFence::clone_held(
                    &source.handle,
                    source.object_version(),
                )?,
                crate::http::stream::file_content_type(&file.path),
                budget,
            )
            .await?,
        ),
        None => None,
    };
    let output = launch_scanner(
        &tools.scanner,
        &index.to_string(),
        &source.handle,
        input.as_ref(),
        runtime_cache,
        Duration::from_secs(6 * 60 * 60),
        plurx_core::process::ChildWork::background("complete-title Dolby Vision scan"),
    )
    .await;
    // Read the bridge's verdict before it is closed: the scanner has exited,
    // so every served read that could fail has already been observed.
    let read = source_read(input.as_ref());
    if let Some(input) = input {
        input
            .close_and_wait()
            .await
            .map_err(|error| format!("joining title source input: {error}"))?;
    }
    let verdict = scanner_verdict(output, read);
    // An aggregate and a content refusal are both judgements of these bytes
    // by this scanner: trust either only while both are still current.
    if matches!(verdict, Ok(_) | Err(TitleScanError::Refused(_))) {
        if !tools.is_current().await {
            return Err("DV scanner identity changed".into());
        }
        if !source.unchanged() || source.reopen(file).await.is_err() {
            return Err("source changed during title scan".into());
        }
    }
    let stdout = verdict?;
    let stats: DvTitleStats =
        serde_json::from_slice(&stdout).map_err(|e| format!("invalid DV aggregate: {e}"))?;
    if stats.eligibility() == TitleEligibility::InvalidAggregate || !stats.complete {
        return Err("incomplete or inconsistent DV aggregate".into());
    }
    Ok(stats)
}

/// Physical controls start from an ordinary probe and use the same durable
/// demand, installed-scanner binding, attempt heartbeat and publication fence
/// as the title worker. An ordinary upsert must continue invalidating aggregates.
#[cfg(all(test, target_os = "linux"))]
pub(crate) async fn publish_title_for_control(
    transcode: &crate::transcode::TranscodeManager,
    store: &std::sync::Arc<dyn plurx_core::store::Store>,
    file: &MediaFile,
) -> Result<DvTitleStats, String> {
    let tools = ToolSet::capture()
        .await?
        .ok_or("title scanner unavailable")?;
    publish_title_with_tools_for_control(transcode, store, file, tools).await
}

/// Lifecycle controls supply a scoped, artifact-hashed scanner executable.
/// The admitted durable job, held-source checks and publication fence remain
/// identical; this does not qualify the installed physical scanner.
#[cfg(all(test, target_os = "linux"))]
pub(crate) async fn publish_title_with_tools_for_control(
    transcode: &crate::transcode::TranscodeManager,
    store: &std::sync::Arc<dyn plurx_core::store::Store>,
    file: &MediaFile,
    tools: std::sync::Arc<ToolSet>,
) -> Result<DvTitleStats, String> {
    use plurx_core::store::{
        background_jobs::{ClaimJob, EnqueueJob, EnqueueOutcome, JobKind, JobPayload, JobRequest},
        dv_title_jobs::TitleOutput,
    };
    use sha2::{Digest, Sha256};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    let admission = transcode
        .admit_dv_title(false)
        .await
        .ok_or("title probe admission unavailable")?;
    let scanner = tools.title_scanner_digest();
    let source = crate::fragment_index_cluster::open_source_playback_fence(file, None).await?;
    let mut demand = store
        .dv_title_demands(&scanner, 0, crate::source_probe::now_ms())
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|demand| demand.file_id == file.id)
        .ok_or("ordinary probe created no current title demand")?;
    if demand.source_size != file.size || demand.source_mtime != file.mtime {
        return Err("title control source does not match ordinary probe demand".into());
    }
    demand.probe_revision = store
        .bind_dv_title_demand(demand.probe_revision, &scanner)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("title demand changed before scanner binding")?;
    let identity = hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            "dv-title-v1",
            demand.file_id,
            demand.probe_revision,
            &scanner,
        ))
        .map_err(|e| e.to_string())?,
    ));
    let now = super::now_ms();
    let outcome = store
        .enqueue_job(EnqueueJob {
            id: uuid::Uuid::new_v4().to_string(),
            payload: JobPayload::DvTitleScan {
                file_id: file.id,
                source_size: file.size,
                source_mtime: file.mtime,
                probe_revision: demand.probe_revision,
                scanner_digest: scanner.clone(),
            },
            dedupe_key: format!("dv-title:{identity}"),
            priority: 0,
            not_before_ms: now,
            now_ms: now,
            request: JobRequest {
                scope: "dv-title".into(),
                request_id: identity.clone(),
                request_digest: identity,
                consumer_kind: "dv_title_scan".into(),
                consumer_ref: file.id.to_string(),
                target_node_id: None,
                deadline_ms: None,
                retain_identity: false,
            },
        })
        .await
        .map_err(|e| e.to_string())?;
    let job_id = match outcome {
        EnqueueOutcome::Accepted { job_id, .. }
        | EnqueueOutcome::Existing {
            job_id,
            cancelled: false,
            ..
        } => job_id,
        _ => return Err("title control enqueue refused".into()),
    };
    let candidate = store
        .background_job(&job_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("enqueued title job unavailable")?;
    let now = super::now_ms();
    let (job, deadline) = crate::background_jobs::claim_with_resolution(
        store.as_ref(),
        &candidate,
        ClaimJob {
            job_id,
            expected_revision: candidate.revision,
            node_id: "title-control".into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind: JobKind::DvTitleScan,
            payload_version: 1,
            now_ms: now,
            dispatched_at_ms: now,
        },
    )
    .await
    .map_err(|e| e.to_string())?
    .ok_or("title control claim refused")?;
    let active = crate::background_jobs::ActiveBackgroundJob::start(
        Arc::clone(store),
        Arc::new(plurx_core::cluster::coordination::UnclusteredJobAuthority),
        job.token.ok_or("claimed title job has no token")?,
        deadline,
        JobKind::DvTitleScan,
    )
    .await
    .map_err(|e| e.to_string())?;
    let fence = active.fence();
    let result = async {
        let source_version = crate::fragment_index_cluster::inspect_source(file)
            .await
            .ok();
        let stats = super::run_source_admitted_probe(
            transcode,
            &admission,
            &fence.loss_token(),
            &CancellationToken::new(),
            None,
            &fence,
            || async {
                source_version.is_some()
                    && tools.is_current().await
                    && crate::fragment_index_cluster::inspect_source(file)
                        .await
                        .ok()
                        == source_version
            },
            scan(
                file,
                &demand.probe_json,
                &tools,
                transcode.runtime_cache_dir(),
            ),
        )
        .await
        .map_err(|error| error.to_string())?;
        if !tools.is_current().await || !source.unchanged() || source.reopen(file).await.is_err() {
            return Err("title scanner or source changed before control publication".into());
        }
        if !fence
            .publish_dv_title(TitleOutput {
                file_id: file.id,
                probe_revision: demand.probe_revision,
                source_size: demand.source_size,
                source_mtime: demand.source_mtime,
                scanner_digest: scanner,
                stats,
            })
            .await
            .map_err(|e| e.to_string())?
        {
            return Err("title control publication fenced".into());
        }
        let stored = store
            .get_file_probe_json(file.id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("published title probe unavailable")?;
        let published =
            DvTitleStats::from_probe_json(&stored).ok_or("published title facts unavailable")?;
        if published != stats {
            return Err("published title facts changed".into());
        }
        Ok(published)
    }
    .await;
    active.finish().await;
    result
}

#[cfg(any(unix, test))]
pub(super) fn selected_video_index(document: &serde_json::Value) -> Option<u8> {
    let stream = document.get("streams")?.as_array()?.iter().find(|stream| {
        stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
            && stream
                .get("disposition")
                .and_then(|v| v.get("attached_pic"))
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0)
                == 0
    })?;
    u8::try_from(stream.get("index")?.as_u64()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn title_scan_selects_the_catalogue_video_not_an_attached_cover() {
        let document = serde_json::json!({"streams":[
            {"index":0,"codec_type":"video","disposition":{"attached_pic":1}},
            {"index":1,"codec_type":"audio"},{"index":2,"codec_type":"video"},{"index":3,"codec_type":"video"}
        ]});
        assert_eq!(selected_video_index(&document), Some(2));
        assert_eq!(
            selected_video_index(&serde_json::json!({"streams":[]})),
            None
        );
    }
}

#[cfg(test)]
mod scanner_launch_tests {
    use super::*;
    use plurx_core::process::source_lifetime as lifetime;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn managed_title_scanner_reads_the_broker_with_a_bound_manifest() {
        const CHILD: &str = "PLURX_TITLE_SCANNER_MANIFEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = tokio::process::Command::new(std::env::current_exe().expect("test image"))
                .args([
                    "--exact",
                    concat!(
                        module_path!(),
                        "::managed_title_scanner_reads_the_broker_with_a_bound_manifest"
                    )
                    .trim_start_matches("plurxd::"),
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .kill_on_drop(true)
                .output()
                .await
                .expect("isolated source-policy test");
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        static OBSERVED: AtomicUsize = AtomicUsize::new(0);

        fn observe(value: &lifetime::LaunchObservation) -> std::io::Result<()> {
            assert_eq!(value.declared, lifetime::SourceAccess::Brokered);
            assert_eq!(
                value.reason,
                Some("broker_confinement_unqualified"),
                "the command, manifest, registered broker and daemon cwd must pass all preceding checks"
            );
            assert!(!value.verified, "the fixture cannot grant confinement");
            assert!(!value.descriptor_handoff, "legacy descriptor hook");
            OBSERVED.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        let root = crate::test_tempdir().expect("scanner fixture");
        let runtime = root.path().join("runtime");
        let tools = root.path().join("tools");
        std::fs::create_dir(&runtime).expect("managed runtime cwd");
        std::fs::create_dir(&tools).expect("managed tool root");
        let scanner = tools.join("scan_title");
        std::fs::write(
            &scanner,
            r#"#!/bin/sh
set -eu
test "$(pwd -P)" = "$(cd "${0%/*}/../runtime" && pwd -P)"
test "$2" = 0
test ! -e /dev/fd/3
exec /usr/bin/curl --noproxy '*' -fsS "$1"
"#,
        )
        .expect("write scanner fixture");
        std::fs::set_permissions(&scanner, std::fs::Permissions::from_mode(0o700))
            .expect("executable scanner fixture");
        let body = b"scanner source fixture\n".repeat(4096);
        let path = root.path().join("original.mkv");
        std::fs::write(&path, &body).expect("write source");
        let held = std::fs::File::open(&path).expect("hold source");

        let (_store, _id, active) = crate::background_jobs::tests::active().await;
        lifetime::set_daemon_roots(std::slice::from_ref(&runtime), &[tools])
            .expect("source-policy roots");
        lifetime::install_observer(observe).expect("isolated observer");
        let input = SourceInput::open_background_held(
            crate::fragment_index_cluster::SourceFence::clone_current_held(&held).expect("source"),
            "application/octet-stream",
            active.fence().source_budget(),
        )
        .await
        .expect("budgeted broker");
        let output = launch_scanner(
            &scanner,
            "0",
            &held,
            Some(&input),
            &runtime,
            Duration::from_secs(10),
            plurx_core::process::ChildWork::background("scanner manifest regression"),
        )
        .await
        .expect("bounded managed scanner");
        assert_eq!(input.observation().source_bytes, body.len() as u64);
        input.close_and_wait().await.expect("join broker");
        active.finish().await;
        assert!(output.status.success(), "{:?}", output.stderr);
        assert_eq!(output.stdout, body[..64 * 1024]);
        assert_eq!(OBSERVED.load(Ordering::SeqCst), 1);
    }
}
