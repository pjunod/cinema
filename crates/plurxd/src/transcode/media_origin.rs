use super::*;

/// Where a copy session's media actually begins in the source.
///
/// See [`plurx_core::transcode::keyframe_probe_args`] for why the requested
/// start is not the answer. Every failure path returns `start_seconds`, which
/// is both the old behaviour and the correct answer whenever the requested
/// offset happens to land on a keyframe.
pub(crate) async fn probe_media_origin(source_path: &std::path::Path, start_seconds: f64) -> f64 {
    if start_seconds <= 0.0 {
        return 0.0;
    }
    #[cfg(windows)]
    let source = {
        let path = source_path.to_owned();
        let opened = tokio::task::spawn_blocking(move || {
            plurx_core::fs_secure::open_read_nofollow_blocking(&path)
        })
        .await;
        let Ok(Ok(source)) = opened else {
            tracing::warn!(
                target: "plurxd::transcode",
                start_seconds, "media-origin source could not be held"
            );
            return start_seconds;
        };
        source
    };
    #[cfg(windows)]
    let input = match crate::ffmpeg::windows_source_path(&source) {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(
                target: "plurxd::transcode",
                start_seconds, %error, "media-origin source path could not be resolved"
            );
            return start_seconds;
        }
    };
    #[cfg(not(windows))]
    let input = source_path.to_owned();
    let args = plurx_core::transcode::keyframe_probe_args(&input.to_string_lossy(), start_seconds);
    let mut command = tokio::process::Command::new(crate::ffmpeg::ffprobe_bin());
    command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    if let Err(error) = crate::ffmpeg::verify_windows_source_path(&source, &input) {
        tracing::warn!(
            target: "plurxd::transcode",
            start_seconds, %error, "media-origin source changed before probe"
        );
        return start_seconds;
    }
    let probe = crate::process_control::output_job_owned(
        &mut command,
        crate::process_control::ChildWork::realtime("playback start media probe"),
    );
    let Ok(Ok(out)) = tokio::time::timeout(MEDIA_ORIGIN_PROBE_TIMEOUT, probe).await else {
        tracing::warn!(
            target: "plurxd::transcode",
            start_seconds,
            "media-origin probe did not answer; subtitle cues fall back to the requested start"
        );
        return start_seconds;
    };
    if !out.status.success() {
        tracing::warn!(
            target: "plurxd::transcode",
            start_seconds,
            "media-origin probe failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return start_seconds;
    }
    let origin =
        plurx_core::transcode::parse_keyframe_origin(&String::from_utf8_lossy(&out.stdout))
            // A keyframe *after* the requested start would mean the demuxer
            // seeked forward, which `-noaccurate_seek` does not do. Treat it as a
            // probe that misread rather than moving cues the wrong way.
            .filter(|origin| *origin <= start_seconds + 0.001)
            .unwrap_or(start_seconds);
    if (origin - start_seconds).abs() > 0.05 {
        tracing::info!(
            target: "plurxd::transcode",
            start_seconds,
            origin,
            lead_seconds = start_seconds - origin,
            "copy session begins at the preceding keyframe; subtitle cues shift by the origin"
        );
    }
    origin
}

/// A copy session must use the preceding keyframe reported on stdout, not the
/// requested seek time. Generate exactly two keyframes so the expected origin
/// is a property of this fixture rather than of an installed media file.
#[cfg(test)]
#[tokio::test]
#[ignore = "needs ffmpeg"]
pub(super) async fn probe_media_origin_reads_the_preceding_keyframe() {
    plurx_core::testfixtures::require_ffmpeg();
    let directory = crate::test_tempdir().expect("media-origin fixture");
    let source = directory.path().join("two-keyframes.mp4");
    let output = std::process::Command::new(plurx_core::testfixtures::ffmpeg())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=15:duration=2",
        ])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "15",
            "-keyint_min",
            "15",
            "-sc_threshold",
            "0",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&source)
        .output()
        .expect("generate media-origin fixture");
    assert!(
        output.status.success(),
        "fixture encode failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let requested = 1.5;
    let origin = probe_media_origin(&source, requested).await;
    assert!(
        (origin - 1.0).abs() < 0.05,
        "expected the preceding 1.0 s keyframe, got {origin}"
    );
    assert_ne!(origin, requested, "probe fell back to the requested seek");
}
