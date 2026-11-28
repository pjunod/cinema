use plurx_core::{
    domain::MediaFile,
    fmp4::{self, track_projection::CopyTrackProjection, Init, VideoCodec},
    sharing_resources::SharingHlsRole,
    transcode::split_master::{
        SplitAudio, SplitBandwidth, SplitBudget, SplitPresentation, SplitVideo, VideoRange,
    },
};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, sync::Arc};
use tokio::{
    io::AsyncReadExt,
    sync::{OwnedSemaphorePermit, Semaphore},
};

const MAX_WINDOW_BYTES: usize = 16 * 1024 * 1024;
const SCRATCH_PAGE: usize = 64 * 1024;
static WORKERS: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(4)));
static SCRATCH: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(256 * 1024 * 1024 / SCRATCH_PAGE)));

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub(crate) struct SplitRangeRequest {
    id: String,
    bitrate: u64,
    index: i64,
    name: Option<String>,
    language: Option<String>,
}

impl SplitRangeRequest {
    pub(crate) fn new(file: &MediaFile, id: &str, index: Option<i64>) -> Option<Self> {
        if file.video_codec.as_deref() != Some("h264") || file.hdr.is_some() {
            return None;
        }
        let audio = if let Some(index) = index {
            file.audio_streams
                .iter()
                .find(|audio| audio.index == index)?
        } else {
            file.audio_streams.first()?
        };
        Some(Self {
            id: id.to_owned(),
            bitrate: u64::try_from(file.bitrate?)
                .ok()?
                .saturating_mul(4)
                .max(1_000_000),
            index: audio.index,
            name: audio.title.clone(),
            language: audio.language.clone(),
        })
    }
}

struct Child {
    id: String,
    init: String,
}

pub(crate) struct SplitRange {
    author: CopyTrackProjection,
    video: Child,
    audio: Child,
    pub(crate) presentation: SplitPresentation,
    peak_bps: u64,
}

#[derive(Clone)]
pub(crate) struct PairRow {
    video: TrackRow,
    audio: TrackRow,
}

#[derive(Clone)]
struct TrackRow {
    start: u64,
    end: u64,
    duration: f64,
    digest: String,
}

impl PairRow {
    fn track(&self, role: SharingHlsRole) -> &TrackRow {
        match role {
            SharingHlsRole::Video => &self.video,
            SharingHlsRole::Audio => &self.audio,
        }
    }
    pub(crate) fn duration(&self, role: SharingHlsRole) -> f64 {
        self.track(role).duration
    }
}

