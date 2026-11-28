use super::*;
use plurx_core::fmp4::track_projection::{CopyTrackProjection, ProjectedVodWindow};
use plurx_core::transcode::split_master::{
    SplitAudio, SplitBandwidth, SplitBudget, SplitPresentation, SplitVideo, VideoRange,
};
use plurx_core::transcode::{OutputGrade, VodCopyLayout, VodSoundtrackIdentity};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

// An encoded segment is already bounded by the VOD segment plan. Bound the
// additional scratch opened for its children, including tracked init buffers.
const MAX_WINDOW_BYTES: u64 = 64 * 1024 * 1024;
const MAX_INIT_BYTES: u64 = 1024 * 1024;
const MIB: u64 = 1024 * 1024;
// A permit covers both projected children until the last file reader closes.
static PROJECTION_SCRATCH: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(256)));
static PROJECTION_WORKERS: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));

pub(super) struct EncodedVodProjection {
    layout: VodCopyLayout,
    projector: Arc<CopyTrackProjection>,
    master: SplitPresentation,
    peak_bps: u64,
    // Audio and video requests can share one projected window. The files
    // are linked only through this session and reclaimed when it advances.
    recent: Arc<RecentProjection>,
}

type RecentProjection = tokio::sync::Mutex<Option<ProjectedFiles>>;
static RECENT_PROJECTIONS: StdMutex<Vec<std::sync::Weak<RecentProjection>>> =
    StdMutex::new(Vec::new());

fn reserve_projection_scratch(mib: u32) -> Option<tokio::sync::OwnedSemaphorePermit> {
    let reserve = || {
        Arc::clone(&PROJECTION_SCRATCH)
            .try_acquire_many_owned(mib)
            .ok()
    };
    if let Some(permit) = reserve() {
        return Some(permit);
    }
    let mut projections = RECENT_PROJECTIONS
        .lock()
        .expect("projection cache registry");
    projections.retain(|cell| cell.strong_count() != 0);
    for cell in projections.iter().filter_map(std::sync::Weak::upgrade) {
        if let Ok(mut window) = cell.try_lock() {
            window.take();
            if let Some(permit) = reserve() {
                return Some(permit);
            }
        }
    }
    None
}

struct ProjectedFiles {
    index: u32,
    input_etag: String,
    video: ProjectedObject,
    audio: ProjectedObject,
    scratch: Arc<tokio::sync::OwnedSemaphorePermit>,
}

struct ProjectedObject {
    file: tempfile::TempPath,
    etag: String,
    len: u64,
}

impl ProjectedObject {
    fn write(directory: &std::path::Path, bytes: &[u8]) -> Result<Self, VodError> {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new_in(directory).map_err(projection_io)?;
        file.write_all(bytes).map_err(projection_io)?;
        Ok(Self {
            file: file.into_temp_path(),
            etag: hex::encode(Sha256::digest(bytes)),
            len: bytes.len() as u64,
        })
    }

