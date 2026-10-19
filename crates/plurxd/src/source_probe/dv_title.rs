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
