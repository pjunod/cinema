//! Source-owned native text subtitle facts and windowed delivery.
//!
//! Subtitles never gate a Source start. The Source publishes its subtitle
//! group from the track facts the prepared file already holds, and serves
//! cues one window at a time through the same cache, cue-index fast path and
//! background warmers local playback uses. Whole-track extraction stays the
//! background `subtitle_source` job; nothing here starts a demux before video.
use super::source_actor::SourceProducerAuthority;
use plurx_core::{
    domain::MediaFile, sharing_source_sessions::SourceDispatchAssignment, store::Store,
};
use std::{collections::BTreeSet, path::PathBuf, time::Instant};

const TRACK_BYTES: usize = 2 * 1024 * 1024;
const TRACKS: usize = 32;
const CUES: usize = 50_000;

/// The Source's native subtitle facts: which tracks it offers, never their
/// cues. Cues are resolved per window when a segment or a cached revision is
/// asked for.
pub(super) struct SourceNativeTracks {
    assignment: SourceDispatchAssignment,
    object_version: String,
    probe: String,
    indexes: BTreeSet<u16>,
    selected: Option<i64>,
    file: MediaFile,
    subs_dir: PathBuf,
    access: crate::subtitle_source::StoreAccess,
    store: std::sync::Arc<dyn Store>,
}
impl SourceNativeTracks {
    /// Held facts only: the prepared file's subtitle streams and the stored
    /// probe evidence. No child process, no file read.
    pub(super) async fn prepare(
        file: &MediaFile,
        object_version: &str,
        assignment: &SourceDispatchAssignment,
        gate: &SourceProducerAuthority,
        selected: Option<i64>,
        state: &crate::state::AppState,
    ) -> Result<Self, String> {
        let indexes: BTreeSet<u16> = supported_tracks(file, selected)?.into_iter().collect();
        let initial = gate
            .current_preparation(assignment)
            .await
            .map_err(|_| "Source native current authority unavailable".to_owned())?;
        let probe = state
            .store
            .source_index_probe_evidence(&initial)
            .await
            .map_err(|_| "Source native evidence read failed".to_owned())?
            .ok_or_else(|| "Source native stored probe unavailable".to_owned())?;
        Ok(Self {
            assignment: assignment.clone(),
            object_version: object_version.into(),
            probe,
            indexes,
            selected,
            file: file.clone(),
            subs_dir: state.subs_dir.clone(),
            access: state.subtitle_source_access(),
            store: std::sync::Arc::clone(&state.store),
        })
    }
    pub(super) fn matches(&self, assignment: &SourceDispatchAssignment, object: &str) -> bool {
        self.assignment.same_identity(assignment) && self.object_version == object
    }
    pub(super) fn indexes(&self) -> BTreeSet<u16> {
        self.indexes.clone()
    }
    pub(super) fn has_track(&self, index: u16) -> bool {
        self.indexes.contains(&index)
    }
    pub(super) fn selected(&self) -> Option<i64> {
        self.selected
    }
    pub(super) fn probe(&self) -> &str {
        &self.probe
    }
    async fn window_seconds(&self) -> i64 {
        let stored = self
            .store
            .get_setting(plurx_core::store::keys::SUBTITLE_WINDOW_SECS)
            .await
            .ok()
            .flatten();
        plurx_core::store::bounded_subtitle_window_seconds(stored.as_deref())
    }
    /// What the cache holds for the demanded position, and the versioned
    /// revision identity (`f{file}-s{track}-{size}-{mtime}[-w{anchor}-{span}].vtt`,
    /// the grammar local playback publishes) the `cached-` route serves. Never
    /// starts work.
    pub(super) async fn readiness(
        &self,
        index: u16,
        demand_seconds: i64,
    ) -> (crate::playback_control::SubtitleTrackCache, Option<String>) {
        use crate::playback_control::SubtitleTrackCache as Cache;
        use crate::subtitles::SidecarState as Sidecar;
        if !self.has_track(index) {
            return (Cache::Unavailable, None);
        }
        let (state, revision) = crate::subtitles::readiness_revision_with_store(
            &self.subs_dir,
            &self.file,
            i64::from(index),
            demand_seconds,
            self.window_seconds().await,
            &self.access,
        )
        .await;
        let cache = match state {
            Sidecar::Ready => Cache::Ready,
            Sidecar::Failed => Cache::Unavailable,
            Sidecar::Warming | Sidecar::Absent => Cache::Warming,
        };
        (cache, revision)
    }
    /// Redeem one `cached-` revision. `None` is absent: a revision this Source
    /// did not publish for this file and track (including the retired
    /// `source-{incarnation}-{generation}-{track}.vtt` identity) or one not warm.
    pub(super) async fn cached(
        &self,
        index: u16,
        revision: &str,
    ) -> Result<Option<(Vec<u8>, bool)>, String> {
        if !self.has_track(index) {
            return Ok(None);
        }
        let cached = crate::subtitles::read_cached_revision(
            &self.subs_dir,
            &self.file,
            i64::from(index),
            revision,
        )
        .await?;
        match cached {
            Some((bytes, complete)) if validate_text(&bytes).is_ok() => Ok(Some((bytes, complete))),
            Some(_) => Err("Source native window is unsupported".into()),
            None => Ok(None),
        }
    }
    /// One HLS subtitle segment, cut from the window the cache can serve now.
    /// A cold cache answers an empty document and warms in the background.
    pub(super) async fn segment(
        &self,
        session: &str,
        index: u16,
        video: &[u8],
        sequence: u64,
        origin: f64,
        deadline: Instant,
    ) -> Result<Option<Vec<u8>>, String> {
        if !self.has_track(index) {
            return Ok(None);
        }
        let Some((start, end)) = crate::http::hls::source_native_segment_window(video, sequence)
        else {
            return Ok(None);
        };
        let (bytes, _absolute) = crate::http::hls::source_window_cues(
            session,
            &self.subs_dir,
            &self.file,
            i64::from(index),
            (origin + start).max(0.0) as i64,
            self.window_seconds().await,
            &self.access,
            deadline,
        )
        .await;
        // A cold answer is an empty document, and it still carries the HLS
        // timestamp map: a client that pins the segment's time base from the
        // empty answer must not lose it when the cues arrive on retry.
        let Some(out) = crate::http::hls::source_native_slice(&bytes, origin, start, end) else {
            return Ok(None);
        };
        validate_text(&out)?;
        Ok(Some(out))
    }
}
/// Only embedded plain text tracks are included in this finite lane.
pub(super) fn supported_tracks(
    file: &MediaFile,
    selected: Option<i64>,
) -> Result<Vec<u16>, String> {
    if file.subtitle_streams.len() > 4096 {
        return Err("Source subtitle metadata exceeds bound".into());
    }
    let tracks: Vec<u16> = file
        .subtitle_streams
        .iter()
        .enumerate()
        .filter(|(i, s)| {
            file.downloaded_subtitle(*i as i64).is_none()
                && plurx_core::tracks::is_native_text_subtitle(&s.codec)
        })
        .map(|(i, _)| i as u16)
        .collect();
    if tracks.is_empty()
        || tracks.len() > TRACKS
        || selected.is_some_and(|i| !(0..=4095).contains(&i) || !tracks.contains(&(i as u16)))
    {
        return Err("Source native track is unsupported".into());
    }
    Ok(tracks)
}
/// Bounds applied to every window a Source serves: UTF-8, size, cue count and
/// timing shape. The header may carry only the HLS timestamp map.
fn validate_text(bytes: &[u8]) -> Result<(), String> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| "Source native text is not UTF-8".to_owned())?;
    if !text.starts_with("WEBVTT") || bytes.len() > TRACK_BYTES || text.contains('\0') {
        return Err("Source native document is unsupported".into());
    }
    // Keep parser overhead bounded by document bytes, not one allocation
    // per line in a deliberately newline-heavy document.
    let normalized = text.replace("\r\n", "\n");
    let mut blocks = normalized
        .split("\n\n")
        .filter(|block| !block.trim().is_empty());
    let header = blocks
        .next()
        .ok_or_else(|| "Source native header missing".to_owned())?;
    let mut header_lines = header.trim().lines();
    if header_lines.next() != Some("WEBVTT")
        || header_lines.any(|line| !line.starts_with("X-TIMESTAMP-MAP="))
    {
        return Err("Source native header unsupported".into());
    }
    let mut count = 0usize;
    for block in blocks {
        // Cue text may contain a literal arrow. Only the timing header (or
        // the timing header following a cue identifier) is parsed as timing.
        let mut lines = block.lines();
        let first = lines
            .next()
            .ok_or_else(|| "Source native cue missing".to_owned())?;
        let line = if first.contains("-->") {
            first
        } else {
            lines
                .next()
                .ok_or_else(|| "Source native cue timing missing".to_owned())?
        };
        count += 1;
        if count > CUES || line.len() > 4096 {
            return Err("Source native cue bound exceeded".into());
        }
        let (left, right) = line
            .split_once("-->")
            .ok_or_else(|| "Source native cue malformed".to_owned())?;
        let end = right
            .split_whitespace()
            .next()
            .ok_or_else(|| "Source native cue end missing".to_owned())?;
        let start = timestamp(left)?;
        let end = timestamp(end)?;
        if end < start {
            return Err("Source native cue runs backwards".into());
        }
    }
    Ok(())
}
fn timestamp(raw: &str) -> Result<f64, String> {
    let parts: Vec<_> = raw.trim().split(':').collect();
    let parsed = match parts.as_slice() {
        [h, m, s] => h
            .parse::<u64>()
            .ok()
            .zip(m.parse::<u64>().ok())
            .zip(s.parse::<f64>().ok())
            .filter(|((_, m), s)| *m < 60 && s.is_finite() && *s >= 0.0 && *s < 60.0)
            .map(|((h, m), s)| h as f64 * 3600.0 + m as f64 * 60.0 + s),
        [m, s] => m
            .parse::<u64>()
            .ok()
            .zip(s.parse::<f64>().ok())
            .filter(|(_, s)| s.is_finite() && *s >= 0.0 && *s < 60.0)
            .map(|(m, s)| m as f64 * 60.0 + s),
        _ => None,
    };
    parsed
        .filter(|n| n.is_finite() && *n <= 1_728_000.0)
        .ok_or_else(|| "Source native cue timestamp exceeds bound".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_native_text_refuses_malformed_and_excessive_cues() {
        assert!(validate_text(b"WEBVTT\n\n00:00:00.200 --> 00:00:01.800\nactual\n").is_ok());
        assert!(
            validate_text(b"WEBVTT\n\ncued-id\n00:00:00.200 --> 00:00:01.800\nGo --> home\n")
                .is_ok()
        );
        assert!(
            validate_text(b"WEBVTT\r\n\r\n00:00:00.200 --> 00:00:01.800\r\nGo --> home\r\n")
                .is_ok()
        );
        for malformed in ["00:00:NaN", "00:00:60", "00:60:00", "-1:00:00", "481:00:00"] {
            assert!(validate_text(
                format!("WEBVTT\n\n{malformed} --> 00:00:02.000\ntext\n").as_bytes()
            )
            .is_err());
        }
        assert!(validate_text(b"WEBVTT\n\n00:00:02.000 --> 00:00:01.000\ntext\n").is_err());
        assert!(validate_text(b"WEBVTT\0").is_err());
        assert!(validate_text(b"WEBVTT\n00:00:00.000 --> 00:00:01.000\n\n").is_err());
        // A served window carries the HLS timestamp map and nothing else.
        assert!(validate_text(
            b"WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:0,LOCAL:00:00:00.000\n\n00:00:00.200 --> 00:00:01.800\nGo\n"
        )
        .is_ok());
        assert!(
            validate_text(b"WEBVTT\nNOTE smuggled\n\n00:00:00.200 --> 00:00:01.800\nGo\n").is_err()
        );
        assert!(validate_text(&vec![b'x'; TRACK_BYTES + 1]).is_err());
        let excessive = format!(
            "WEBVTT\n\n{}",
            "00:00:00.000 --> 00:00:01.000\n\n".repeat(CUES + 1)
        );
        assert!(validate_text(excessive.as_bytes()).is_err());
    }
}
