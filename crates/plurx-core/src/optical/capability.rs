use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpticalCapability {
    Available,
    Missing,
    ProbeFailed { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpticalCapabilities {
    pub ffmpeg_reporter: Option<String>,
    pub ffprobe_reporter: Option<String>,
    pub dvd_titles: OpticalCapability,
    pub bluray_titles: OpticalCapability,
    pub helper_available: bool,
}

/// Interpret FFmpeg's help response without trusting its exit status.
///
/// FFmpeg exits zero after printing `Unknown format` and `Unknown protocol` on
/// several builds. Capability detection therefore needs positive help text and
/// must reject those diagnostic strings explicitly.
pub fn classify_help_output(
    stdout: &[u8],
    stderr: &[u8],
    expected_terms: &[&str],
) -> OpticalCapability {
    let mut text = String::from_utf8_lossy(stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(stderr));
    let lower = text.to_ascii_lowercase();
    if lower.contains("unknown format")
        || lower.contains("unknown protocol")
        || lower.contains("not found")
    {
        return OpticalCapability::Missing;
    }
    if expected_terms
        .iter()
        .all(|term| lower.contains(&term.to_ascii_lowercase()))
    {
        return OpticalCapability::Available;
    }
    let detail = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("capability probe produced no output")
        .chars()
        .take(240)
        .collect();
    OpticalCapability::ProbeFailed { detail }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optical_zero_exit_unknown_help_is_missing_not_available() {
        assert_eq!(
            classify_help_output(b"", b"Unknown format 'dvdvideo'.\n", &["dvdvideo"]),
            OpticalCapability::Missing
        );
        assert_eq!(
            classify_help_output(b"", b"Unknown protocol 'bluray'.\n", &["bluray"]),
            OpticalCapability::Missing
        );
    }

    #[test]
    fn optical_capability_needs_all_positive_markers() {
        assert_eq!(
            classify_help_output(
                b"Demuxer dvdvideo [DVD-Video]\ntitle playback title number\n",
                b"",
                &["dvdvideo", "title"]
            ),
            OpticalCapability::Available
        );
        assert!(matches!(
            classify_help_output(b"ffmpeg help", b"", &["dvdvideo", "title"]),
            OpticalCapability::ProbeFailed { .. }
        ));
    }
}
