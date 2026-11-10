//! Source native track facts and bounded, requested subtitle windows.
//! Physical work stays inside the actor's counted resource/control factory;
//! no generic warmer or subtitle flight can confer Source authority.
use super::{
    source_actor::{resource::SourceResourceReadCustody, SourceWorkerError},
    source_preparation::{SourceProbeHookOwner, SourceResourceChildFailure},
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
    unsupported: Mutex<BTreeSet<String>>,
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
            unsupported: Mutex::new(BTreeSet::new()),
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
            self.unsupported
                .lock()
                .expect("Source caption refusals")
                .contains(revision)
        }) {
            return (Cache::Unavailable, None);
        }
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
    ) -> Result<Option<(Vec<u8>, bool)>, SourceWorkerError> {
        if !self.has_track(index) {
            return Ok(None);
        }
        let object = custody
            .source()
            .map_err(|_| SourceWorkerError::Unavailable)?
            .object_version();
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
        if Self::revision(&path).map_err(|_| SourceWorkerError::Unsupported)? != revision {
            return Ok(None);
        }
        let complete = false;
        custody
            .source()
            .map_err(|_| SourceWorkerError::Unavailable)?;
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
                validate_window_text(&bytes).map_err(|_| SourceWorkerError::Unsupported)?;
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
    ) -> Result<Vec<u8>, SourceWorkerError> {
        let path = self.window_path(
            index,
            demand,
            span,
            custody
                .source()
                .map_err(|_| SourceWorkerError::Unavailable)?
                .object_version(),
        );
        let revision = Self::revision(&path).map_err(|_| SourceWorkerError::Unsupported)?;
        if self
            .unsupported
            .lock()
            .expect("Source caption refusals")
            .contains(&revision)
        {
            return Err(SourceWorkerError::Unsupported);
        }
        let result = self
            .window_inner(manager, index, demand, span, custody, deadline)
            .await;
        if matches!(result, Err(SourceWorkerError::Unsupported)) {
            let mut unsupported = self.unsupported.lock().expect("Source caption refusals");
            if unsupported.len() >= HELD_WINDOWS {
                unsupported.pop_first();
            }
            unsupported.insert(revision);
        }
        result
    }
    async fn window_inner(
        &self,
        manager: &TranscodeManager,
        index: u16,
        demand: i64,
        span: i64,
        custody: &SourceResourceReadCustody,
        deadline: Instant,
    ) -> Result<Vec<u8>, SourceWorkerError> {
        if !self.has_track(index) {
            return Err(SourceWorkerError::Unsupported);
        }
        let _flight = tokio::select! {
            guard = self.flight.lock() => guard,
            () = custody.cancelled() => return Err(SourceWorkerError::Unavailable),
            () = tokio::time::sleep_until(deadline.into()) => return Err(SourceWorkerError::Deadline),
        };
        let anchor = crate::subtitles::window_anchor_seconds(demand, span);
        let path = self.window_path(
            index,
            demand,
            span,
            custody
                .source()
                .map_err(|_| SourceWorkerError::Unavailable)?
                .object_version(),
        );
        let revision = Self::revision(&path).map_err(|_| SourceWorkerError::Unsupported)?;
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
                result = manager.admit_source_copy(deadline) => result?,
                () = custody.cancelled() => return Err(SourceWorkerError::Unavailable),
                () = tokio::time::sleep_until(deadline.into()) => return Err(SourceWorkerError::Deadline),
            };
            let request = manager.foreground_source_request(
                &self.file,
                &self.assignment.binding().incarnation_id().to_string(),
                self.assignment.binding().incarnation_id(),
                plurx_core::store::background_jobs_resources::ForegroundSourcePolicy::Rolling,
            );
            let _charge = match request {
                Some(request) => Some(
                    request
                        .register("source-caption-window")
                        .await
                        .map_err(|_| SourceWorkerError::Unavailable)?,
                ),
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
        validate_window_text(&bytes).map_err(|_| SourceWorkerError::Unsupported)?;
        custody
            .source()
            .map_err(|_| SourceWorkerError::Unavailable)?;
        if custody.is_cancelled() {
            return Err(SourceWorkerError::Unavailable);
        }
        if Instant::now() >= deadline {
            return Err(SourceWorkerError::Deadline);
        }
        custody
            .publish_subtitle(&path, bytes.clone(), deadline)
            .await?;
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
    ) -> Result<Option<Vec<u8>>, SourceWorkerError> {
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
            validate_text(bytes).map_err(|_| SourceWorkerError::Unsupported)?;
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
    ) -> Result<Vec<u8>, SourceWorkerError> {
        let source = crate::fragment_index_cluster::open_source_playback_fence(
            &self.file,
            Some(
                custody
                    .source()
                    .map_err(|_| SourceWorkerError::Unavailable)?
                    .object_version(),
            ),
        )
        .await
        .map_err(|_| SourceWorkerError::Unavailable)?;
        let result = async {
            let input = held_input(&source.handle).map_err(|_| SourceWorkerError::Unavailable)?;
            if matches!(
                self.file.container.as_deref(),
                Some("mkv" | "matroska" | "webm")
            ) {
                // Only a proven, requested cue index needs timeline evidence.
                // An optional reporter/conversion refusal leaves demux usable;
                // actual authority, deadline and output-bound failures do not.
                if let Some(index_data) = self.sparse_index(index, custody, deadline).await? {
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
                    let start = optional_source_timeline(
                        super::source_preparation::run_resource_child(
                            &self.file, custody, deadline, probe, 1024, hooks,
                        )
                        .await,
                    )?;
                    if let Some(start) = start {
                        if let Some(bytes) = self
                            .sparse_window(
                                index, index_data, anchor, end, start, custody, deadline, hooks,
                            )
                            .await?
                        {
                            return Ok(bytes);
                        }
                    }
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
            .await
            .map_err(SourceResourceChildFailure::worker_error)?;
            if !output.stderr.is_empty() {
                return Err(SourceWorkerError::Unsupported);
            }
            let bytes = normalize_source_window(&output.stdout, anchor, end, indexed)?;
            Ok(bytes)
        }
        .await;
        super::source_preparation::close_source_before_settlement(source, |closed| {
            hooks.record_parent_closed(closed)
        });
        result
    }
    async fn sparse_index(
        &self,
        index: u16,
        custody: &SourceResourceReadCustody,
        deadline: Instant,
    ) -> Result<Option<crate::mkv_sparse::CueIndex>, SourceWorkerError> {
        let codecs: Vec<String> = self
            .file
            .subtitle_streams
            .iter()
            .map(|track| track.codec.clone())
            .collect();
        source_cue_read(custody, deadline, move |reader, cancel| {
            let Some(index_data) =
                crate::mkv_sparse::read_index_cancellable(reader, cancel, deadline)
                    .map_err(|error| SourceSparseFailure::from_read(error, deadline))?
            else {
                return Ok(None);
            };
            if index_data.tracks.len() != codecs.len()
                || !index_data
                    .tracks
                    .iter()
                    .zip(&codecs)
                    .all(|(track, codec)| crate::mkv_sparse::same_codec(&track.codec, codec))
                || !index_data
                    .tracks
                    .get(usize::from(index))
                    .is_some_and(crate::mkv_sparse::SubtitleTrack::indexed)
            {
                return Ok(None);
            }
            Ok(Some(index_data))
        })
        .await
    }
    pub(super) async fn stage_sparse_input(
        &self,
        bytes: Vec<u8>,
        custody: &SourceResourceReadCustody,
        deadline: Instant,
    ) -> Result<SourceSparseInput, SourceWorkerError> {
        let reservation_path = self.directory.join(format!(
            ".tmp-source-caption-subset-{}.mkv",
            uuid::Uuid::new_v4()
        ));
        let reservation = custody
            .reserve_subtitle(&reservation_path, bytes.len() as u64, deadline)
            .await?;
        let custody = custody.clone();
        let directory = self.directory.clone();
        tokio::task::spawn_blocking(move || {
            use super::source_actor::resource::cache_io_error;
            use std::io::{Seek, Write};
            custody
                .source()
                .map_err(|_| SourceWorkerError::Unavailable)?;
            std::fs::create_dir_all(&directory).map_err(cache_io_error)?;
            #[cfg(unix)]
            let mut input = tempfile::tempfile_in(&directory).map_err(cache_io_error)?;
            #[cfg(windows)]
            let mut named = tempfile::Builder::new()
                .prefix(".tmp-source-caption-subset-")
                .tempfile_in(&directory)
                .map_err(cache_io_error)?;
            #[cfg(windows)]
            let pin = crate::playback_cache::pin(named.path()).map_err(cache_io_error)?;
            #[cfg(windows)]
            {
                named.write_all(&bytes).map_err(cache_io_error)?;
                named.flush().map_err(cache_io_error)?;
            }
            #[cfg(windows)]
            let mut input = named.reopen().map_err(cache_io_error)?;
            #[cfg(unix)]
            input.write_all(&bytes).map_err(cache_io_error)?;
            input.rewind().map_err(cache_io_error)?;
            custody
                .source()
                .map_err(|_| SourceWorkerError::Unavailable)?;
            Ok(SourceSparseInput {
                handle: input,
                #[cfg(windows)]
                _named: named,
                #[cfg(windows)]
                _pin: pin,
                _reservation: reservation,
            })
        })
        .await
        .map_err(|_| SourceWorkerError::Unavailable)?
    }
    #[allow(clippy::too_many_arguments)]
    async fn sparse_window(
        &self,
        index: u16,
        index_data: crate::mkv_sparse::CueIndex,
        anchor: i64,
        end: i64,
        start: f64,
        custody: &SourceResourceReadCustody,
        deadline: Instant,
        hooks: &SourceProbeHookOwner,
    ) -> Result<Option<Vec<u8>>, SourceWorkerError> {
        let subset = source_cue_read(custody, deadline, move |reader, cancel| {
            let offset = (start * 1e9).round() as i128;
            let span = Some((
                i128::from(anchor.saturating_sub(60).max(0)) * 1_000_000_000 + offset,
                i128::from(end) * 1_000_000_000 + offset,
            ));
            crate::mkv_sparse::subset_cancellable(
                reader,
                &index_data,
                &[usize::from(index)],
                span,
                deadline,
                cancel,
            )
            .map(Some)
            .map_err(|error| SourceSparseFailure::from_read(error, deadline))
        })
        .await?;
        let Some(subset) = subset else {
            return Ok(None);
        };
        if subset.blocks == 0 {
            return Ok(Some(b"WEBVTT\n\n".to_vec()));
        }
        // Exact temporary bytes are reserved before write. Its reservation is
        // the last field dropped, after both readable and Windows named handles.
        let input = self
            .stage_sparse_input(subset.bytes, custody, deadline)
            .await?;
        let path = held_input(&input.handle).map_err(|_| SourceWorkerError::Unavailable)?;
        let mut args = vec!["-copyts".to_owned()];
        if start != 0.0 {
            args.extend(["-itsoffset".to_owned(), format!("{:.6}", -start)]);
        }
        let command = crate::subtitles::held_sparse_window_command(
            &input.handle,
            &path,
            &args,
            i64::from(index),
        );
        let output = super::source_preparation::run_resource_child(
            &self.file,
            custody,
            deadline,
            command,
            TRACK_BYTES,
            hooks,
        )
        .await;
        // The child and pipes have actually settled before the file closes and
        // its disk allowance returns, including cancellation and error paths.
        drop(input);
        let output = match output {
            Ok(output) if output.stderr.is_empty() => output,
            Ok(_) | Err(SourceResourceChildFailure::Conversion)
                if !custody.is_cancelled() && Instant::now() < deadline =>
            {
                return Ok(None)
            }
            Err(SourceResourceChildFailure::Work(error)) => return Err(error),
            _ => {
                return Err(if custody.is_cancelled() {
                    SourceWorkerError::Unavailable
                } else {
                    SourceWorkerError::Deadline
                })
            }
        };
        validate_window_text(&output.stdout).map_err(|_| SourceWorkerError::Unsupported)?;
        let bytes = crate::subtitle_ranges::filter_absolute_window(&output.stdout, anchor, end)
            .map_err(|_| SourceWorkerError::Unsupported)?;
        validate_window_text(&bytes).map_err(|_| SourceWorkerError::Unsupported)?;
        Ok(Some(bytes))
    }
}
pub(super) struct SourceSparseInput {
    handle: std::fs::File,
    #[cfg(windows)]
    _named: tempfile::NamedTempFile,
    #[cfg(windows)]
    _pin: Option<crate::playback_cache::CachePin>,
    _reservation: Option<crate::playback_cache::CacheReservation>,
}
fn optional_source_timeline(
    result: Result<crate::ffmpeg::BoundedOutput, SourceResourceChildFailure>,
) -> Result<Option<f64>, SourceWorkerError> {
    match result {
        Ok(output) if output.stderr.is_empty() => Ok(std::str::from_utf8(&output.stdout)
            .ok()
            .and_then(crate::subtitles::parse_source_start_seconds)),
        Ok(_) | Err(SourceResourceChildFailure::Conversion) => Ok(None),
        Err(SourceResourceChildFailure::Work(error)) => Err(error),
    }
}
async fn source_cue_read<T, F>(
    custody: &SourceResourceReadCustody,
    deadline: Instant,
    read: F,
) -> Result<Option<T>, SourceWorkerError>
where
    T: Send + 'static,
    F: FnOnce(
            &dyn crate::mkv_sparse::ReadAt,
            &tokio_util::sync::CancellationToken,
        ) -> Result<Option<T>, SourceSparseFailure>
        + Send
        + 'static,
{
    let authority = custody
        .authorize_work()
        .await
        .map_err(|_| SourceWorkerError::Unavailable)?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let owned_cancel = cancel.clone();
    let owned = custody.clone();
    let mut job = tokio::task::spawn_blocking(move || {
        authority
            .validate_before_spawn()
            .map_err(|_| SourceSparseFailure::Work(SourceWorkerError::Unavailable))?;
        owned
            .source()
            .map_err(|_| SourceSparseFailure::Work(SourceWorkerError::Unavailable))?;
        let result = read(&SourceCaptionReader(&owned), &owned_cancel)?;
        owned
            .source()
            .map_err(|_| SourceSparseFailure::Work(SourceWorkerError::Unavailable))?;
        Ok::<_, SourceSparseFailure>(result)
    });
    let result = tokio::select! {
        result = &mut job => result,
        () = custody.cancelled() => { cancel.cancel(); job.await },
        () = tokio::time::sleep_until(deadline.into()) => { cancel.cancel(); job.await },
    }
    .map_err(|_| SourceWorkerError::Unavailable)?;
    if custody.is_cancelled() {
        return Err(SourceWorkerError::Unavailable);
    }
    if cancel.is_cancelled() {
        return Err(SourceWorkerError::Deadline);
    }
    match result {
        Ok(value) => Ok(value),
        Err(SourceSparseFailure::Fallback) => Ok(None),
        Err(SourceSparseFailure::Work(error)) => Err(error),
    }
}

enum SourceSparseFailure {
    Work(SourceWorkerError),
    Fallback,
}
impl SourceSparseFailure {
    fn from_read(error: crate::mkv_sparse::SparseError, deadline: Instant) -> Self {
        match error {
            crate::mkv_sparse::SparseError::Cancelled => {
                Self::Work(if Instant::now() >= deadline {
                    SourceWorkerError::Deadline
                } else {
                    SourceWorkerError::Unavailable
                })
            }
            crate::mkv_sparse::SparseError::NotIndexed(_)
            | crate::mkv_sparse::SparseError::Malformed(_)
            | crate::mkv_sparse::SparseError::Io(_) => Self::Fallback,
        }
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
    if header_lines.next() != Some("WEBVTT") {
        return Err("Source native header unsupported".into());
    }
    if let Some(map) = header_lines.next() {
        if header_lines.next().is_some() || !valid_timestamp_map(map) {
            return Err("Source native timestamp map unsupported".into());
        }
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
fn normalize_source_window(
    raw: &[u8],
    anchor: i64,
    end: i64,
    indexed: bool,
) -> Result<Vec<u8>, SourceWorkerError> {
    validate_window_text(raw).map_err(|_| SourceWorkerError::Unsupported)?;
    let bytes = if indexed {
        crate::subtitle_ranges::filter_absolute_window(raw, anchor, end)
            .map_err(|_| SourceWorkerError::Unsupported)?
    } else {
        crate::subtitles::normalize_window_cues(raw, anchor)
    };
    validate_window_text(&bytes).map_err(|_| SourceWorkerError::Unsupported)?;
    Ok(bytes)
}
fn validate_window_text(bytes: &[u8]) -> Result<(), String> {
    validate_text(bytes)?;
    let text =
        std::str::from_utf8(bytes).map_err(|_| "Source native text is not UTF-8".to_owned())?;
    if text
        .lines()
        .take_while(|line| !line.is_empty())
        .any(|line| line.starts_with("X-TIMESTAMP-MAP="))
    {
        return Err("Source unsliced window cannot carry a timestamp map".into());
    }
    Ok(())
}
fn valid_timestamp_map(line: &str) -> bool {
    let Some(raw) = line.strip_prefix("X-TIMESTAMP-MAP=") else {
        return false;
    };
    let Some((first, second)) = raw.split_once(',') else {
        return false;
    };
    let (mpeg, local) = match (
        first.strip_prefix("MPEGTS:"),
        second.strip_prefix("LOCAL:"),
        first.strip_prefix("LOCAL:"),
        second.strip_prefix("MPEGTS:"),
    ) {
        (Some(mpeg), Some(local), _, _) | (_, _, Some(local), Some(mpeg)) => (mpeg, local),
        _ => return false,
    };
    !mpeg.is_empty()
        && mpeg.bytes().all(|byte| byte.is_ascii_digit())
        && mpeg.parse::<u64>().is_ok_and(|value| value < (1_u64 << 33))
        && valid_local_timestamp(local)
}
fn valid_local_timestamp(raw: &str) -> bool {
    let parts: Vec<_> = raw.split(':').collect();
    let [hours, minutes, seconds] = parts.as_slice() else {
        return false;
    };
    let Some((seconds, millis)) = seconds.split_once('.') else {
        return false;
    };
    hours.len() >= 2
        && hours.bytes().all(|byte| byte.is_ascii_digit())
        && minutes.len() == 2
        && minutes.bytes().all(|byte| byte.is_ascii_digit())
        && seconds.len() == 2
        && seconds.bytes().all(|byte| byte.is_ascii_digit())
        && millis.len() == 3
        && millis.bytes().all(|byte| byte.is_ascii_digit())
        && timestamp(raw).is_ok()
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
    fn source_raw_positive_anchor_rejects_lossy_utf8_and_invalid_timestamp_maps() {
        let invalid = b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nbad \xff\n";
        assert_eq!(
            normalize_source_window(invalid, 600, 860, false),
            Err(SourceWorkerError::Unsupported)
        );
        let valid = b"WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:8589934591,LOCAL:00:00:00.000\n\n";
        assert!(validate_text(valid).is_ok());
        assert!(validate_window_text(valid).is_err());
        for map in [
            "X-TIMESTAMP-MAP=garbage", "X-TIMESTAMP-MAP=MPEGTS:8589934592,LOCAL:00:00:00.000",
            "X-TIMESTAMP-MAP=MPEGTS:0,LOCAL:00:00:NaN", "X-TIMESTAMP-MAP=MPEGTS:0,LOCAL:00:00:0e0",
            "X-TIMESTAMP-MAP=MPEGTS:0,LOCAL:00:00:00.000\nX-TIMESTAMP-MAP=MPEGTS:0,LOCAL:00:00:00.000",
        ] { assert!(validate_text(format!("WEBVTT\n{map}\n\n").as_bytes()).is_err(), "{map}"); }
    }
    #[test]
    fn source_optional_index_timeline_falls_back_without_hiding_typed_work_failures() {
        assert_eq!(
            optional_source_timeline(Err(SourceResourceChildFailure::Conversion)),
            Ok(None)
        );
        assert_eq!(
            optional_source_timeline(Ok(crate::ffmpeg::BoundedOutput {
                stdout: b"not-a-clock".to_vec(),
                stderr: Vec::new()
            })),
            Ok(None)
        );
        for error in [
            SourceWorkerError::Unsupported,
            SourceWorkerError::Unavailable,
            SourceWorkerError::Deadline,
            SourceWorkerError::Capacity,
        ] {
            assert_eq!(
                optional_source_timeline(Err(SourceResourceChildFailure::Work(error))),
                Err(error)
            );
        }
    }
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
