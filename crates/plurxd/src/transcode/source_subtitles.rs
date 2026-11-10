//! Source native track facts and bounded, requested subtitle windows.
//! Physical work stays inside the actor's counted resource/control factory;
//! no generic warmer or subtitle flight can confer Source authority.
use super::{
    source_actor::resource::SourceResourceReadCustody, source_preparation::SourceProbeHookOwner,
    TranscodeManager,
};
use plurx_core::{domain::MediaFile, sharing_source_sessions::SourceDispatchAssignment};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Mutex,
    time::Instant,
};

const TRACK_BYTES: usize = 2 * 1024 * 1024;
const TRACKS: usize = 32;
const CUES: usize = 50_000;
const HELD_WINDOWS: usize = 4;

pub(super) struct SourceNativeTracks {
    assignment: SourceDispatchAssignment,
    file: MediaFile,
    indexes: BTreeSet<u16>,
    selected: Option<i64>,
    presentation_probe: Mutex<Option<String>>,
    directory: PathBuf,
    windows: Mutex<BTreeMap<String, Vec<u8>>>,
    flight: tokio::sync::Mutex<()>,
}
impl SourceNativeTracks {
    /// Prepared track facts only. No file open, probe, admission or extraction.
    pub(super) fn prepare(
        file: &MediaFile,
        assignment: &SourceDispatchAssignment,
        selected: Option<i64>,
        directory: PathBuf,
    ) -> Result<Self, String> {
        Ok(Self {
            assignment: assignment.clone(),
            file: file.clone(),
            indexes: supported_tracks(file, selected)?.into_iter().collect(),
            selected,
            directory,
            presentation_probe: Mutex::new(None),
            windows: Mutex::new(BTreeMap::new()),
            flight: tokio::sync::Mutex::new(()),
        })
    }
    pub(super) fn matches(&self, assignment: &SourceDispatchAssignment, file: &MediaFile) -> bool {
        self.assignment.same_identity(assignment)
            && self.file.id == file.id
            && self.file.size == file.size
            && self.file.mtime == file.mtime
            && self.file.path == file.path
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
    // Copy recipe preparation already reads this bounded evidence. Captions
    // reuse its projection facts without adding a probe prerequisite or read.
    pub(super) fn set_presentation_probe(&self, probe: Option<&str>) {
        *self
            .presentation_probe
            .lock()
            .expect("Source copy presentation") = probe.map(str::to_owned);
    }
    pub(super) fn presentation_probe(&self) -> Option<String> {
        self.presentation_probe
            .lock()
            .expect("Source copy presentation")
            .clone()
    }
    fn remember(&self, revision: String, bytes: Vec<u8>) {
        let mut windows = self.windows.lock().expect("Source caption cache");
        if windows.len() >= HELD_WINDOWS {
            windows.pop_first();
        }
        windows.insert(revision, bytes);
    }
    fn window_path(&self, index: u16, demand: i64, span: i64, object: &str) -> PathBuf {
        let anchor = crate::subtitles::window_anchor_seconds(demand, span);
        self.directory.join(format!(
            "f{}-s{}-{}-{}-o{}-w{}-{}.vtt",
            self.file.id,
            index,
            self.file.size,
            self.file.mtime,
            hex::encode(Sha256::digest(object.as_bytes())),
            anchor,
            span
        ))
    }
    #[cfg(test)]
    pub(super) fn take_held_revision_for_test(&self) -> String {
        let mut windows = self.windows.lock().expect("Source caption cache");
        let revision = windows
            .keys()
            .next()
            .expect("actual derived Source window")
            .clone();
        windows.clear();
        revision
    }
    #[cfg(test)]
    pub(super) fn revision_for_object_for_test(&self, object: &str) -> String {
        Self::revision(&self.window_path(0, 0, 200, object)).expect("Source object revision")
    }
    fn revision(path: &Path) -> Result<String, String> {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .ok_or_else(|| "Source subtitle revision is not UTF-8".into())
    }
    /// Readiness only observes bytes this exact Source has already accepted.
    pub(super) fn readiness(
        &self,
        index: u16,
        demand: i64,
        span: i64,
        custody: &SourceResourceReadCustody,
    ) -> (crate::playback_control::SubtitleTrackCache, Option<String>) {
        use crate::playback_control::SubtitleTrackCache as Cache;
        if !self.has_track(index) {
            return (Cache::Unavailable, None);
        }
        let revision = custody.source().ok().and_then(|source| {
            Self::revision(&self.window_path(index, demand, span, source.object_version())).ok()
        });
        if revision.as_ref().is_some_and(|revision| {
            self.windows
                .lock()
                .expect("Source caption cache")
                .contains_key(revision)
        }) {
            (Cache::Ready, revision)
        } else {
            (Cache::Warming, None)
        }
    }
    /// Only exact versioned identities for this file/track can name a cache
    /// object. The legacy in-session grammar remains parseable but absent.
    pub(super) async fn cached(
        &self,
        index: u16,
        revision: &str,
        custody: &SourceResourceReadCustody,
    ) -> Result<Option<(Vec<u8>, bool)>, String> {
        if !self.has_track(index) {
            return Ok(None);
        }
        let object = custody.source()?.object_version();
        let prefix = format!(
            "f{}-s{}-{}-{}-o{}-w",
            self.file.id,
            index,
            self.file.size,
            self.file.mtime,
            hex::encode(Sha256::digest(object.as_bytes()))
        );
        let Some((anchor, span)) = revision
            .strip_prefix(&prefix)
            .and_then(|value| value.strip_suffix(".vtt"))
            .and_then(|value| value.split_once('-'))
        else {
            return Ok(None);
        };
        let (Ok(anchor), Ok(span)) = (anchor.parse::<i64>(), span.parse::<i64>()) else {
            return Ok(None);
        };
        if anchor < 0
            || span != crate::subtitles::bounded_window_seconds(span)
            || anchor != crate::subtitles::window_anchor_seconds(anchor, span)
        {
            return Ok(None);
        }
        let path = self.window_path(index, anchor, span, object);
        if Self::revision(&path)? != revision {
            return Ok(None);
        }
        let complete = false;
        custody.source()?;
        if let Some(bytes) = self
            .windows
            .lock()
            .expect("Source caption cache")
            .get(revision)
            .cloned()
        {
            return Ok(Some((bytes, complete)));
        }
        let bytes = custody.read_subtitle(&path, TRACK_BYTES).await?;
        match bytes {
            Some(bytes) => {
                validate_text(&bytes)?;
                Ok(Some((bytes, complete)))
            }
            None => Ok(None),
        }
    }
    pub(super) async fn window_seconds(manager: &TranscodeManager) -> i64 {
        let value = manager
            .store
            .get_setting(plurx_core::store::keys::SUBTITLE_WINDOW_SECS)
            .await
            .ok()
            .flatten();
        plurx_core::store::bounded_subtitle_window_seconds(value.as_deref())
    }
    pub(super) async fn window(
        &self,
        manager: &TranscodeManager,
        index: u16,
        demand: i64,
        span: i64,
        custody: &SourceResourceReadCustody,
        deadline: Instant,
    ) -> Result<Vec<u8>, String> {
        if !self.has_track(index) {
            return Err("Source native track is unsupported".into());
        }
        let _flight = tokio::select! {
            guard = self.flight.lock() => guard,
            () = custody.cancelled() => return Err("Source subtitle demand retired".into()),
            () = tokio::time::sleep_until(deadline.into()) => return Err("Source subtitle demand expired".into()),
        };
        let anchor = crate::subtitles::window_anchor_seconds(demand, span);
        let path = self.window_path(index, demand, span, custody.source()?.object_version());
        let revision = Self::revision(&path)?;
        if let Some((bytes, _)) = self.cached(index, &revision, custody).await? {
            self.remember(revision, bytes.clone());
            return Ok(bytes);
        }
        let end = anchor
            .saturating_add(span)
            .saturating_add(crate::subtitles::WINDOW_SLACK_SECONDS);
        // Local sidecars carry only file ID/size/mtime. They cannot establish
        // which held physical object supplied their cues, so a Source never
        // adopts them, even when they are warm and small.
        let bytes = {
            let _permit = tokio::select! {
                result = manager.admit_source_copy(deadline) => result.map_err(|error| format!("Source subtitle admission: {error:?}"))?,
                () = custody.cancelled() => return Err("Source subtitle demand retired".into()),
            };
            let request = manager.foreground_source_request(
                &self.file,
                &self.assignment.binding().incarnation_id().to_string(),
                self.assignment.binding().incarnation_id(),
                plurx_core::store::background_jobs_resources::ForegroundSourcePolicy::Rolling,
            );
            let _charge = match request {
                Some(request) => Some(request.register("source-caption-window").await?),
                None => None,
            };
            self.extract_window(
                index,
                anchor,
                end,
                custody,
                deadline,
                &manager.source_workers.native_hooks,
            )
            .await?
        };
        validate_text(&bytes)?;
        custody.source()?;
        if custody.is_cancelled() || Instant::now() >= deadline {
            return Err("Source subtitle demand expired".into());
        }
        custody.publish_subtitle(&path, bytes.clone()).await?;
        self.remember(revision, bytes.clone());
        Ok(bytes)
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn segment(
        &self,
        manager: &TranscodeManager,
        index: u16,
        video: &[u8],
        sequence: u64,
        origin: f64,
        custody: &SourceResourceReadCustody,
        deadline: Instant,
    ) -> Result<Option<Vec<u8>>, String> {
        let Some((start, end)) = crate::http::hls::source_native_segment_window(video, sequence)
        else {
            return Ok(None);
        };
        let span = Self::window_seconds(manager).await;
        let bytes = self
            .window(
                manager,
                index,
                (origin + start).max(0.0) as i64,
                span,
                custody,
                deadline,
            )
            .await?;
        let out = crate::http::hls::source_native_slice(&bytes, origin, start, end);
        if let Some(bytes) = out.as_ref() {
            validate_text(bytes)?;
        }
        Ok(out)
    }
    async fn extract_window(
        &self,
        index: u16,
        anchor: i64,
        end: i64,
        custody: &SourceResourceReadCustody,
        deadline: Instant,
        hooks: &SourceProbeHookOwner,
    ) -> Result<Vec<u8>, String> {
        let source = crate::fragment_index_cluster::open_source_playback_fence(
            &self.file,
            Some(custody.source()?.object_version()),
        )
        .await?;
        let result = async {
            let input = held_input(&source.handle)?;
            if matches!(
                self.file.container.as_deref(),
                Some("mkv" | "matroska" | "webm")
            ) {
                // A timeline query is real Source work too; never use the global
                // reporter cache or Local's anonymous probe for a cold Source.
                let mut probe = tokio::process::Command::new(crate::ffmpeg::ffprobe_bin());
                crate::ffmpeg::inherit_file_descriptors(&mut probe, &[(&source.handle, 3)]);
                probe
                    .args([
                        "-v",
                        "error",
                        "-show_entries",
                        "format=start_time",
                        "-of",
                        "default=noprint_wrappers=1:nokey=1",
                    ])
                    .arg(&input);
                let output = super::source_preparation::run_resource_child(
                    &self.file, custody, deadline, probe, 1024, hooks,
                )
                .await?;
                let start = std::str::from_utf8(&output.stdout)
                    .ok()
                    .and_then(crate::subtitles::parse_source_start_seconds)
                    .ok_or("Source subtitle timeline is invalid")?;
                if let Some(bytes) = self
                    .sparse_window(index, anchor, end, start, custody, deadline, hooks)
                    .await?
                {
                    return Ok(bytes);
                }
            }
            let indexed = crate::subtitle_ranges::indexed_text(&self.file, i64::from(index));
            let command = crate::subtitles::held_window_command(
                &source.handle,
                &input,
                i64::from(index),
                anchor,
                end,
                indexed,
            );
            let output = super::source_preparation::run_resource_child(
                &self.file,
                custody,
                deadline,
                command,
                TRACK_BYTES,
                hooks,
            )
            .await?;
            if !output.stderr.is_empty() {
                return Err("Source subtitle decoder reported an error".into());
            }
            let bytes = if indexed {
                crate::subtitle_ranges::filter_absolute_window(&output.stdout, anchor, end)?
            } else {
                crate::subtitles::normalize_window_cues(&output.stdout, anchor)
            };
            validate_text(&bytes)?;
            Ok(bytes)
        }
        .await;
        super::source_preparation::close_source_before_settlement(source, |closed| {
            hooks.record_parent_closed(closed)
        });
        result
    }
    #[allow(clippy::too_many_arguments)]
    async fn sparse_window(
        &self,
        index: u16,
        anchor: i64,
        end: i64,
        start: f64,
        custody: &SourceResourceReadCustody,
        deadline: Instant,
        hooks: &SourceProbeHookOwner,
    ) -> Result<Option<Vec<u8>>, String> {
        let authority = custody.authorize_work().await?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let owned_cancel = cancel.clone();
        let owned_custody = custody.clone();
        let codecs: Vec<String> = self
            .file
            .subtitle_streams
            .iter()
            .map(|track| track.codec.clone())
            .collect();
        // The blocking parser and its scoped workers retain the exact Source
        // guard. Cancellation is signalled, followed by actual job completion.
        let mut job = tokio::task::spawn_blocking(move || {
            authority.validate_before_spawn()?;
            owned_custody.source()?;
            let reader = SourceCaptionReader(&owned_custody);
            let index_data =
                crate::mkv_sparse::read_index_cancellable(&reader, &owned_cancel, deadline)
                    .map_err(|error| error.to_string())?;
            let Some(index_data) = index_data else {
                return Ok(None);
            };
            if index_data.tracks.len() != codecs.len()
                || !index_data
                    .tracks
                    .iter()
                    .zip(&codecs)
                    .all(|(track, codec)| crate::mkv_sparse::same_codec(&track.codec, codec))
            {
                return Ok(None);
            }
            let offset = (start * 1e9).round() as i128;
            let span = Some((
                i128::from(anchor.saturating_sub(60).max(0)) * 1_000_000_000 + offset,
                i128::from(end) * 1_000_000_000 + offset,
            ));
            let subset = crate::mkv_sparse::subset_cancellable(
                &reader,
                &index_data,
                &[usize::from(index)],
                span,
                deadline,
                &owned_cancel,
            )
            .map_err(|error| error.to_string())?;
            owned_custody.source()?;
            Ok::<_, String>(Some(subset))
        });
        let result = tokio::select! {
            result = &mut job => result,
            () = custody.cancelled() => { cancel.cancel(); job.await },
            () = tokio::time::sleep_until(deadline.into()) => { cancel.cancel(); job.await },
        }
        .map_err(|error| format!("Source subtitle cue read: {error}"))?;
        if cancel.is_cancelled() || custody.is_cancelled() {
            return Err("Source subtitle cue read retired".into());
        }
        let subset = match result {
            Ok(Some(subset)) => subset,
            Ok(None) | Err(_) => return Ok(None),
        };
        if subset.blocks == 0 {
            return Ok(Some(b"WEBVTT\n\n".to_vec()));
        }
        let bytes = subset.bytes;
        let owned_custody = custody.clone();
        let directory = self.directory.clone();
        let input = tokio::task::spawn_blocking(move || {
            use std::io::{Seek, Write};
            owned_custody.source()?;
            std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
            let mut input = tempfile::tempfile_in(directory).map_err(|error| error.to_string())?;
            input.write_all(&bytes).map_err(|error| error.to_string())?;
            input.rewind().map_err(|error| error.to_string())?;
            Ok::<_, String>(input)
        })
        .await
        .map_err(|error| error.to_string())??;
        let path = held_input(&input)?;
        let mut args = vec!["-copyts".to_owned()];
        if start != 0.0 {
            args.extend(["-itsoffset".to_owned(), format!("{:.6}", -start)]);
        }
        let command =
            crate::subtitles::held_sparse_window_command(&input, &path, &args, i64::from(index));
        let output = match super::source_preparation::run_resource_child(
            &self.file,
            custody,
            deadline,
            command,
            TRACK_BYTES,
            hooks,
        )
        .await
        {
            Ok(output) if output.stderr.is_empty() => output,
            _ if !custody.is_cancelled() && Instant::now() < deadline => return Ok(None),
            _ => return Err("Source subtitle conversion retired".into()),
        };
        let bytes = crate::subtitle_ranges::filter_absolute_window(&output.stdout, anchor, end)?;
        validate_text(&bytes)?;
        Ok(Some(bytes))
    }
}
struct SourceCaptionReader<'a>(&'a SourceResourceReadCustody);
impl crate::mkv_sparse::ReadAt for SourceCaptionReader<'_> {
    fn read_at(&self, bytes: &mut [u8], offset: u64) -> std::io::Result<usize> {
        if self.0.is_cancelled() {
            return Err(std::io::Error::other("Source caption read retired"));
        }
        let source = self.0.source().map_err(std::io::Error::other)?;
        let count = crate::mkv_sparse::ReadAt::read_at(&source.handle, bytes, offset)?;
        self.0.source().map_err(std::io::Error::other)?;
        Ok(count)
    }
}
fn held_input(source: &std::fs::File) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        let _ = source;
        Ok(PathBuf::from("/dev/fd/3"))
    }
    #[cfg(windows)]
    {
        let path = crate::ffmpeg::windows_source_path(source)?;
        crate::ffmpeg::verify_windows_source_path(source, &path)?;
        Ok(path)
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
