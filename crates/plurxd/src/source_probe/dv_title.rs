//! Full-title facts acquired only by the persistent, admitted title job.
use crate::{dv_tools::ToolSet, source_input::SourceInput};
use plurx_core::{
    domain::MediaFile,
    scan::dv_title::{DvTitleStats, TitleEligibility},
};

use std::{path::Path, time::Duration};

mod m7_census;
pub(super) use m7_census::{M7WindowRange, VerifiedSourceCensus};

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
    launch_scanner_with_args(
        scanner,
        index,
        held,
        input,
        runtime_cache,
        wall_time,
        work,
        &[],
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "scanner process, source and owner controls"
)]
async fn launch_scanner_with_args(
    scanner: &Path,
    index: &str,
    held: &std::fs::File,
    input: Option<&SourceInput>,
    runtime_cache: &Path,
    wall_time: Duration,
    work: plurx_core::process::ChildWork,
    args: &[String],
) -> std::io::Result<plurx_core::process::bounded::Output> {
    use crate::producer_spawn::{ChildStdio, ChildStdios, Descriptors};

    let mut command = tokio::process::Command::new(scanner);
    command.current_dir(runtime_cache);
    let descriptor = if cfg!(target_os = "linux") {
        "/proc/self/fd/3"
    } else {
        "/dev/fd/3"
    };
    command
        .args([input.map_or(descriptor, SourceInput::url), index])
        .args(args);
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
) -> Result<DvTitleStats, String> {
    scan_owned(file, document, tools, runtime_cache, None)
        .await
        .map(|(stats, _)| stats)
        .map_err(CensusScanError::into_message)
}

/// Why a complete-title scan ended without a verified aggregate.
#[derive(Debug)]
pub(super) enum CensusScanError {
    /// Something a later attempt need not repeat: a busy or failed source
    /// read, a signal or wall-time kill, a launch error, changed tools or a
    /// changed source, or a census the daemon could not verify.
    Transient(String),
    /// The current scanner exited on its own after reading the whole held
    /// source, which is still unchanged, and refused it. The same scanner
    /// refuses the same source again, so retrying cannot settle it.
    Refused(String),
}

impl CensusScanError {
    pub(super) fn into_message(self) -> String {
        match self {
            Self::Transient(message) | Self::Refused(message) => message,
        }
    }
}

impl From<String> for CensusScanError {
    fn from(message: String) -> Self {
        Self::Transient(message)
    }
}

impl From<&str> for CensusScanError {
    fn from(message: &str) -> Self {
        Self::Transient(message.into())
    }
}

/// A failed scan is a refusal only when the scanner exited by itself (not by
/// a signal, which a wall-time kill or crash leaves) over inputs that are
/// still current.
fn classify_scan_failure(
    output: &plurx_core::process::bounded::Output,
    inputs_current: bool,
) -> CensusScanError {
    let message = scan_failure(output);
    if inputs_current && output.status.code().is_some() {
        CensusScanError::Refused(message)
    } else {
        CensusScanError::Transient(message)
    }
}

/// Select the planned windows before the whole scan. Their actual source
/// ordinals and AU hashes are settled by that same scanner at complete EOF.
pub(super) async fn scan_with_m7_census(
    file: &MediaFile,
    document: &str,
    tools: &ToolSet,
    runtime_cache: &Path,
    ranges: &[M7WindowRange],
) -> Result<(DvTitleStats, VerifiedSourceCensus), CensusScanError> {
    let (stats, census) = scan_owned(file, document, tools, runtime_cache, Some(ranges)).await?;
    Ok((stats, census.ok_or("planned M7 census was not returned")?))
}