    async fn open(
        &self,
        delivery: Arc<crate::meter::Meter>,
        observed_media_duration_ms: Option<u32>,
        scratch: Arc<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<SegmentReady, VodError> {
        Ok(SegmentReady {
            file: tokio::fs::File::open(&self.file)
                .await
                .map_err(projection_io)?,
            len: self.len,
            etag: self.etag.clone(),
            observed_media_duration_ms,
            delivery,
            retained_lease: None,
            cache_pin: None,
            projection_scratch: Some(scratch),
        })
    }
}

fn projection_io(error: std::io::Error) -> VodError {
    VodError::ProducerFailed(format!("encoded split window: {error}"))
}

fn invalid(reason: impl Into<String>) -> VodError {
    VodError::ProducerFailed(reason.into())
}

async fn read_projection_input(ready: SegmentReady, limit: u64) -> Result<Vec<u8>, VodError> {
    if ready.len == 0 || ready.len > limit {
        return Err(invalid(
            "encoded split object exceeds its projection budget",
        ));
    }
    let mut bytes = Vec::with_capacity(ready.len as usize);
    ready
        .file
        .take(ready.len + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(projection_io)?;
    if bytes.len() as u64 != ready.len {
        return Err(invalid("encoded object changed during projection"));
    }
    Ok(bytes)
}

impl EncodedVodProjection {
    fn new(rendition: &Rendition, init: Init, source_copy: bool) -> Result<Self, VodError> {
        use plurx_core::fmp4::{self, VideoCodec};
        let encoding = rendition.recipe.encoding.as_ref();
        if encoding.is_none() && !source_copy {
            return Err(invalid("encoded split has no executable recipe"));
        }
        let audio_index = rendition
            .recipe
            .audio_index
            .or_else(|| {
                rendition
                    .recipe
                    .file
                    .audio_streams
                    .first()
                    .map(|stream| stream.index)
            })
            .ok_or_else(|| invalid("encoded split has no selected soundtrack"))?;
        let stream = rendition
            .recipe
            .file
            .audio_streams
            .iter()
            .find(|stream| stream.index == audio_index)
            .ok_or_else(|| invalid("encoded split soundtrack is missing from the held source"))?;
        if rendition.plan.entries.is_empty()
            || rendition
                .plan
                .entries
                .iter()
                .any(|entry| entry.kind != plurx_core::segplan::PlanEntryKind::Video)
        {
            return Err(invalid(
                "encoded split needs a finite paired audio and video plan",
            ));
        }
        let projector =
            CopyTrackProjection::new(init).map_err(|error| invalid(error.to_string()))?;
        let video_init = projector.video_init();
        let audio_init = projector.audio_init();
        let video = fmp4::copy_video_sample_entry_facts(video_init)
            .map_err(|error| invalid(error.to_string()))?;
        let audio = fmp4::aac_lc_sample_entry_facts(audio_init)
            .map_err(|error| invalid(error.to_string()))?;
        let sample_entry = std::str::from_utf8(&video.sample_entry)
            .map_err(|_| invalid("encoded video sample entry is invalid"))?;
        let codec = match video.codec {
            VideoCodec::H264 => {
                fmp4::avc_rfc6381_codec(video_init).map_err(|error| invalid(error.to_string()))?
            }
            VideoCodec::Hevc => {
                crate::http::hls::hevc_codec_from_init(&video_init.bytes, sample_entry)
            }
        }
        .ok_or_else(|| invalid("encoded video init cannot prove its codec"))?;
        let signaled_range = video
            .color
            .and_then(|(_, transfer, _, _)| VideoRange::from_transfer(transfer));
        let range = match encoding.map_or(OutputGrade::Sdr, |encoding| {
            encoding.delivered_output_grade()
        }) {
            OutputGrade::Sdr
                if signaled_range == Some(VideoRange::Sdr)
                    || signaled_range.is_none()
                        && video.codec == VideoCodec::H264
                        && video.bit_depth == 8 =>
            {
                VideoRange::Sdr
            }
            OutputGrade::Hdr10
                if video.codec == VideoCodec::Hevc
                    && video.bit_depth == 10
                    && video.color == Some((9, 16, 9, false)) =>
            {
                VideoRange::Pq
            }
            _ => {
                return Err(invalid(
                    "encoded video init does not prove the requested range",
                ))
            }
        };
        let dolby =
            fmp4::dolby_vision_record(video_init).map_err(|error| invalid(error.to_string()))?;
        let preserves_processed_dv =
            encoding.is_some_and(|encoding| encoding.preserves_processed_dv());
        let supplemental_codecs = match dolby {
            Some(record)
                if preserves_processed_dv
                    && range == VideoRange::Pq
                    && record.profile == 8
                    && record.bl_signal_compatibility_id == 1
                    && record.bl_present
                    && record.rpu_present
                    && !record.el_present
                    && record.level != 0 =>
            {
                Some(format!(
                    "{}.08.{:02}/db1p",
                    if sample_entry == "hev1" {
                        "dvhe"
                    } else {
                        "dvh1"
                    },
                    record.level
                ))
            }
            None if !preserves_processed_dv => None,
            _ => {
                return Err(invalid(
                    "encoded video init does not prove its Dolby Vision contract",
                ))
            }
        };
        let video_init_id = hex::encode(Sha256::digest(&video_init.bytes));
        let audio_init_id = hex::encode(Sha256::digest(&audio_init.bytes));
        // Processed trials can use a random physical directory. Bind the
        // child URLs to the canonical recipe and its verified init instead.
        let file = &rendition.recipe.file;
        let duration = file
            .duration_ms
            .ok_or_else(|| invalid("encoded title has no duration"))?;
        let producer_id = if rendition.key.starts_with("source-") {
            // Source recipes are already bound to the catalogue revision.
            // Keep that namespace in the projected identity and encode it
            // using the rendition contract's digest format.
            hex::encode(Sha256::digest(rendition.key.as_bytes()))
        } else {
            let encoding = encoding.ok_or_else(|| invalid("encoded VOD identity is missing"))?;
            let identity = encoding.identity(file, duration as f64 / 1000.0);
            rendition_key(&rendition.recipe, &identity)
        };
        let child_id = |role: &[u8], init_id: &str| {
            let mut sha = Sha256::new();
            sha.update(b"plurx-vod-encoded-projection-v1\0");
            sha.update(producer_id.as_bytes());
            sha.update(role);
            sha.update(init_id.as_bytes());
            hex::encode(sha.finalize())
        };
        let video_id = child_id(b"video", &video_init_id);
        let audio_id = child_id(b"audio", &audio_init_id);
        let peak = match encoding {
            Some(encoding) => encoding.projected_vod_peak_bps(&rendition.plan),
            None => rendition
                .plan
                .entries
                .iter()
                .map(|entry| {
                    let seconds = entry.seconds(rendition.plan.timescale);
                    (entry.est_bytes as f64 * 16.0 / seconds).ceil() as u64
                })
                .max(),
        }
        .filter(|peak| *peak > 0 && *peak < u64::MAX)
        .ok_or_else(|| invalid("paired VOD producer cannot prove its delivery budget"))?;
        let master = SplitPresentation {
            variants: vec![SplitVideo {
                rendition_id: video_id.clone(),
                codec,
                supplemental_codecs,
                width: Some(u32::from(video.width)),
                height: Some(u32::from(video.height)),
                frame_rate: encoding.map(|encoding| {
                    f64::from(encoding.grid.numerator) / f64::from(encoding.grid.denominator)
                }),
                video_range: range,
                budget: None,
            }],
            audio: vec![SplitAudio {
                rendition_id: audio_id.clone(),
                source_index: audio_index,
                title: stream.title.clone(),
                language: stream.language.clone(),
                default: true,
                channels: Some(audio.channels),
                codec: audio.codec.to_owned(),
                budget: None,
            }],
            bandwidth: SplitBandwidth::Combined(SplitBudget {
                peak_bps: peak,
                average_bps: None,
            }),
            subtitles: String::new(),
            independent_segments: false,
        };
        master
            .master_text()
            .map_err(|error| invalid(error.to_string()))?;
        let layout = VodCopyLayout::EncodedSplitV1 {
            producer_id,
            video_rendition_id: video_id,
            video_init_id,
            soundtrack: VodSoundtrackIdentity {
                source_index: audio_index,
                rendition_id: audio_id,
                init_id: audio_init_id,
                source_copy: source_copy && !rendition.recipe.aac,
            },
        };
        if !layout.valid() {
            return Err(invalid("encoded projection identity is invalid"));
        }
        let recent = Arc::new(tokio::sync::Mutex::new(None));
        {
            let mut projections = RECENT_PROJECTIONS
                .lock()
                .expect("projection cache registry");
            projections.retain(|cell| cell.strong_count() != 0);
            projections.push(Arc::downgrade(&recent));
        }
        Ok(Self {
            layout,
            projector: Arc::new(projector),
            master,
            peak_bps: peak,
            recent,
        })
    }

    fn child(&self, role: &str, rendition: &str) -> Option<(&str, &Init)> {
        let VodCopyLayout::EncodedSplitV1 {
            video_rendition_id,
            video_init_id,
            soundtrack,
            ..
        } = &self.layout
        else {
            return None;
        };
        match role {
            "video" if rendition == video_rendition_id => {
                Some((video_init_id, self.projector.video_init()))
            }
            "audio" if rendition == soundtrack.rendition_id => {
                Some((&soundtrack.init_id, self.projector.audio_init()))
            }
            _ => None,
        }
    }

    async fn project_segment(
        &self,
        rendition: &Rendition,
        index: u32,
        source: SegmentReady,
        audio: bool,
        custody: Option<&crate::transcode::source_actor::resource::SourceResourceReadCustody>,
    ) -> Result<SegmentReady, VodError> {
        let mut recent = self.recent.lock().await;
        let delivery = Arc::clone(&source.delivery);
        let duration = source.observed_media_duration_ms;
        if !recent
            .as_ref()
            .is_some_and(|window| window.index == index && window.input_etag == source.etag)
        {
            // A replaced window no longer occupies this session's scratch.
            // Its open HTTP readers retain their own allowance.
            recent.take();
            let busy = || VodError::CapacityUnavailable {
                reason: "encoded split projection capacity is occupied".into(),
                retry_after: PENDING_RETRY_AFTER,
            };
            let worker = Arc::clone(&PROJECTION_WORKERS)
                .try_acquire_owned()
                .map_err(|_| busy())?;
            let output_limit = source
                .len
                .checked_mul(2)
                .filter(|_| source.len <= MAX_WINDOW_BYTES)
                .ok_or_else(|| invalid("encoded window exceeds its projection budget"))?;
            let mib = u32::try_from(output_limit.div_ceil(MIB)).map_err(|_| busy())?;
            let scratch = reserve_projection_scratch(mib).ok_or_else(busy)?;
            let etag = source.etag.clone();
            let bytes = read_projection_input(source, MAX_WINDOW_BYTES).await?;
            let projector = Arc::clone(&self.projector);
            let directory = rendition.dir.path().to_owned();
            let peak_bps = self.peak_bps;
            let custody = custody.cloned();
            let projected = tokio::task::spawn_blocking(move || {
                let _worker = worker;
                let _custody = custody;
                let ProjectedVodWindow { video, audio, .. } =
                    projector
                        .vod_window(&bytes, index)
                        .map_err(|error| invalid(error.to_string()))?;
                let total = (video.len() as u64).saturating_add(audio.len() as u64);
                if total > output_limit
                    || duration.is_none_or(|duration| {
                        u128::from(total) * 8_000 > u128::from(peak_bps) * u128::from(duration)
                    })
                {
                    return Err(invalid(
                        "encoded children exceed the reserved projection budget",
                    ));
                }
                Ok::<_, VodError>(ProjectedFiles {
                    index,
                    input_etag: etag,
                    video: ProjectedObject::write(&directory, &video)?,
                    audio: ProjectedObject::write(&directory, &audio)?,
                    scratch: Arc::new(scratch),
                })
            })
            .await
            .map_err(|error| invalid(error.to_string()))??;
            *recent = Some(projected);
        }
        let window = recent.as_ref().expect("published projection");
        let object = if audio { &window.audio } else { &window.video };
        object
            .open(delivery, duration, Arc::clone(&window.scratch))
            .await
    }
}

impl VodServe {
    async fn encoded_split_input(
        &self,
        session_id: &str,
        found: &ResolvedMediaReader,
        owner: &ResponseOwner,
        index: Option<u32>,
        budget: Duration,
        custody: Option<&crate::transcode::source_actor::resource::SourceResourceReadCustody>,
    ) -> Result<SegmentReady, VodError> {
        if let Some(artifact) = owner.retained_output.as_ref() {
            return artifact
                .open(
                    index,
                    &found.delivery,
                    &self.shared,
                    &found.rendition,
                    budget,
                )
                .await;
        }
        // The materializer can enter its watchdog and VOD admission while
        // the opening proof is still on the worker stack. Preserve the same
        // task and custody while moving that nested future off the stack.
        if let Some(index) = index {
            return Box::pin(self.serve_segment_with_read_custody(
                &found.rendition,
                session_id,
                index,
                budget,
                Arc::clone(&found.delivery),
                custody,
            ))
            .await;
        }
        Box::pin(self.serve_init_with_read_custody(
            &found.rendition,
            budget,
            Arc::clone(&found.delivery),
            custody,
        ))
        .await
    }

    pub(super) async fn settle_encoded_split(
        &self,
        requested: &VodCopyLayout,
        mut start: VodStart,
        deadline: Instant,
        fences: &VodCreateFences<'_>,
        allow_source_copy: bool,
    ) -> Result<VodStart, String> {
        let Some(publication) = self.session_media_rendition(&start.session_id, None).await else {
            return Err("encoded split producer disappeared before verification".into());
        };
        let Some(found) = publication.result.map_err(|error| format!("{error:?}"))? else {
            return Err("encoded split producer is unavailable".into());
        };
        let owner = publication.owner;
        if !Arc::ptr_eq(&owner.incarnation, &start.incarnation) {
            return Err("encoded producer ownership changed before split verification".into());
        }
        let prove = async {
            let recipe = &found.rendition.recipe;
            let may_project = recipe.encoding.as_ref().map_or(
                allow_source_copy
                    && !recipe.audio_excluded
                    && !recipe.file.audio_streams.is_empty(),
                |encoding| encoding.plan.options().input_has_audio,
            );
            if !may_project {
                return Err(invalid("paired VOD producer has no soundtrack to project"));
            }
            let entry = media_entry_containing_ms(
                &found.rendition.plan,
                owner.preferred_start_ms.unwrap_or_default(),
            );
            let budget = deadline.saturating_duration_since(Instant::now());
            // Start the exact admitted window first. A processed encoder cannot bootstrap
            // with the ordinary init-head pipe because it needs the verified helper input.
            let first = self
                .encoded_split_input(&start.session_id, &found, &owner, Some(entry), budget, None)
                .await?;
            let ready = self
                .encoded_split_input(&start.session_id, &found, &owner, None, budget, None)
                .await?;
            let bytes = read_projection_input(ready, MAX_INIT_BYTES).await?;
            let init_id = hex::encode(Sha256::digest(&bytes));
            let exact_init = if let Some(artifact) = owner.retained_output.as_ref() {
                artifact.observation.served_init == init_id
            } else {
                found
                    .rendition
                    .identity
                    .lock()
                    .await
                    .identity
                    .as_ref()
                    .is_some_and(|identity| identity.served_init == init_id)
            };
            if !exact_init {
                return Err(invalid("encoded producer init changed during projection"));
            }
            let mut reader = FragmentReader::new();
            reader.push(&bytes);
            let Some(Unit::Init(init)) = reader
                .next_unit()
                .map_err(|error| invalid(error.to_string()))?
            else {
                return Err(invalid(
                    "encoded producer did not publish an initialization record",
                ));
            };
            if reader.buffered() != 0 {
                return Err(invalid("encoded init contains trailing media"));
            }
            let projection =
                EncodedVodProjection::new(&found.rendition, init, recipe.encoding.is_none())?;
            projection
                .project_segment(&found.rendition, entry, first, false, None)
                .await?;
            Ok::<_, VodError>(projection)
        };
        let projected =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), prove).await;
        let (layout, projection) = match projected {
            Ok(Ok(projection)) if requested.pending() || *requested == projection.layout => {
                (projection.layout.clone(), Some(Arc::new(projection)))
            }
            outcome if requested.pending() => {
                let reason = match outcome {
                    Ok(Err(error)) => format!("{error:?}"),
                    Err(_) => "projection deadline expired".into(),
                    Ok(Ok(_)) => "projection identity refused".into(),
                };
                tracing::info!(target: "plurxd::vodserve", file_id = found.rendition.recipe.file.id,
                    %reason, "encoded split verification refused before publication");
                (VodCopyLayout::EncodedMuxed, None)
            }
            _ => {
                return Err("encoded split recovery could not reproduce the issued objects".into())
            }
        };
        let _serving = if let Some(fence) = fences.serving_admission.as_ref() {
            Some(
                fence
                    .commit_guard_before()
                    .await
                    .ok_or("serving authority changed before split publication")?,
            )
        } else {
            None
        };
        let _release = if let Some(fence) = fences.release_fence.as_ref() {
            let lock = fence.transition.lock().await;
            if fence.released.load(Acquire) {
                return Err("encoded producer was released before split publication".into());
            }
            Some(lock)
        } else {
            None
        };
        if found.rendition.closed.load(Acquire)
            || found.rendition.failure().is_some()
            || self.source_changed(&found.rendition)
        {
            return Err("encoded producer changed before split publication".into());
        }
        let mut sessions = self.shared.sessions.lock().await;
        let session = sessions
            .get_mut(&start.session_id)
            .filter(|session| {
                Arc::ptr_eq(&session.incarnation, &owner.incarnation)
                    && session
                        .live_rendition()
                        .is_some_and(|root| Arc::ptr_eq(root, &found.rendition))
                    && session.owns_response_media(&owner)
            })
            .ok_or("encoded producer ownership changed before split publication")?;
        session.vod_copy_layout = Some(layout.clone());
        session.encoded_projection = projection;
        if let Some(attachment_owner) = fences
            .release_fence
            .as_ref()
            .and_then(|fence| fence.attachment_owner)
        {
            *attachment_owner.lock().expect("first preparation owner") =
                Some(session.response_owner());
        }
        start.split = layout.split();
        start.vod_copy_layout = Some(layout);
        Ok(start)
    }

