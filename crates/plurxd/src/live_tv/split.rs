use super::*;
use bytes::Bytes;
use plurx_core::fmp4::{
    self, track_projection::CopyTrackProjection, FragmentReader, Unit, VideoCodec,
};
use plurx_core::transcode::split_master::{
    SplitAudio, SplitBandwidth, SplitBudget, SplitPresentation, SplitVideo, VideoRange,
};
use sha2::{Digest, Sha256};

const WINDOW_BYTES: usize = 96 * 1024 * 1024;
const PAGE_BYTES: usize = 64 * 1024;
const TOTAL_BYTES: usize = 384 * 1024 * 1024;
const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;

static PROJECTION_PAGES: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(TOTAL_BYTES / PAGE_BYTES)));

pub(super) struct WindowBudget {
    _permit: tokio::sync::OwnedSemaphorePermit,
    allocated: std::sync::atomic::AtomicUsize,
}

impl WindowBudget {
    fn reserve() -> Result<Arc<Self>, String> {
        let permit = Arc::clone(&PROJECTION_PAGES)
            .try_acquire_many_owned((WINDOW_BYTES / PAGE_BYTES) as u32)
            .map_err(|_| "live-TV split capacity exhausted")?;
        Ok(Arc::new(Self {
            _permit: permit,
            allocated: std::sync::atomic::AtomicUsize::new(0),
        }))
    }

    fn hold(self: &Arc<Self>, bytes: Vec<u8>) -> Result<Bytes, String> {
        let size = bytes.len();
        self.allocated
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(size)
                    .filter(|total| *total <= WINDOW_BYTES)
            })
            .map_err(|_| "live-TV split window exceeds its byte budget")?;
        Ok(Bytes::from_owner(BoundedBytes {
            bytes,
            budget: Arc::clone(self),
        }))
    }
}

struct BoundedBytes {
    bytes: Vec<u8>,
    budget: Arc<WindowBudget>,
}

