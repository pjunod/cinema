//! Attach complete-title DV facts inside the existing admitted probe job.
//! The job's publication fence remains the only owner of catalogue writes.

use plurx_core::{
    domain::{MediaFile, ProbeResult},
    error::ProbeError,
};

pub(super) async fn probe(file: &MediaFile) -> Result<ProbeResult, ProbeError> {
    #[cfg(unix)]
    {
        probe_unix(file).await
    }
    #[cfg(not(unix))]
    {
        plurx_core::scan::probe::probe_single_threaded(&file.path).await
    }
}

#[cfg(unix)]
async fn probe_unix(file: &MediaFile) -> Result<ProbeResult, ProbeError> {
    use crate::fragment_index_cluster::open_source_fence;
    use plurx_core::scan::dv_title::{DvTitleStats, TitleEligibility, PROBE_FIELD};
    use std::{os::fd::AsRawFd, time::Duration};

    let changed = |reason: String| ProbeError::Transient {
        path: file.path.display().to_string(),
        reason,
    };
    // Bind before the stream probe as well as the title walk. The existing
    // publisher additionally compares the job's catalogue source revision.
    let source = open_source_fence(file, None).await.map_err(&changed)?;
    let mut result = plurx_core::scan::probe::probe_single_threaded(&file.path).await?;
    let Some(raw) = result.raw_json.as_deref() else {
        return Ok(result);
    };
    let mut document: serde_json::Value =
        serde_json::from_str(raw).map_err(|error| ProbeError::Parse(error.to_string()))?;
    if let Some(object) = document.as_object_mut() {
        // A fresh attempt never inherits a prior source's or partial scan's facts.
        object.remove(PROBE_FIELD);
    }
    if result.video_codec.as_deref() == Some("hevc")
        && matches!(result.dolby_vision.profile, Some(5 | 7 | 8))
        && result.dolby_vision.rpu_present == Some(true)
    {
        let scanned = async {
            let index = selected_video_index(&document).ok_or("unknown selected video stream")?;
            let tools = crate::dv_runtime::ToolSet::capture()
                .await?
                .ok_or("DV title scanner not installed")?;
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
            // Static FFmpeg/libdovi only. No inherited library override or GPU
            // environment is needed. Cancellation reaps the background process
            // before the caller's physical probe permit can be released.
            let output = plurx_core::process::bounded::output_command_clean_env(
                &mut command,
                Duration::from_secs(6 * 60 * 60),
                64 * 1024,
                plurx_core::process::ChildWork::background("complete-title Dolby Vision scan"),
            )
            .await
            .map_err(|error| error.to_string())?;
            if !output.status.success() {
                return Err("complete-title DV scan failed; master grade remains selected".into());
            }
            if !tools.is_current().await {
                return Err("DV scanner identity changed".into());
            }
            let stats: DvTitleStats = serde_json::from_slice(&output.stdout)
                .map_err(|error| format!("invalid DV title aggregate: {error}"))?;
            if stats.eligibility() == TitleEligibility::InvalidAggregate || !stats.complete {
                return Err("incomplete or inconsistent DV title aggregate".into());
            }
            Ok::<_, String>(stats)
        }
        .await;
        match scanned {
            Ok(stats) => {
                document[PROBE_FIELD] = serde_json::to_value(stats)
                    .map_err(|error| ProbeError::Parse(error.to_string()))?
            }
            // Missing helper, bad L1 or a failed read must not make the original
            // stream probe unusable. No aggregate means targeting is unknown.
            Err(reason) => tracing::debug!(%reason, "title DV scan unavailable"),
        }
    }
    if !source.unchanged() || source.reopen(file).await.is_err() {
        return Err(changed("source changed during stream/title probe".into()));
    }
    result.raw_json = Some(
        serde_json::to_string(&document).map_err(|error| ProbeError::Parse(error.to_string()))?,
    );
    Ok(result)
}

#[cfg(any(unix, test))]
fn selected_video_index(document: &serde_json::Value) -> Option<u8> {
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
        let document = serde_json::json!({"streams": [
            {"index": 0, "codec_type": "video", "disposition": {"attached_pic": 1}},
            {"index": 1, "codec_type": "audio"},
            {"index": 2, "codec_type": "video"},
            {"index": 3, "codec_type": "video"}
        ]});
        assert_eq!(selected_video_index(&document), Some(2));
        assert_eq!(
            selected_video_index(&serde_json::json!({"streams": []})),
            None
        );
    }
}