    async fn encoded_split_snapshot(
        &self,
        session_id: &str,
    ) -> Option<(
        Option<Arc<EncodedVodProjection>>,
        ResolvedMediaReader,
        ResponseOwner,
    )> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        if !session.encoded_split_only() {
            return None;
        }
        let projection = session.encoded_projection.clone();
        let rendition = Arc::clone(session.live_rendition()?);
        Some((
            projection,
            ResolvedMediaReader {
                rendition,
                block_budget: session.block_budget,
                delivery: Arc::clone(&session.delivery),
            },
            session.response_owner(),
        ))
    }

    pub(super) async fn encoded_split_master(
        &self,
        session_id: &str,
    ) -> Option<VodPublication<Option<SplitPresentation>>> {
        let (projection, found, owner) = self.encoded_split_snapshot(session_id).await?;
        let result = match projection {
            _ if self.source_changed(&found.rendition) => {
                Err(invalid("encoded split source changed"))
            }
            Some(projection) => Ok(Some(projection.master.clone())),
            None => Err(VodError::Pending {
                retry_after: PENDING_RETRY_AFTER,
            }),
        };
        Some(VodPublication { result, owner })
    }

    pub(super) async fn encoded_split_playlist(
        &self,
        session_id: &str,
        role: &str,
        rendition_id: &str,
    ) -> Option<VodPublication<Option<Vec<u8>>>> {
        let (projection, found, owner) = self.encoded_split_snapshot(session_id).await?;
        let Some(projection) = projection else {
            return Some(VodPublication {
                result: Err(VodError::Pending {
                    retry_after: PENDING_RETRY_AFTER,
                }),
                owner,
            });
        };
        let result = if self.source_changed(&found.rendition) {
            Err(invalid("encoded split source changed"))
        } else {
            Ok(projection.child(role, rendition_id).map(|(init_id, _)| {
                let playlist =
                    plurx_core::transcode::vod_split_media_playlist(&found.rendition.plan, init_id);
                super::vod_serve_delivery::playlist_with_preferred_start(
                    playlist.as_bytes(),
                    owner.preferred_start_ms,
                )
            }))
        };
        Some(VodPublication { result, owner })
    }

    pub(super) async fn encoded_split_segment(
        &self,
        session_id: &str,
        request: &ChildMediaRequest,
        deadline: Instant,
        custody: Option<&crate::transcode::source_actor::resource::SourceResourceReadCustody>,
    ) -> Option<VodPublication<Option<SegmentReady>>> {
        let (projection, found, owner) = self.encoded_split_snapshot(session_id).await?;
        let Some(projection) = projection else {
            return Some(VodPublication {
                result: Err(VodError::Pending {
                    retry_after: PENDING_RETRY_AFTER,
                }),
                owner,
            });
        };
        let result = async {
            let Some((init_id, init)) = projection.child(&request.role, &request.rendition) else {
                return Ok(None);
            };
            if self.source_changed(&found.rendition) {
                return Err(invalid("encoded split source changed"));
            }
            if let Some(custody) = custody {
                custody.source().map_err(invalid)?;
            }
            if request.kind == "init" {
                if request.object.strip_suffix(".mp4") != Some(init_id) {
                    return Ok(None);
                }
                let file =
                    tempfile::tempfile_in(found.rendition.dir.path()).map_err(projection_io)?;
                let mut file = tokio::fs::File::from_std(file);
                use tokio::io::{AsyncSeekExt, AsyncWriteExt};
                file.write_all(&init.bytes).await.map_err(projection_io)?;
                file.rewind().await.map_err(projection_io)?;
                return Ok(Some(SegmentReady {
                    file,
                    len: init.bytes.len() as u64,
                    etag: init_id.to_owned(),
                    observed_media_duration_ms: None,
                    delivery: found.delivery,
                    retained_lease: None,
                    cache_pin: None,
                    projection_scratch: None,
                }));
            }
            let Some(index) = request
                .object
                .strip_suffix(".m4s")
                .and_then(|part| part.parse().ok())
            else {
                return Ok(None);
            };
            if found.rendition.plan.entry(index).is_none() {
                return Ok(None);
            }
            let budget = found
                .block_budget
                .min(deadline.saturating_duration_since(Instant::now()));
            let source = self
                .encoded_split_input(session_id, &found, &owner, Some(index), budget, custody)
                .await?;
            let ready = projection
                .project_segment(
                    &found.rendition,
                    index,
                    source,
                    request.role == "audio",
                    custody,
                )
                .await?;
            if self.source_changed(&found.rendition) {
                return Err(invalid("encoded split source changed during projection"));
            }
            Ok(Some(ready))
        };
        // The HTTP child request already carries the custody and response
        // fences. Poll the projection on the same task with its own frame.
        let result = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            Box::pin(result),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(VodError::Pending {
                retry_after: PENDING_RETRY_AFTER,
            }),
        };
        Some(VodPublication { result, owner })
    }
}