impl SplitRange {
    pub(super) fn new(request: SplitRangeRequest, init: Init) -> Result<Self, String> {
        let author = CopyTrackProjection::new(init).map_err(|error| error.to_string())?;
        let picture = author.video_init();
        let soundtrack = author.audio_init();
        let facts =
            fmp4::copy_video_sample_entry_facts(picture).map_err(|error| error.to_string())?;
        if facts.codec != VideoCodec::H264
            || facts.bit_depth != 8
            || facts.dolby_vision_config
            || facts.color.is_some_and(|(_, transfer, _, _)| {
                VideoRange::from_transfer(transfer) != Some(VideoRange::Sdr)
            })
        {
            return Err("bounded Source split requires a verified SDR AVC init".into());
        }
        let video_codec = fmp4::avc_rfc6381_codec(picture)
            .map_err(|error| error.to_string())?
            .ok_or("bounded Source split has no AVC codec string")?;
        let audio =
            fmp4::aac_lc_sample_entry_facts(soundtrack).map_err(|error| error.to_string())?;
        let make_child = |role: &str, init: &Init| {
            let init = digest(&init.bytes);
            let id =
                digest(format!("source-range-split-v1\0{}\0{role}\0{init}", request.id).as_bytes());
            Child { id, init }
        };
        let video_child = make_child("video", picture);
        let audio_child = make_child("audio", soundtrack);
        let presentation = SplitPresentation {
            variants: vec![SplitVideo {
                rendition_id: video_child.id.clone(),
                codec: video_codec,
                supplemental_codecs: None,
                width: Some(u32::from(facts.width)),
                height: Some(u32::from(facts.height)),
                frame_rate: None,
                video_range: VideoRange::Sdr,
                budget: None,
            }],
            audio: vec![SplitAudio {
                rendition_id: audio_child.id.clone(),
                source_index: request.index,
                title: request.name,
                language: request.language,
                default: true,
                channels: Some(audio.channels),
                codec: audio.codec.to_owned(),
                budget: None,
            }],
            bandwidth: SplitBandwidth::Combined(SplitBudget {
                peak_bps: request.bitrate,
                average_bps: None,
            }),
            subtitles: String::new(),
            independent_segments: false,
        };
        presentation
            .master_text()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            author,
            video: video_child,
            audio: audio_child,
            presentation,
            peak_bps: request.bitrate,
        })
    }

    fn child(&self, role: SharingHlsRole) -> (&Child, &Init) {
        match role {
            SharingHlsRole::Video => (&self.video, self.author.video_init()),
            SharingHlsRole::Audio => (&self.audio, self.author.audio_init()),
        }
    }

    pub(crate) fn ids(&self, role: SharingHlsRole) -> (&str, &str) {
        let (child, _) = self.child(role);
        (&child.id, &child.init)
    }

    pub(super) async fn prove(
        self: &Arc<Self>,
        input: &[u8],
        sequence: u32,
        previous: Option<&PairRow>,
    ) -> Result<PairRow, String> {
        let (worker, scratch) = reserve(input.len())?;
        let author = Arc::clone(self);
        let input = input.to_vec();
        let previous = previous.cloned();
        tokio::task::spawn_blocking(move || {
            let _credits = (worker, scratch);
            let parts = author
                .author
                .segment(&input, sequence)
                .map_err(|error| error.to_string())?;
            let track_row = |role, part: &fmp4::Fragment| {
                let (_, init) = author.child(role);
                let track = part.tracks.first().ok_or("missing projected range track")?;
                let start = track.base_decode_time;
                let end = start
                    .checked_add(track.duration())
                    .ok_or("range split clock overflow")?;
                if previous
                    .as_ref()
                    .is_some_and(|previous| previous.track(role).end != start)
                {
                    return Err("range split track does not join its previous segment");
                }
                let duration = (end - start) as f64 / f64::from(init.tracks[0].timescale);
                if !duration.is_finite()
                    || duration <= 0.0
                    || duration.round()
                        > f64::from(plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS)
                {
                    return Err("range split track duration exceeds its target");
                }
                Ok(TrackRow {
                    start,
                    end,
                    duration,
                    digest: digest(&part.bytes),
                })
            };
            let video = track_row(SharingHlsRole::Video, &parts.video)?;
            let audio = track_row(SharingHlsRole::Audio, &parts.audio)?;
            let displacement = |video, audio| {
                video as f64 / f64::from(author.author.video_init().tracks[0].timescale)
                    - audio as f64 / f64::from(author.author.audio_init().tracks[0].timescale)
            };
            let bytes = parts.video.bytes.len() + parts.audio.bytes.len();
            if displacement(video.start, audio.start).abs() > 0.1
                || displacement(video.end, audio.end).abs() > 0.1
                || bytes as f64 * 8.0 / video.duration.min(audio.duration) > author.peak_bps as f64
            {
                return Err(
                    "range split clocks or bandwidth do not prove an aligned pair".to_owned(),
                );
            }
            Ok(PairRow { video, audio })
        })
        .await
        .map_err(|error| error.to_string())?
    }

    pub(crate) async fn init_object(
        &self,
        role: SharingHlsRole,
        directory: PathBuf,
        delivery: Arc<crate::meter::Meter>,
    ) -> Result<crate::vodserve::SegmentReady, String> {
        let (_, init) = self.child(role);
        let (worker, scratch) = reserve(init.bytes.len())?;
        let bytes = init.bytes.clone();
        tokio::task::spawn_blocking(move || {
            let _worker = worker;
            write_child(directory, &bytes, None, delivery, scratch)
        })
        .await
        .map_err(|error| error.to_string())?
    }

    pub(crate) async fn media_object(
        self: Arc<Self>,
        role: SharingHlsRole,
        sequence: u32,
        row: PairRow,
        mut source: crate::vodserve::SegmentReady,
        directory: PathBuf,
        custody: crate::transcode::source_actor::resource::SourceResourceReadCustody,
    ) -> Result<crate::vodserve::SegmentReady, String> {
        let len = usize::try_from(source.len).map_err(|_| "Source range object is too large")?;
        let (worker, scratch) = reserve(len)?;
        let mut bytes = Vec::with_capacity(len);
        (&mut source.file)
            .take(source.len + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| error.to_string())?;
        tokio::task::spawn_blocking(move || {
            let _worker = worker;
            if bytes.len() != len || digest(&bytes) != source.etag || custody.source().is_err() {
                return Err("Source range payload no longer matches its committed digest".into());
            }
            let parts = self
                .author
                .segment(&bytes, sequence)
                .map_err(|error| error.to_string())?;
            let bytes = match role {
                SharingHlsRole::Video => &parts.video.bytes,
                SharingHlsRole::Audio => &parts.audio.bytes,
            };
            let contract = row.track(role);
            if digest(bytes) != contract.digest {
                return Err("Source range child no longer matches the advertised digest".into());
            }
            write_child(
                directory,
                bytes,
                Some((contract.duration * 1000.0).ceil() as u32),
                source.delivery,
                scratch,
            )
        })
        .await
        .map_err(|error| error.to_string())?
    }
}

fn reserve(bytes: usize) -> Result<(OwnedSemaphorePermit, OwnedSemaphorePermit), String> {
    if bytes == 0 || bytes > MAX_WINDOW_BYTES {
        return Err("Source range projection input is outside its byte budget".into());
    }
    let worker = Arc::clone(&WORKERS)
        .try_acquire_owned()
        .map_err(|_| "Source range projection worker is busy")?;
    let scratch = Arc::clone(&SCRATCH)
        .try_acquire_many_owned((4 * bytes).div_ceil(SCRATCH_PAGE) as u32)
        .map_err(|_| "Source range projection scratch is busy")?;
    Ok((worker, scratch))
}

fn write_child(
    directory: PathBuf,
    bytes: &[u8],
    observed_media_duration_ms: Option<u32>,
    delivery: Arc<crate::meter::Meter>,
    scratch: OwnedSemaphorePermit,
) -> Result<crate::vodserve::SegmentReady, String> {
    use std::io::{Seek, Write};
    let mut file = tempfile::tempfile_in(directory).map_err(|error| error.to_string())?;
    file.write_all(bytes)
        .and_then(|()| file.rewind())
        .map_err(|error| error.to_string())?;
    Ok(crate::vodserve::SegmentReady {
        observed_media_duration_ms,
        file: tokio::fs::File::from_std(file),
        len: bytes.len() as u64,
        etag: digest(bytes),
        delivery,
        retained_lease: None,
        cache_pin: None,
        projection_scratch: Some(Arc::new(scratch)),
    })
}