async fn scan_owned(
    file: &MediaFile,
    document: &str,
    tools: &ToolSet,
    runtime_cache: &Path,
    ranges: Option<&[M7WindowRange]>,
) -> Result<(DvTitleStats, Option<VerifiedSourceCensus>), CensusScanError> {
    let args = ranges.map(m7_census::arguments).transpose()?;
    let census_dir = ranges
        .map(|_| tempfile::tempdir_in(runtime_cache))
        .transpose()
        .map_err(|error| error.to_string())?;
    let source = crate::fragment_index_cluster::open_source_playback_fence(file, None).await?;
    let document: serde_json::Value = serde_json::from_str(document).map_err(|e| e.to_string())?;
    let index = selected_video_index(&document).ok_or("unknown selected video stream")?;
    if !tools.is_current().await {
        return Err("DV scanner identity changed".into());
    }
    let budget = crate::background_jobs::source_budget();
    if ranges.is_some() && budget.is_none() {
        return Err("planned M7 census requires an owned title source budget".into());
    }
    let input = match budget {
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
    let work = plurx_core::process::ChildWork::background("complete-title Dolby Vision scan");
    let index_text = index.to_string();
    let wall_time = Duration::from_secs(6 * 60 * 60);
    let output = if let (Some(args), Some(census_dir)) = (args.as_ref(), census_dir.as_ref()) {
        launch_scanner_with_args(
            &tools.scanner,
            &index_text,
            &source.handle,
            input.as_ref(),
            census_dir.path(),
            wall_time,
            work,
            args,
        )
        .await
    } else {
        launch_scanner(
            &tools.scanner,
            &index_text,
            &source.handle,
            input.as_ref(),
            runtime_cache,
            wall_time,
            work,
        )
        .await
    };
    let source_read_failed = input.as_ref().is_some_and(SourceInput::source_read_failed);
    if let Some(input) = input {
        input
            .close_and_wait()
            .await
            .map_err(|error| format!("joining title source input: {error}"))?;
    }
    if ranges.is_some() && source_read_failed {
        return Err("planned M7 title source read failed".into());
    }
    let output = output.map_err(|error| error.to_string())?;
    if !output.status.success() {
        let inputs_current = !source_read_failed
            && tools.is_current().await
            && source.unchanged()
            && source.reopen(file).await.is_ok();
        return Err(classify_scan_failure(&output, inputs_current));
    }
    if !tools.is_current().await {
        return Err("DV scanner identity changed".into());
    }
    if !source.unchanged() || source.reopen(file).await.is_err() {
        return Err("source changed during title scan".into());
    }
    let stats: DvTitleStats =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("invalid DV aggregate: {e}"))?;
    if stats.eligibility() == TitleEligibility::InvalidAggregate || !stats.complete {
        return Err("incomplete or inconsistent DV aggregate".into());
    }
    let census = census_dir
        .zip(ranges)
        .map(|(dir, ranges)| {
            m7_census::read(
                &dir.path().join(m7_census::OUTPUT),
                &source,
                tools,
                index,
                stats.decoded_frames,
                ranges,
            )
        })
        .transpose()?;
    Ok((stats, census))
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
        .await?;
        if !tools.is_current().await || !source.unchanged() || source.reopen(file).await.is_err() {
            return Err("title scanner or source changed before control publication".into());
        }
        if !fence
            .publish_dv_title(
                TitleOutput {
                    file_id: file.id,
                    probe_revision: demand.probe_revision,
                    source_size: demand.source_size,
                    source_mtime: demand.source_mtime,
                    scanner_digest: scanner,
                    stats,
                },
                None,
            )
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

/// Longest scanner refusal carried into a title job's error.
const MAX_SCAN_FAILURE_BYTES: usize = 512;

/// A failed scan names how the scanner ended and, when it said, why: its
/// last `DV title scan refused:` line, else the last line it wrote. Without
/// it a refusal is indistinguishable from any other failure.
fn scan_failure(output: &plurx_core::process::bounded::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines = || {
        stderr
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
    };
    let reason = lines()
        .rfind(|line| line.starts_with("DV title scan refused:"))
        .or_else(|| lines().next_back());
    let mut message = format!("complete-title DV scan failed ({})", output.status);
    if let Some(reason) = reason {
        let mut end = reason.len().min(MAX_SCAN_FAILURE_BYTES);
        while !reason.is_char_boundary(end) {
            end -= 1;
        }
        message.push_str(": ");
        message.push_str(&reason[..end]);
    }
    message
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

    #[cfg(unix)]
    #[test]
    fn a_failed_scan_names_the_scanners_refusal() {
        use std::os::unix::process::ExitStatusExt;
        let output = |status: i32, stderr: &str| plurx_core::process::bounded::Output {
            status: std::process::ExitStatus::from_raw(status),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        };
        let refused = scan_failure(&output(
            1 << 8,
            "[hevc] noise\nDV title scan refused: complete demux EOF\n[mkv] after\n",
        ));
        assert!(refused.contains("exit status: 1"), "{refused}");
        assert!(
            refused.ends_with(": DV title scan refused: complete demux EOF"),
            "{refused}"
        );
        let other = scan_failure(&output(1 << 8, "first\nlast words\n\n"));
        assert!(other.ends_with(": last words"), "{other}");
        let silent = scan_failure(&output(9, ""));
        assert!(
            silent.starts_with("complete-title DV scan failed ("),
            "{silent}"
        );
        assert!(!silent.contains("): "), "{silent}");
        let long = scan_failure(&output(1 << 8, &"é".repeat(2 * MAX_SCAN_FAILURE_BYTES)));
        assert!(long.len() <= 64 + MAX_SCAN_FAILURE_BYTES, "{}", long.len());
    }

    /// Eclipse's census refusal retried every maintenance pass because every
    /// scan failure was a retry. Only the scanner's own exit over current
    /// inputs is a refusal; a kill or stale inputs still retry.
    #[cfg(unix)]
    #[test]
    fn only_the_scanners_own_exit_over_current_inputs_is_a_refusal() {
        use std::os::unix::process::ExitStatusExt;
        let output = |status: i32| plurx_core::process::bounded::Output {
            status: std::process::ExitStatus::from_raw(status),
            stdout: Vec::new(),
            stderr: b"DV title scan refused: exact M7 census interval end\n".to_vec(),
        };
        let refused = classify_scan_failure(&output(1 << 8), true);
        assert!(
            matches!(&refused, CensusScanError::Refused(message)
                if message.ends_with("exact M7 census interval end")),
            "{refused:?}"
        );
        assert!(matches!(
            classify_scan_failure(&output(1 << 8), false),
            CensusScanError::Transient(_)
        ));
        assert!(matches!(
            classify_scan_failure(&output(9), true),
            CensusScanError::Transient(_)
        ));
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
