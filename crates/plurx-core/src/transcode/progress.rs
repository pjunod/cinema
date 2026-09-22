//! Classification of FFmpeg `-progress` records shared by producer readers.

/// Whether one line is a key emitted by FFmpeg's `-progress` protocol.
///
/// This is deliberately a closed set. An unknown future progress key costs a
/// diagnostic log entry; accepting arbitrary lower-snake keys can hide a real
/// filter or bitstream-filter error such as `filter_units=remove_types=…`.
pub fn is_progress_line(line: &str) -> bool {
    let Some((key, _)) = line.split_once('=') else {
        return false;
    };
    matches!(
        key,
        "frame"
            | "fps"
            | "bitrate"
            | "total_size"
            | "out_time_us"
            | "out_time_ms"
            | "out_time"
            | "dup_frames"
            | "drop_frames"
            | "speed"
            | "progress"
    ) || (key.starts_with("stream_") && key.ends_with("_q"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_progress_keys_do_not_swallow_diagnostics() {
        for line in [
            "out_time_us=1234",
            "out_time_ms=1234",
            "speed=1.02x",
            "stream_0_0_q=-1.0",
            "progress=continue",
            "frame=142",
            "fps=59.94",
            "bitrate=N/A",
            "total_size=4096",
            "out_time=00:00:01.000000",
            "dup_frames=0",
            "drop_frames=0",
        ] {
            assert!(is_progress_line(line), "progress: {line}");
        }
        for line in [
            "filter_units=remove_types=32-34",
            "[AVBSFContext @ 0x55] Option remove_types=32-34 not found",
            "some_future_key=1",
            "[out#0/mp4 @ 0x1] Output file is empty, nothing was encoded",
            "Conversion failed!",
            "",
        ] {
            assert!(!is_progress_line(line), "diagnostic: {line}");
        }
    }
}
