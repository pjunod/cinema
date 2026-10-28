//! Full-title facts acquired only by the persistent, admitted title job.
use crate::vodencode::dv_runtime::ToolSet;
use plurx_core::{
    domain::MediaFile,
    scan::dv_title::{DvTitleStats, TitleEligibility},
};

#[cfg(unix)]
pub(super) async fn scan(
    file: &MediaFile,
    document: &str,
    tools: &ToolSet,
) -> Result<DvTitleStats, String> {
    use std::{os::fd::AsRawFd, time::Duration};
    let source = crate::fragment_index_cluster::open_source_playback_fence(file, None).await?;
    let document: serde_json::Value = serde_json::from_str(document).map_err(|e| e.to_string())?;
    let index = selected_video_index(&document).ok_or("unknown selected video stream")?;
    if !tools.is_current().await {
        return Err("DV scanner identity changed".into());
    }
    let mut command = tokio::process::Command::new(&tools.scanner);
    let descriptor = if cfg!(target_os = "linux") {
        "/proc/self/fd/3"
    } else {
        "/dev/fd/3"
    };
    command.args([descriptor, &index.to_string()]);
    crate::ffmpeg::inherit_raw_file_descriptors_with_cwd(
        &mut command,
        &[(source.handle.as_raw_fd(), 3)],
        None,
    );
    let output = plurx_core::process::bounded::output_command_clean_env(
        &mut command,
        Duration::from_secs(6 * 60 * 60),
        64 * 1024,
        plurx_core::process::ChildWork::background("complete-title Dolby Vision scan"),
    )
    .await
    .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("complete-title DV scan failed".into());
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
        .admit_dv_title()
        .await
        .ok_or("title probe admission unavailable")?;
    let scanner = tools.title_scanner_digest();
    let source = crate::fragment_index_cluster::open_source_playback_fence(file, None).await?;
    let mut demand = store
        .dv_title_demands(&scanner, 0)
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
    .map_err(|e| e.to_string())?;
    let fence = active.fence();
    let result = async {
        let stats = super::run_admitted_probe(
            transcode,
            &admission,
            &fence.loss_token(),
            &CancellationToken::new(),
            None,
            scan(file, &demand.probe_json, &tools),
        )
        .await?;
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