impl AsRef<[u8]> for BoundedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
impl Drop for BoundedBytes {
    fn drop(&mut self) {
        self.budget
            .allocated
            .fetch_sub(self.bytes.len(), Ordering::AcqRel);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Role {
    Video,
    Audio,
}

impl Role {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "video" => Some(Self::Video),
            "audio" => Some(Self::Audio),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct Segment {
    video: Bytes,
    audio: Bytes,
    video_seconds: f64,
    audio_seconds: f64,
    video_span: (u64, u64),
    audio_span: (u64, u64),
}

#[derive(Clone, Debug)]
pub(super) struct Window {
    layout: Arc<Layout>,
    segments: Arc<std::collections::BTreeMap<u64, Segment>>,
    retained: Arc<std::collections::BTreeMap<u64, RetainedSegment>>,
    metadata: Arc<std::collections::BTreeMap<u64, Vec<String>>>,
    discontinuity_sequence: Option<String>,
    sequence: u64,
    target: u64,
}

#[derive(Clone, Debug)]
struct RetainedSegment {
    segment: Segment,
    expires: tokio::time::Instant,
}

impl std::fmt::Debug for Segment {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output
            .debug_struct("Segment")
            .field("video_bytes", &self.video.len())
            .field("audio_bytes", &self.audio.len())
            .finish()
    }
}

#[derive(Debug)]
struct Layout {
    video_id: String,
    audio_id: String,
    video_init: Bytes,
    audio_init: Bytes,
    master: String,
}

#[derive(Default)]
pub(super) enum Projection {
    #[default]
    Pending,
    Muxed,
    Split {
        author: Box<CopyTrackProjection>,
        input_init: [u8; 32],
        window: Window,
        budget: Arc<WindowBudget>,
    },
}

impl Window {
    fn id(&self, role: Role) -> &str {
        match role {
            Role::Video => &self.layout.video_id,
            Role::Audio => &self.layout.audio_id,
        }
    }

    pub(super) fn master(&self) -> String {
        self.layout.master.clone()
    }

    pub(super) fn playlist(&self, role: Role, id: &str) -> Option<String> {
        use std::fmt::Write as _;
        if self.id(role) != id {
            return None;
        }
        let mut playlist = format!(
            "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{}\n",
            self.target, self.sequence,
        );
        if let Some(tag) = &self.discontinuity_sequence {
            let _ = writeln!(playlist, "{tag}");
        }
        let _ = writeln!(playlist, "#EXT-X-MAP:URI=\"init/{id}.mp4\"");
        for (sequence, segment) in self.segments.iter() {
            if let Some(tags) = self.metadata.get(sequence) {
                for tag in tags {
                    let _ = writeln!(playlist, "{tag}");
                }
            }
            let duration = match role {
                Role::Video => segment.video_seconds,
                Role::Audio => segment.audio_seconds,
            };
            let _ = writeln!(playlist, "#EXTINF:{duration:.6},\nsegment/{sequence}.m4s");
        }
        Some(playlist)
    }

    pub(super) fn object(&self, role: Role, id: &str, sequence: Option<u64>) -> Option<Bytes> {
        if self.id(role) != id {
            return None;
        }
        match sequence {
            None => Some(match role {
                Role::Video => self.layout.video_init.clone(),
                Role::Audio => self.layout.audio_init.clone(),
            }),
            Some(sequence) => self
                .segments
                .get(&sequence)
                .or_else(|| {
                    self.retained
                        .get(&sequence)
                        .filter(|old| old.expires > tokio::time::Instant::now())
                        .map(|old| &old.segment)
                })
                .map(|segment| match role {
                    Role::Video => segment.video.clone(),
                    Role::Audio => segment.audio.clone(),
                }),
        }
    }
}

impl Projection {
    /// One scan owns the private pair. A refusal before the first accepted
    /// window keeps the original HLS. Later refusals stop that split.
    pub(super) async fn observe(
        &mut self,
        directory: &Path,
        delivery: &LiveDeliveryPlan,
        input: &ScratchInventory,
        eligible: bool,
    ) -> Result<Option<Window>, LiveTvError> {
        if matches!(self, Self::Pending) {
            if !startup_publishable(input) {
                return Ok(None);
            }
            if !eligible {
                *self = Self::Muxed;
            }
        }
        if matches!(self, Self::Muxed) {
            return Ok(None);
        }
        match self.next(directory, delivery, input).await {
            Ok(window) => Ok(Some(window)),
            Err(reason) if matches!(self, Self::Pending) => {
                tracing::info!(target: "plurxd::live_tv", %reason, "live-TV split opening declined");
                *self = Self::Muxed;
                Ok(None)
            }
            Err(reason) => Err(LiveTvError::StreamFailed(reason)),
        }
    }

    async fn next(
        &mut self,
        directory: &Path,
        delivery: &LiveDeliveryPlan,
        input: &ScratchInventory,
    ) -> Result<Window, String> {
        let (init_name, _) = input.init.as_ref().ok_or("no live fMP4 init")?;
        let init = read_bounded_regular_file(&directory.join(init_name), 1024 * 1024)
            .await
            .map_err(|error| error.to_string())?;
        let digest: [u8; 32] = Sha256::digest(&init).into();
        let mut new_author = None;
        let (author, layout, previous, budget) = match self {
            Self::Split {
                author,
                input_init,
                window,
                budget,
            } => {
                if *input_init != digest {
                    return Err("live-TV split initialization changed".into());
                }
                (
                    author.as_mut(),
                    Arc::clone(&window.layout),
                    Some(window.clone()),
                    Arc::clone(budget),
                )
            }
            _ => {
                let mut reader = FragmentReader::new();
                reader.push(&init);
                let Some(Unit::Init(init)) =
                    reader.next_unit().map_err(|error| error.to_string())?
                else {
                    return Err("no complete live fMP4 initialization".into());
                };
                let author = CopyTrackProjection::new(init).map_err(|error| error.to_string())?;
                let budget = WindowBudget::reserve()?;
                let layout = Arc::new(layout(&author, delivery, &budget)?);
                new_author = Some(author);
                (
                    new_author.as_mut().expect("new projection"),
                    layout,
                    None,
                    budget,
                )
            }
        };
        let first = input.media_sequence;
        let last = input.newest_listed().ok_or("no live split media")?;
        let timeline = parse_playlist_bytes(&input.playlist).map_err(|error| error.to_string())?;
        if !timeline.segments.iter().copied().eq(first..=last) {
            return Err("live-TV split requires consecutive media sequences".into());
        }
        let (discontinuity_sequence, metadata) =
            segment_metadata(&input.playlist, &timeline.segments)?;
        let target = previous
            .as_ref()
            .map_or(input.target_duration, |window| window.target);
        let base_retention = Duration::from_secs(target)
            .checked_mul(MAX_LISTED_SEGMENTS as u32)
            .ok_or("live-TV split retention overflow")?;
        let last_previous = previous
            .as_ref()
            .and_then(|window| window.segments.last_key_value());
        if let Some((sequence, _)) = last_previous {
            if first > sequence.saturating_add(1) || last < *sequence {
                return Err("live-TV split sequence did not advance continuously".into());
            }
        }
        let mut segments = std::collections::BTreeMap::new();
        for sequence in first..=last {
            let projected = if let Some(existing) = previous
                .as_ref()
                .and_then(|window| window.segments.get(&sequence))
            {
                existing.clone()
            } else {
                let (name, _) = input
                    .segments
                    .get(&sequence)
                    .ok_or("missing live split pair")?;
                let raw = read_bounded_regular_file(&directory.join(name), MAX_INPUT_BYTES)
                    .await
                    .map_err(|error| error.to_string())?;
                let ordinal =
                    u32::try_from(sequence).map_err(|_| "live-TV segment sequence overflow")?;
                let parts = author
                    .segment(&raw, ordinal)
                    .map_err(|error| error.to_string())?;
                if parts.video.tracks[0]
                    .runs
                    .iter()
                    .flat_map(|run| &run.samples)
                    .next()
                    .is_none_or(|sample| !sample.is_sync())
                {
                    return Err("live-TV video fragment is not independently decodable".into());
                }
                let extent = |track: &fmp4::TrackFragment| {
                    let start = track.base_decode_time;
                    start
                        .checked_add(track.duration())
                        .map(|end| (start, end))
                        .ok_or("live-TV sample time overflow")
                };
                let video_span = extent(&parts.video.tracks[0])?;
                let audio_span = extent(&parts.audio.tracks[0])?;
                Segment {
                    video_seconds: (video_span.1 - video_span.0) as f64
                        / f64::from(author.video_init().tracks[0].timescale),
                    audio_seconds: (audio_span.1 - audio_span.0) as f64 / 48_000.0,
                    video_span,
                    audio_span,
                    video: budget.hold(parts.video.bytes)?,
                    audio: budget.hold(parts.audio.bytes)?,
                }
            };
            let prior = segments
                .last_key_value()
                .or_else(|| last_previous.filter(|(index, _)| index.saturating_add(1) == sequence));
            if let Some((_, previous)) = prior {
                let previous: &Segment = previous;
                let discontinuity = metadata
                    .get(&sequence)
                    .is_some_and(|tags| tags.iter().any(|tag| tag == "#EXT-X-DISCONTINUITY"));
                if !discontinuity
                    && (previous.video_span.1 != projected.video_span.0
                        || previous.audio_span.1 != projected.audio_span.0)
                {
                    return Err("live-TV split track clocks are discontinuous".into());
                }
            }
            if [projected.audio_seconds, projected.video_seconds]
                .into_iter()
                .any(|duration| {
                    !duration.is_finite() || duration <= 0.0 || duration.round() > target as f64
                })
            {
                return Err("live-TV split track duration exceeds its target".into());
            }
            segments.insert(sequence, projected);
        }
        if previous.is_none() {
            let largest_pair = segments
                .values()
                .map(|item| item.video.len() + item.audio.len())
                .max()
                .ok_or("no live split media")?;
            let pairs_with_retention = 2 * MAX_LISTED_SEGMENTS + 2;
            let init_bytes = layout.video_init.len() + layout.audio_init.len();
            if largest_pair > (WINDOW_BYTES - init_bytes) / pairs_with_retention {
                return Err("live-TV split opening pair exceeds the full-window budget".into());
            }
        }
        let now = tokio::time::Instant::now();
        let mut retained = std::collections::BTreeMap::new();
        if let Some(previous) = previous.as_ref() {
            retained.extend(
                previous
                    .retained
                    .iter()
                    .filter(|(_, item)| item.expires > now)
                    .map(|(sequence, item)| (*sequence, item.clone())),
            );
            for (sequence, segment) in previous.segments.range(..first) {
                let removed_duration =
                    Duration::try_from_secs_f64(segment.video_seconds.max(segment.audio_seconds))
                        .map_err(|_| "live-TV split segment duration overflow")?;
                let expires = base_retention
                    .checked_add(removed_duration)
                    .and_then(|lifetime| now.checked_add(lifetime))
                    .ok_or("live-TV split retention deadline overflow")?;
                retained.insert(
                    *sequence,
                    RetainedSegment {
                        segment: segment.clone(),
                        expires,
                    },
                );
            }
        }
        let window = Window {
            layout,
            segments: Arc::new(segments),
            retained: Arc::new(retained),
            metadata: Arc::new(metadata),
            discontinuity_sequence,
            sequence: first,
            target,
        };
        if let Some(author) = new_author {
            *self = Self::Split {
                author: Box::new(author),
                input_init: digest,
                window: window.clone(),
                budget,
            };
        } else if let Self::Split {
            window: current, ..
        } = self
        {
            *current = window.clone();
        }
        Ok(window)
    }
}

fn segment_metadata(
    playlist: &[u8],
    sequences: &[u64],
) -> Result<(Option<String>, std::collections::BTreeMap<u64, Vec<String>>), String> {
    let playlist = std::str::from_utf8(playlist).map_err(|error| error.to_string())?;
    let mut discontinuity_sequence = None;
    let mut pending = Vec::new();
    let mut metadata = std::collections::BTreeMap::new();
    let mut sequences = sequences.iter();
    for line in playlist
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if !line.starts_with('#') {
            let index = sequences.next().ok_or("unexpected live split media URI")?;
            metadata.insert(*index, std::mem::take(&mut pending));
        } else if line.starts_with("#EXT-X-DISCONTINUITY-SEQUENCE:") {
            discontinuity_sequence = Some(line.to_owned());
        } else if line == "#EXT-X-DISCONTINUITY" || line.starts_with("#EXT-X-PROGRAM-DATE-TIME:") {
            pending.push(line.to_owned());
        } else if line.starts_with("#EXT")
            && !(matches!(line, "#EXTM3U" | "#EXT-X-INDEPENDENT-SEGMENTS")
                || [
                    "#EXT-X-VERSION:",
                    "#EXT-X-TARGETDURATION:",
                    "#EXT-X-MEDIA-SEQUENCE:",
                    "#EXT-X-MAP:",
                    "#EXTINF:",
                ]
                .iter()
                .any(|prefix| line.starts_with(prefix)))
        {
            return Err("live-TV split found unsupported playlist metadata".into());
        }
    }
    if !pending.is_empty() || sequences.next().is_some() {
        return Err("live-TV split metadata does not close its window".into());
    }
    Ok((discontinuity_sequence, metadata))
}

fn layout(
    author: &CopyTrackProjection,
    delivery: &LiveDeliveryPlan,
    budget: &Arc<WindowBudget>,
) -> Result<Layout, String> {
    let video = author.video_init();
    let audio = author.audio_init();
    let video_id = hex::encode(Sha256::digest(&video.bytes));
    let audio_id = hex::encode(Sha256::digest(&audio.bytes));
    let facts = fmp4::copy_video_sample_entry_facts(video).map_err(|error| error.to_string())?;
    let audio_facts = fmp4::aac_lc_sample_entry_facts(audio).map_err(|error| error.to_string())?;
    let entry = std::str::from_utf8(&facts.sample_entry).map_err(|error| error.to_string())?;
    let codec = match facts.codec {
        VideoCodec::H264 => fmp4::avc_rfc6381_codec(video).map_err(|error| error.to_string())?,
        VideoCodec::Hevc => crate::http::hls::hevc_codec_from_init(&video.bytes, entry),
    }
    .ok_or("live split codec is unavailable")?;
    let range = facts
        .color
        .and_then(|(_, transfer, _, _)| VideoRange::from_transfer(transfer));
    let range = range
        .or_else(|| {
            (facts.codec == VideoCodec::H264 && facts.bit_depth == 8).then_some(VideoRange::Sdr)
        })
        .ok_or("live split color is unavailable")?;
    if facts.dolby_vision_config {
        return Err("live split has no Dolby Vision proof".into());
    }
    let master = SplitPresentation {
        variants: vec![SplitVideo {
            rendition_id: video_id.clone(),
            codec,
            supplemental_codecs: None,
            width: Some(u32::from(facts.width)),
            height: Some(u32::from(facts.height)),
            frame_rate: delivery
                .output
                .frame_rate
                .map(|rate| f64::from(rate.num) / f64::from(rate.den)),
            video_range: range,
            budget: None,
        }],
        audio: vec![SplitAudio {
            rendition_id: audio_id.clone(),
            source_index: i64::from(delivery.audio_track),
            title: None,
            language: None,
            default: true,
            channels: Some(audio_facts.channels),
            codec: audio_facts.codec.to_owned(),
            budget: None,
        }],
        bandwidth: SplitBandwidth::Combined(SplitBudget {
            peak_bps: delivery
                .max_bitrate_bps
                .unwrap_or(8_000_000)
                .saturating_add(192_000),
            average_bps: None,
        }),
        subtitles: String::new(),
        independent_segments: false,
    }
    .master_text()
    .map_err(|error| error.to_string())?;
    Ok(Layout {
        video_id,
        audio_id,
        video_init: budget.hold(video.bytes.clone())?,
        audio_init: budget.hold(audio.bytes.clone())?,
        master,
    })
}
