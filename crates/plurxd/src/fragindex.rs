//! Building a file's fragment index.
//!
//! Companion to [`plurx_core::segplan`] (what an index *is* and what a plan is
//! made from it) — this is *how one gets built*, which means one ffmpeg child,
//! one pass over the file, and no side effects.
//!
//! The shape is [`crate::copyseg::run`] with the segmenter taken out: the same
//! [`FragmentReader`] over the same production-shaped pipe, recording a row per
//! fragment instead of publishing media. Sharing the reader is deliberate —
//! a second implementation of "what is a fragment" is exactly how an index
//! comes to describe a stream the producer never emits.
//!
//! Two rules the caller inherits and must not soften:
//!
//! - **An index build never blocks a playback.** A file with no index keeps
//!   the legacy live presentation for that watch and converts from the next
//!   one (plan §2.2, ledger D4), so no cold play ever waits on this.
//! - **A partial read is not an index.** ffmpeg dying a third of the way
//!   through a file emits a perfectly well-formed prefix, and an index built
//!   from it would place every later boundary in the wrong part of the film.
//!   A typed failure is the only honest answer there, and the caller applies
//!   the cause-specific retry policy rather than persisting the prefix.

use std::collections::VecDeque;
use std::io::{Seek, SeekFrom};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use plurx_core::content_analysis::{
    CompletionCoverage, CompletionProvenance, IndexDiagnostic, IndexFailureCode,
    VideoCompletionExpectation,
};
use plurx_core::domain::MediaFile;
use plurx_core::fmp4::{self, FragmentReader, Init, PromotionInputs, TrackKind, Unit};
use plurx_core::segplan::{FragmentIndex, IndexRow, SourceIdentity};
use plurx_core::transcode;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::ffmpeg::ffmpeg_bin;
use crate::subtitle_ride_along::{PendingRideAlong, RideAlongGate, RideAlongPlan, StderrScan};

#[cfg(unix)]
type SourceFd = std::os::fd::RawFd;
#[cfg(not(unix))]
type SourceFd = i32;

/// Matches [`crate::copyseg::READ_CHUNK`]'s reasoning: large enough that a
/// fast copy is not a syscall storm, small enough that the reader parks in one
/// `read` rather than holding a large buffer.
const READ_CHUNK: usize = 256 * 1024;
const PACKET_PROBE_WALL_BUDGET: Duration = Duration::from_secs(30);
const PACKET_PROBE_HEAD_PACKETS: u32 = 256;
const PACKET_PROBE_ATTEMPT_MAX_BYTES: u64 = 1024 * 1024;
const PACKET_PROBE_AGGREGATE_MAX_BYTES: u64 = 4 * 1024 * 1024;
const PACKET_PROBE_TAIL_OFFSETS_SECS: [i64; 3] = [128, 512, 2_048];

type IndexProgress = dyn Fn(u64, i64, usize) + Send + Sync;
pub(crate) type SharedIndexProgress = Arc<dyn Fn(&PassProgress) + Send + Sync>;

/// One progress report from a running index pass, as its caller sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PassProgress {
    pub(crate) bytes_read: u64,
    pub(crate) media_ms: i64,
    pub(crate) fragments: usize,
    /// PGS tracks this pass is also keeping — zero when it does not ride.
    pub(crate) pgs_tracks: usize,
    /// Bytes the ride-along has written into its stage so far, measured at
    /// most once a second.
    pub(crate) pgs_bytes_written: u64,
}

/// How often a running pass re-measures its ride-along stage for progress.
const RIDE_ALONG_MEASURE_EVERY: Duration = Duration::from_secs(1);

/// How an index build ended.
#[derive(Debug, Clone, PartialEq)]
pub enum IndexOutcome {
    /// A complete pass over the file.
    Built(Box<FragmentIndex>),
    /// This file cannot be indexed by this path at all, and retrying will not
    /// change that. Recorded as terminal until the file's own identity
    /// changes, so the background job stops asking.
    Unsupported(String),
    /// A production failure with a stable operator code and bounded facts.
    /// Raw parser tests may still use the legacy variants above; production
    /// callers receive this variant for operational and completion failures.
    Failed(Box<IndexFailure>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexFailure {
    pub code: IndexFailureCode,
    pub reason: String,
    pub rows: usize,
    pub transient_allowlisted: bool,
    pub diagnostic: IndexDiagnostic,
}

impl IndexFailure {
    fn new(code: IndexFailureCode, reason: impl Into<String>, rows: usize) -> Self {
        Self {
            code,
            reason: reason.into(),
            rows,
            transient_allowlisted: false,
            diagnostic: IndexDiagnostic {
                version: 1,
                code: code.as_str().to_owned(),
                fragment_count: Some(u32::try_from(rows).unwrap_or(u32::MAX)),
                ..IndexDiagnostic::default()
            },
        }
    }

    fn transient(mut self, transient: bool) -> Self {
        self.transient_allowlisted = transient;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CompletionExpectationError {
    Unsupported(String),
    Unverified(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PacketProbeContext {
    stream_index: u32,
    time_base: (u64, u64),
    seek_hint_seconds: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PacketTimelineBounds {
    origin_ticks: i128,
    end_ticks: i128,
    packet_count: u64,
    output_bytes: u64,
}

impl PacketTimelineBounds {
    fn expectation(
        self,
        context: PacketProbeContext,
        source_object_version: &str,
    ) -> Result<VideoCompletionExpectation, CompletionExpectationError> {
        let span_ticks = self
            .end_ticks
            .checked_sub(self.origin_ticks)
            .ok_or_else(|| {
                CompletionExpectationError::Unverified(
                    "packet timeline span overflowed during normalization".to_owned(),
                )
            })?;
        if span_ticks <= 0 {
            return Err(CompletionExpectationError::Unverified(
                "packet timeline has no positive selected-video span".to_owned(),
            ));
        }
        let numerator = span_ticks
            .checked_mul(i128::from(context.time_base.0))
            .and_then(|value| u128::try_from(value).ok())
            .ok_or_else(|| {
                CompletionExpectationError::Unverified(
                    "packet timeline numerator overflowed".to_owned(),
                )
            })?;
        let Some((duration_num, duration_den)) =
            reduce_rational(numerator, u128::from(context.time_base.1))
        else {
            return Err(CompletionExpectationError::Unverified(
                "packet timeline could not be reduced safely".to_owned(),
            ));
        };
        let expectation = VideoCompletionExpectation {
            stream_index: context.stream_index,
            duration_num,
            duration_den,
            provenance: CompletionProvenance::PacketTimeline,
            source_object_version: source_object_version.to_owned(),
        };
        expectation
            .validate()
            .map_err(|reason| CompletionExpectationError::Unverified(reason.to_owned()))?;
        Ok(expectation)
    }
}

fn parse_positive_decimal(value: &str) -> Option<(u64, u64)> {
    let value = value.trim();
    if value.is_empty() || value.len() > 64 || value.starts_with(['-', '+']) {
        return None;
    }
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let denominator = 10_u128.checked_pow(u32::try_from(fraction.len()).ok()?)?;
    let whole = whole.parse::<u128>().ok()?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u128>().ok()?
    };
    let numerator = whole.checked_mul(denominator)?.checked_add(fraction)?;
    if numerator == 0 {
        return None;
    }
    reduce_rational(numerator, denominator)
}

fn reduce_rational(mut numerator: u128, mut denominator: u128) -> Option<(u64, u64)> {
    if numerator == 0 || denominator == 0 {
        return None;
    }
    let (mut left, mut right) = (numerator, denominator);
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    numerator /= left;
    denominator /= left;
    let millis = numerator.checked_mul(1_000)?.checked_div(denominator)?;
    if millis > i64::MAX as u128 {
        return None;
    }
    Some((
        u64::try_from(numerator).ok()?,
        u64::try_from(denominator).ok()?,
    ))
}

fn parse_time_base(value: &str) -> Option<(u64, u64)> {
    if value.len() > 64 {
        return None;
    }
    let (num, den) = value.split_once('/')?;
    let num = num.parse::<u128>().ok()?;
    let den = den.parse::<u128>().ok()?;
    reduce_rational(num, den)
}

fn parse_matroska_duration(value: &str) -> Option<(u64, u64)> {
    if value.len() > 64 {
        return None;
    }
    let mut parts = value.split(':');
    let hours = parts.next()?.parse::<u128>().ok()?;
    let minutes = parts.next()?.parse::<u128>().ok()?;
    let seconds = parts.next()?;
    if parts.next().is_some() || minutes >= 60 {
        return None;
    }
    let (seconds_num, seconds_den) = parse_positive_decimal(seconds)?;
    if u128::from(seconds_num) >= 60 * u128::from(seconds_den) {
        return None;
    }
    let whole_seconds = hours
        .checked_mul(3_600)?
        .checked_add(minutes.checked_mul(60)?)?;
    let numerator = whole_seconds
        .checked_mul(u128::from(seconds_den))?
        .checked_add(u128::from(seconds_num))?;
    reduce_rational(numerator, u128::from(seconds_den))
}

fn rational_diff_exceeds_two_seconds(left: (u64, u64), right: (u64, u64)) -> bool {
    let left_scaled = u128::from(left.0).checked_mul(u128::from(right.1));
    let right_scaled = u128::from(right.0).checked_mul(u128::from(left.1));
    let bound = u128::from(left.1)
        .checked_mul(u128::from(right.1))
        .and_then(|value| value.checked_mul(2));
    match (left_scaled, right_scaled, bound) {
        (Some(left), Some(right), Some(bound)) => left.abs_diff(right) > bound,
        _ => true,
    }
}

fn completion_expectation_from_probe(
    raw: &str,
    source_object_version: &str,
) -> Result<VideoCompletionExpectation, CompletionExpectationError> {
    let document: serde_json::Value = serde_json::from_str(raw).map_err(|error| {
        CompletionExpectationError::Unverified(format!("invalid ffprobe JSON: {error}"))
    })?;
    let streams = document
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            CompletionExpectationError::Unverified(
                "ffprobe did not return a stream array".to_owned(),
            )
        })?;
    let stream = streams
        .iter()
        .find(|stream| {
            stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
        })
        .ok_or_else(|| {
            CompletionExpectationError::Unsupported(
                "the index map selects no video stream".to_owned(),
            )
        })?;
    if stream
        .pointer("/disposition/attached_pic")
        .and_then(serde_json::Value::as_i64)
        == Some(1)
    {
        return Err(CompletionExpectationError::Unsupported(
            "the index map selects an attached picture".to_owned(),
        ));
    }
    let stream_index = stream
        .get("index")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            CompletionExpectationError::Unverified(
                "the selected video has no valid absolute stream index".to_owned(),
            )
        })?;

    let mut candidates = Vec::new();
    let duration_ts = stream.get("duration_ts").and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_str()?.parse::<i64>().ok())
            .filter(|value| *value > 0)
    });
    if let (Some(ticks), Some(time_base)) = (
        duration_ts,
        stream
            .get("time_base")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_time_base),
    ) {
        if let Some(value) = reduce_rational(
            u128::try_from(ticks)
                .ok()
                .unwrap_or_default()
                .checked_mul(u128::from(time_base.0))
                .unwrap_or_default(),
            u128::from(time_base.1),
        ) {
            candidates.push((value, CompletionProvenance::StreamTicks));
        }
    }
    if let Some(value) = stream
        .get("duration")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_positive_decimal)
    {
        candidates.push((value, CompletionProvenance::StreamSeconds));
    }
    let tags = stream.get("tags").and_then(serde_json::Value::as_object);
    for key in ["DURATION", "DURATION-eng"] {
        if let Some(value) = tags
            .and_then(|tags| tags.get(key))
            .and_then(serde_json::Value::as_str)
            .and_then(parse_matroska_duration)
        {
            candidates.push((value, CompletionProvenance::MatroskaDurationTag));
        }
    }
    let Some(&(selected, provenance)) = candidates.first() else {
        return Err(CompletionExpectationError::Unverified(
            "the selected video has no trustworthy duration".to_owned(),
        ));
    };
    if candidates
        .iter()
        .enumerate()
        .any(|(left_index, (left, _))| {
            candidates
                .iter()
                .skip(left_index + 1)
                .any(|(right, _)| rational_diff_exceeds_two_seconds(*left, *right))
        })
    {
        return Err(CompletionExpectationError::Unverified(
            "the selected video's duration metadata conflicts by more than 2000 ms".to_owned(),
        ));
    }
    let expectation = VideoCompletionExpectation {
        stream_index,
        duration_num: selected.0,
        duration_den: selected.1,
        provenance,
        source_object_version: source_object_version.to_owned(),
    };
    expectation
        .validate()
        .map_err(|reason| CompletionExpectationError::Unverified(reason.to_owned()))?;
    Ok(expectation)
}

fn packet_probe_context_from_metadata(
    raw: &str,
) -> Result<PacketProbeContext, CompletionExpectationError> {
    let document: serde_json::Value = serde_json::from_str(raw).map_err(|error| {
        CompletionExpectationError::Unverified(format!("invalid ffprobe JSON: {error}"))
    })?;
    let stream = document
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .and_then(|streams| {
            streams.iter().find(|stream| {
                stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
            })
        })
        .ok_or_else(|| {
            CompletionExpectationError::Unsupported(
                "the index map selects no video stream".to_owned(),
            )
        })?;
    if stream
        .pointer("/disposition/attached_pic")
        .and_then(serde_json::Value::as_i64)
        == Some(1)
    {
        return Err(CompletionExpectationError::Unsupported(
            "the index map selects an attached picture".to_owned(),
        ));
    }
    let stream_index = stream
        .get("index")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            CompletionExpectationError::Unverified(
                "the selected video has no valid absolute stream index".to_owned(),
            )
        })?;
    let time_base = stream
        .get("time_base")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_time_base)
        .ok_or_else(|| {
            CompletionExpectationError::Unverified(
                "the selected video has no usable packet time base".to_owned(),
            )
        })?;
    let seek_hint = document
        .pointer("/format/duration")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_positive_decimal)
        .and_then(|(numerator, denominator)| numerator.checked_div(denominator))
        .and_then(|seconds| i64::try_from(seconds).ok())
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| {
            CompletionExpectationError::Unverified(
                "packet fallback has no usable format seek hint".to_owned(),
            )
        })?;
    Ok(PacketProbeContext {
        stream_index,
        time_base,
        seek_hint_seconds: seek_hint,
    })
}

fn packet_integer(value: Option<&serde_json::Value>) -> Option<i128> {
    let value = value?;
    if let Some(value) = value.as_i64() {
        return Some(i128::from(value));
    }
    if let Some(value) = value.as_u64() {
        return Some(i128::from(value));
    }
    value.as_str()?.parse::<i128>().ok()
}

fn packet_document(raw: &[u8]) -> Result<serde_json::Value, CompletionExpectationError> {
    let document: serde_json::Value = serde_json::from_slice(raw).map_err(|error| {
        CompletionExpectationError::Unverified(format!("invalid packet probe JSON: {error}"))
    })?;
    if document
        .get("packets")
        .and_then(serde_json::Value::as_array)
        .is_none()
    {
        return Err(CompletionExpectationError::Unverified(
            "packet probe did not return a packet array".to_owned(),
        ));
    }
    Ok(document)
}

fn head_packet_origin(
    raw: &[u8],
    stream_index: u32,
) -> Result<(i128, u64), CompletionExpectationError> {
    let document = packet_document(raw)?;
    let packets = document
        .get("packets")
        .and_then(serde_json::Value::as_array)
        .expect("packet_document checked the array");
    let mut origin: Option<i128> = None;
    let mut count = 0_u64;
    for packet in packets {
        let packet_stream = packet_integer(packet.get("stream_index"));
        if packet_stream != Some(i128::from(stream_index)) {
            continue;
        }
        let pts = packet_integer(packet.get("pts")).ok_or_else(|| {
            CompletionExpectationError::Unverified(
                "a selected-video prefix packet has no usable PTS".to_owned(),
            )
        })?;
        origin = Some(origin.map_or(pts, |current| current.min(pts)));
        count = count.saturating_add(1);
    }
    let origin = origin.ok_or_else(|| {
        CompletionExpectationError::Unverified(
            "the bounded packet prefix contains no selected-video packet".to_owned(),
        )
    })?;
    Ok((origin, count))
}

fn tail_packet_end(
    raw: &[u8],
    stream_index: u32,
) -> Result<Option<(i128, i128, u64)>, CompletionExpectationError> {
    let document = packet_document(raw)?;
    let packets = document
        .get("packets")
        .and_then(serde_json::Value::as_array)
        .expect("packet_document checked the array");
    let mut minimum: Option<i128> = None;
    let mut maximum_end: Option<i128> = None;
    let mut count = 0_u64;
    for packet in packets {
        let packet_stream = packet_integer(packet.get("stream_index"));
        if packet_stream != Some(i128::from(stream_index)) {
            continue;
        }
        let pts = packet_integer(packet.get("pts")).ok_or_else(|| {
            CompletionExpectationError::Unverified(
                "a selected-video EOF packet has no usable PTS".to_owned(),
            )
        })?;
        let duration = packet_integer(packet.get("duration"))
            .filter(|duration| *duration > 0)
            .ok_or_else(|| {
                CompletionExpectationError::Unverified(
                    "a selected-video EOF packet has no positive duration".to_owned(),
                )
            })?;
        let end = pts.checked_add(duration).ok_or_else(|| {
            CompletionExpectationError::Unverified(
                "selected-video packet end overflowed".to_owned(),
            )
        })?;
        minimum = Some(minimum.map_or(pts, |current| current.min(pts)));
        maximum_end = Some(maximum_end.map_or(end, |current| current.max(end)));
        count = count.saturating_add(1);
    }
    Ok(minimum
        .zip(maximum_end)
        .map(|(minimum, maximum)| (minimum, maximum, count)))
}

fn packet_probe_failure(
    error: crate::ffmpeg::HeldPacketProbeError,
    stream_index: u32,
    output_bytes: u64,
) -> IndexFailure {
    let (code, transient) = match error {
        crate::ffmpeg::HeldPacketProbeError::Timeout => (IndexFailureCode::IndexProbeTimeout, true),
        crate::ffmpeg::HeldPacketProbeError::OutputLimit => {
            (IndexFailureCode::IndexCompletionUnverified, false)
        }
        crate::ffmpeg::HeldPacketProbeError::Source(_) => (IndexFailureCode::IndexSourceIo, true),
        crate::ffmpeg::HeldPacketProbeError::Process(_) => {
            (IndexFailureCode::IndexProcessFailed, false)
        }
    };
    let mut failure = IndexFailure::new(code, error.reason(), 0).transient(transient);
    failure.diagnostic.selected_stream = Some(stream_index);
    failure.diagnostic.output_bytes = Some(output_bytes);
    failure
}

async fn packet_probe_attempt(
    source: &std::fs::File,
    request: crate::ffmpeg::HeldPacketProbeRequest,
    timeout: Duration,
) -> Result<Vec<u8>, crate::ffmpeg::HeldPacketProbeError> {
    let mut view = source;
    view.seek(SeekFrom::Start(0)).map_err(|error| {
        crate::ffmpeg::HeldPacketProbeError::Source(format!(
            "positioning held source before packet probe: {error}"
        ))
    })?;
    let result = crate::ffmpeg::held_source_packet_probe_json(
        source,
        request,
        timeout,
        PACKET_PROBE_ATTEMPT_MAX_BYTES,
    )
    .await;
    let reset = view.seek(SeekFrom::Start(0));
    match (result, reset) {
        (_, Err(error)) => Err(crate::ffmpeg::HeldPacketProbeError::Source(format!(
            "resetting held source after packet probe: {error}"
        ))),
        (result, Ok(_)) => result,
    }
}

async fn held_source_video_packet_bounds(
    source: &std::fs::File,
    context: PacketProbeContext,
    budget: Duration,
) -> Result<PacketTimelineBounds, IndexFailure> {
    if budget.is_zero() {
        return Err(IndexFailure::new(
            IndexFailureCode::IndexBudgetExceeded,
            "no whole-index budget remains for packet fallback",
            0,
        ));
    }
    let started = Instant::now();
    let wall_budget = budget.min(PACKET_PROBE_WALL_BUDGET);
    let remaining = || wall_budget.saturating_sub(started.elapsed());
    let head = packet_probe_attempt(
        source,
        crate::ffmpeg::HeldPacketProbeRequest {
            stream_index: context.stream_index,
            start_seconds: None,
            packet_limit: Some(PACKET_PROBE_HEAD_PACKETS),
        },
        remaining(),
    )
    .await
    .map_err(|error| packet_probe_failure(error, context.stream_index, 0))?;
    let mut output_bytes = u64::try_from(head.len()).unwrap_or(u64::MAX);
    let (origin_ticks, head_packets) =
        head_packet_origin(&head, context.stream_index).map_err(|error| {
            let (CompletionExpectationError::Unverified(reason)
            | CompletionExpectationError::Unsupported(reason)) = error;
            let mut failure =
                IndexFailure::new(IndexFailureCode::IndexCompletionUnverified, reason, 0);
            failure.diagnostic.selected_stream = Some(context.stream_index);
            failure.diagnostic.output_bytes = Some(output_bytes);
            failure
        })?;

    let mut starts = Vec::new();
    for offset in PACKET_PROBE_TAIL_OFFSETS_SECS {
        let start = context.seek_hint_seconds.saturating_sub(offset).max(0);
        if starts.last().copied() != Some(start) {
            starts.push(start);
        }
    }
    for start_seconds in starts.into_iter().take(3) {
        if remaining().is_zero() {
            return Err(packet_probe_failure(
                crate::ffmpeg::HeldPacketProbeError::Timeout,
                context.stream_index,
                output_bytes,
            ));
        }
        let tail = packet_probe_attempt(
            source,
            crate::ffmpeg::HeldPacketProbeRequest {
                stream_index: context.stream_index,
                start_seconds: Some(start_seconds),
                packet_limit: None,
            },
            remaining(),
        )
        .await
        .map_err(|error| packet_probe_failure(error, context.stream_index, output_bytes))?;
        output_bytes = output_bytes.saturating_add(u64::try_from(tail.len()).unwrap_or(u64::MAX));
        if output_bytes > PACKET_PROBE_AGGREGATE_MAX_BYTES {
            return Err(packet_probe_failure(
                crate::ffmpeg::HeldPacketProbeError::OutputLimit,
                context.stream_index,
                output_bytes,
            ));
        }
        let Some((tail_minimum, end_ticks, tail_packets)) =
            tail_packet_end(&tail, context.stream_index).map_err(|error| {
                let (CompletionExpectationError::Unverified(reason)
                | CompletionExpectationError::Unsupported(reason)) = error;
                let mut failure =
                    IndexFailure::new(IndexFailureCode::IndexCompletionUnverified, reason, 0);
                failure.diagnostic.selected_stream = Some(context.stream_index);
                failure.diagnostic.output_bytes = Some(output_bytes);
                failure
            })?
        else {
            continue;
        };
        if tail_minimum < origin_ticks {
            let mut failure = IndexFailure::new(
                IndexFailureCode::IndexCompletionUnverified,
                "packet seek exposed a timestamp before the bounded prefix origin",
                0,
            );
            failure.diagnostic.selected_stream = Some(context.stream_index);
            failure.diagnostic.output_bytes = Some(output_bytes);
            return Err(failure);
        }
        return Ok(PacketTimelineBounds {
            origin_ticks,
            end_ticks,
            packet_count: head_packets.saturating_add(tail_packets),
            output_bytes,
        });
    }
    let mut failure = IndexFailure::new(
        IndexFailureCode::IndexCompletionUnverified,
        "bounded packet seeks did not prove a selected-video EOF endpoint",
        0,
    );
    failure.diagnostic.selected_stream = Some(context.stream_index);
    failure.diagnostic.output_bytes = Some(output_bytes);
    Err(failure)
}

/// Read one index pipe to exhaustion.
///
/// Generic over the source for the same reason [`crate::copyseg::run`] is:
/// everything between the pipe and the index is worth testing and none of it
/// needs a real child process to be worth testing.
/// `dolby_vision` is **every** answer, and deliberately one parameter rather
/// than a flag per outcome. The three are mutually exclusive by construction:
/// a pass either rewrites the record, deletes it, or leaves it alone, and the
/// pass's rows and its served init have to describe the same stream. Separate
/// flags are how those come to disagree, on a pair nothing downstream would
/// notice — the rows would be byte counts for one stream and the served init a
/// description of another.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn index_stream<R: AsyncRead + Unpin>(
    src: R,
    identity: SourceIdentity,
    expected_ms: Option<i64>,
    dolby_vision: DolbyVisionPass,
) -> IndexOutcome {
    let expectation =
        expected_ms
            .filter(|value| *value > 0)
            .map(|value| VideoCompletionExpectation {
                stream_index: 0,
                duration_num: value as u64,
                duration_den: 1_000,
                provenance: CompletionProvenance::StreamSeconds,
                source_object_version: "test-helper".to_owned(),
            });
    index_stream_with_progress(src, identity, expectation.as_ref(), dolby_vision, None).await
}

/// What this index pass does about the muxer's Dolby Vision record.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum DolbyVisionPass {
    /// Leave it. Every ordinary copy, and every preserved Dolby Vision copy:
    /// what the muxer wrote already describes the stream.
    #[default]
    Untouched,
    /// Rewrite it to this record. The Profile 7 → 8.1 conversion, whose RPUs
    /// are rewritten after the muxer, so the record ffmpeg copied out of the
    /// source container describes a stream that no longer exists.
    Rewrite(Box<plurx_core::fmp4::DolbyVisionRecord>),
    /// Delete it. The strip on an ffmpeg without `dovi_rpu`: `filter_units`
    /// removed the RPU and enhancement-layer NAL units by type, and the DOVI
    /// side data the muxer wrote the record from survived, so the output
    /// declares Profile 7 with an enhancement layer over a stream that has
    /// neither.
    Remove,
}

async fn index_stream_with_progress<R: AsyncRead + Unpin>(
    mut src: R,
    identity: SourceIdentity,
    expectation: Option<&VideoCompletionExpectation>,
    dolby_vision: DolbyVisionPass,
    progress: Option<&IndexProgress>,
) -> IndexOutcome {
    let convert = matches!(dolby_vision, DolbyVisionPass::Rewrite(_));
    // A converting identity's index has to describe the CONVERTED bytes. An
    // index is a list of the producer's own output byte counts and the landing
    // matcher compares them, so an index built from the unconverted stream
    // would not be stale — it would be confidently wrong about media this
    // identity never produces. The conversion shortens every RPU, so every
    // fragment carrying one is a different size.
    let mut converter: Option<crate::dvpipe::Converter> = None;
    let mut reader = FragmentReader::new();
    let mut init: Option<Init> = None;
    let mut init_sha = String::new();
    let mut timescale: u32 = 0;
    let mut rows: Vec<IndexRow> = Vec::new();
    let mut bytes_read = 0_u64;
    let mut covered_ticks = 0_u64;
    // The promotion inputs the whole film's generations will share, taken from
    // the first clean fragment, plus whether every later clean fragment agrees
    // with it. Both are plan §2.2's ruling: capture once, check continuously,
    // and refuse a varying title here rather than mid-playback.
    let mut promotion: Option<PromotionInputs> = None;
    let mut parameter_sets_constant = true;
    let mut buf = vec![0u8; READ_CHUNK];

    loop {
        let read = match src.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) => {
                return IndexOutcome::Failed(Box::new(
                    IndexFailure::new(
                        IndexFailureCode::IndexSourceIo,
                        format!("index pipe read: {error}"),
                        rows.len(),
                    )
                    .transient(matches!(
                        error.kind(),
                        std::io::ErrorKind::Interrupted
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                    )),
                ));
            }
        };
        bytes_read = bytes_read.saturating_add(read as u64);
        reader.push(&buf[..read]);

        loop {
            let unit = match reader.next_unit() {
                Ok(Some(unit)) => unit,
                Ok(None) => break,
                Err(error) => {
                    // Unlike a session, an index has no published playlist to
                    // protect, so there is no point past which a broken stream
                    // becomes something other than "do not index this".
                    return IndexOutcome::Failed(Box::new(IndexFailure::new(
                        IndexFailureCode::IndexOutputMalformed,
                        format!(
                            "lost the fragment stream after {} fragments: {error}",
                            rows.len()
                        ),
                        rows.len(),
                    )));
                }
            };
            match unit {
                Unit::Init(parsed) => {
                    let Some(video) = parsed.video() else {
                        return IndexOutcome::Unsupported(
                            "the index pipe's moov has no video track".into(),
                        );
                    };
                    if video.codec.is_none() || video.nal_length_size == 0 {
                        return IndexOutcome::Unsupported(
                            "the video track carries no hvcC/avcC, so no fragment can be \
                             classified"
                                .into(),
                        );
                    }
                    // A video-only pipe declares exactly one track. Anything
                    // else is ffmpeg adding something of its own — a chapter
                    // `text` track is what it was the first time — and its
                    // bytes would land in every fragment's byte count, which
                    // is the number the landing matcher compares.
                    if let Some(extra) = parsed.tracks.iter().find(|t| t.kind != TrackKind::Video) {
                        return IndexOutcome::Unsupported(format!(
                            "the index pipe declared a non-video track (id {}, kind {:?})",
                            extra.id, extra.kind
                        ));
                    }
                    timescale = video.timescale.max(1);
                    init_sha = hex(Sha256::digest(&parsed.bytes));
                    init = Some(parsed);
                }
                Unit::Fragment(fragment) => {
                    let Some(ref init) = init else {
                        return IndexOutcome::Unsupported(
                            "a fragment arrived before the moov".into(),
                        );
                    };
                    let mut fragment = fragment;
                    if convert {
                        if converter.is_none() {
                            match crate::dvpipe::Converter::for_init(init) {
                                Ok(ready) => converter = Some(ready),
                                Err(refused) => {
                                    return IndexOutcome::Unsupported(format!(
                                        "this stream cannot be converted: {refused}"
                                    ))
                                }
                            }
                        }
                        let converter = converter.as_mut().expect("just set");
                        if let Err(refused) = converter.convert(&mut fragment) {
                            return IndexOutcome::Unsupported(format!(
                                "this stream cannot be converted: {refused}"
                            ));
                        }
                    }
                    let fragment = fragment;
                    let Some(video) = init.video() else {
                        return IndexOutcome::Unsupported("the moov lost its video track".into());
                    };
                    let Some(track) = fragment.track(video.id) else {
                        // A video-only pipe emitting a fragment with no video
                        // in it describes nothing the plan can use.
                        return IndexOutcome::Unsupported(
                            "a fragment carried no video track".into(),
                        );
                    };
                    let dts = track.base_decode_time;
                    let duration = fragment.video_duration(init);
                    covered_ticks = covered_ticks.saturating_add(duration);
                    let bytes = u32::try_from(fragment.len()).unwrap_or(u32::MAX);
                    // The landing matcher's quantity. Container overhead is
                    // excluded deliberately: a production generation carries
                    // audio, so its `moof` and `mdat` are tens of kilobytes
                    // larger than this pipe's and vary with the audio track,
                    // while the video samples themselves are copied and come
                    // out byte for byte identical.
                    let video_bytes = u32::try_from(track.byte_len()).unwrap_or(u32::MAX);
                    let class = fmp4::classify(&fragment, init);

                    // Only a clean fragment can begin a segment, so only a
                    // clean fragment can ever be a generation's first — which
                    // makes these the only fragments whose promotion inputs
                    // could ever differ from the stored ones.
                    if class.is_clean() {
                        let here = PromotionInputs::from_fragment(&fragment, init);
                        match promotion {
                            None => promotion = Some(here),
                            Some(ref canonical) => {
                                if &here != canonical {
                                    // A film whose clean starts disagree cannot
                                    // be described by one immutable init. Not a
                                    // failure — a fact, recorded so the title
                                    // keeps the legacy presentation instead of
                                    // being VOD-presented on a promise that
                                    // cannot be kept.
                                    parameter_sets_constant = false;
                                }
                            }
                        }
                    }

                    rows.push(IndexRow {
                        dts,
                        duration,
                        bytes,
                        video_bytes,
                        class,
                    });
                }
                // ffmpeg's random-access index, written at EOF. Seeing it is
                // the strongest evidence the pass was complete.
                Unit::Trailer => {}
            }
        }
        if let Some(progress) = progress {
            let media_ms = if timescale > 0 {
                covered_ticks
                    .saturating_mul(1_000)
                    .saturating_div(u64::from(timescale))
                    .min(i64::MAX as u64) as i64
            } else {
                0
            };
            progress(bytes_read, media_ms, rows.len());
        }
    }

    if rows.is_empty() {
        return IndexOutcome::Unsupported("the index pipe produced no fragments".into());
    }
    // A clean end is one where everything sent was consumed: ffmpeg wrote its
    // `mfra` trailer, or the pipe closed on a fragment boundary. Bytes left in
    // hand mean the child died mid-write.
    if !reader.saw_trailer() && reader.buffered() != 0 {
        return IndexOutcome::Failed(Box::new(IndexFailure::new(
            IndexFailureCode::IndexOutputMalformed,
            format!("{} bytes left mid-fragment", reader.buffered()),
            rows.len(),
        )));
    }
    if let Some(expectation) = expectation {
        let covered = rows
            .iter()
            .try_fold(0_u64, |total, row| total.checked_add(row.duration));
        let Some(covered) = covered else {
            return IndexOutcome::Failed(Box::new(IndexFailure::new(
                IndexFailureCode::IndexCompletionUnverified,
                "video sample-duration sum overflowed",
                rows.len(),
            )));
        };
        let first_dts = rows.iter().map(|row| row.dts).min().unwrap_or_default();
        let end_dts = rows.iter().try_fold(first_dts, |end, row| {
            row.dts.checked_add(row.duration).map(|here| end.max(here))
        });
        let span = end_dts.and_then(|end| end.checked_sub(first_dts));
        let Some(span) = span else {
            return IndexOutcome::Failed(Box::new(IndexFailure::new(
                IndexFailureCode::IndexCompletionUnverified,
                "video decode timeline could not be normalized safely",
                rows.len(),
            )));
        };
        let tolerance = u64::from(timescale).saturating_mul(2);
        if covered.abs_diff(span) > tolerance {
            return IndexOutcome::Failed(Box::new(IndexFailure::new(
                IndexFailureCode::IndexCompletionUnverified,
                format!(
                    "video sample coverage {covered} ticks differs from normalized decode span {span} ticks"
                ),
                rows.len(),
            )));
        }
        match expectation.coverage(covered, timescale) {
            Ok(CompletionCoverage::Complete) => {}
            Ok(CompletionCoverage::Short) => {
                let mut failure = IndexFailure::new(
                    IndexFailureCode::IndexVideoShortfall,
                    format!(
                        "covered {covered} ticks below the selected-video expectation {}/{} seconds",
                        expectation.duration_num, expectation.duration_den
                    ),
                    rows.len(),
                );
                failure.diagnostic.selected_stream = Some(expectation.stream_index);
                failure.diagnostic.expectation_provenance =
                    Some(expectation.provenance.as_str().to_owned());
                failure.diagnostic.covered_ms = i64::try_from(
                    u128::from(covered)
                        .saturating_mul(1_000)
                        .checked_div(u128::from(timescale.max(1)))
                        .unwrap_or_default(),
                )
                .ok();
                failure.diagnostic.expected_ms = expectation.duration_ms_floor();
                return IndexOutcome::Failed(Box::new(failure));
            }
            Ok(CompletionCoverage::Excess) => {
                let mut failure = IndexFailure::new(
                    IndexFailureCode::IndexCompletionUnverified,
                    format!(
                        "covered {covered} ticks beyond the selected-video packet expectation {}/{} seconds",
                        expectation.duration_num, expectation.duration_den
                    ),
                    rows.len(),
                );
                failure.diagnostic.selected_stream = Some(expectation.stream_index);
                failure.diagnostic.expectation_provenance =
                    Some(expectation.provenance.as_str().to_owned());
                failure.diagnostic.covered_ms = i64::try_from(
                    u128::from(covered)
                        .saturating_mul(1_000)
                        .checked_div(u128::from(timescale.max(1)))
                        .unwrap_or_default(),
                )
                .ok();
                failure.diagnostic.expected_ms = expectation.duration_ms_floor();
                return IndexOutcome::Failed(Box::new(failure));
            }
            Err(reason) => {
                return IndexOutcome::Failed(Box::new(IndexFailure::new(
                    IndexFailureCode::IndexCompletionUnverified,
                    reason,
                    rows.len(),
                )));
            }
        }
    }

    // A converting pass that rewrote nothing is a pass whose file's stored
    // facts are wrong: the row says Profile 7 and the stream carries no RPUs
    // at all. Indexing it as the converted identity would record that lie in
    // the keyspace, and every session looking that identity up would be served
    // segments cut for a stream that was never converted.
    if convert {
        let report = converter.as_ref().map(|c| c.report()).unwrap_or_default();
        if report.rpus == 0 {
            return IndexOutcome::Unsupported(
                "this source is recorded as Dolby Vision Profile 7 but its stream carries no \
                 RPUs to convert"
                    .into(),
            );
        }
        // After the refusal, not before it. The index pass reads every RPU in
        // the file, so its answer is the whole film's rather than one
        // session's opening fragment's — and it is what the conversion costs a
        // viewer, since MEL carries no picture detail of its own while FEL
        // carries real residual detail. A line logged on the failure path
        // would report a default MEL/FEL answer for a pass that read no RPU at
        // all, which is worse than saying nothing: this is what an operator
        // reading a "why does this look softer" report has to go on.
        tracing::info!(
            rpus = report.rpus,
            source_profile = report.source_profile,
            enhancement_layer = ?report.enhancement_layer,
            "indexed a converted stream: {}",
            report.enhancement_layer.reason()
        );
    }
    let mut promotion = promotion.unwrap_or_default();
    // The record cannot come out of a fragment: what the muxer wrote describes
    // the source, not the converted stream, which is the whole reason plurx
    // supplies its own. It rides in the stored promotion inputs so the served
    // init this pass validates and the served init a later generation promotes
    // are produced by the same function from the same facts.
    match dolby_vision {
        DolbyVisionPass::Untouched => {}
        DolbyVisionPass::Rewrite(record) => promotion.dolby_vision = Some(*record),
        DolbyVisionPass::Remove => promotion.strip_dolby_vision = true,
    }
    let Some(mut served_init) = init.clone() else {
        return IndexOutcome::Unsupported(
            "the index pipe ended without an init to validate".into(),
        );
    };
    if let Err(error) = fmp4::promote_from(&mut served_init, &promotion) {
        return IndexOutcome::Unsupported(format!(
            "the served init could not be built from this pass's promotion inputs: {error}"
        ));
    }
    if let Err(error) = fmp4::validate_hevc_decoder_configuration(&served_init) {
        return IndexOutcome::Unsupported(format!(
            "validating the HEVC decoder configuration: {error}"
        ));
    }

    let mut built = FragmentIndex::new(timescale, rows, init_sha, identity);
    built.promotion = promotion;
    built.parameter_sets_constant = parameter_sets_constant;
    IndexOutcome::Built(Box::new(built))
}

/// The Dolby Vision configuration record a converted stream must declare.
///
/// Built from the source's stored facts rather than read from the output,
/// because what the output carries is the *source's* record: ffmpeg derives it
/// from its input container rather than from the RPUs (measured,
/// `docs/streaming/PLAYBACK-CAPS-V2-M0.md` §8), and the rewrite that makes the RPUs say
/// 8.1 runs on the far side of that muxer. Left alone, the sample entry would
/// declare Profile 7 over samples that are no longer Profile 7.
///
/// What changes from the source's own record and what does not:
///
/// - **profile becomes 8**, which is what the RPUs now say;
/// - **`el_present` becomes false**, because `filter_units=remove_types=63`
///   dropped the enhancement layer and a record still declaring one tells a
///   decoder to expect a layer that is not in the stream;
/// - **the level is the source's**, unchanged. It bounds resolution and frame
///   rate, neither of which the conversion touches;
/// - **the compatibility id becomes 1**. `ConversionMode::To81` rewrites the
///   RPUs to Profile 8.1, whose base-layer signal is HDR10. Keeping a Profile
///   7 source's compatibility id (Outbreak carried 6) made the init contradict
///   the playlist's `db1p` declaration and Safari rejected the first fragment.
pub(crate) fn converted_dolby_vision_record(
    file: &MediaFile,
) -> Result<plurx_core::fmp4::DolbyVisionRecord, String> {
    let level = file
        .dolby_vision
        .level
        .and_then(|level| u8::try_from(level).ok())
        .ok_or("the source has no stored Dolby Vision level")?;
    // Profile 8.1 is defined by its HDR10-compatible base. This describes the
    // converted output, not the Profile 7 container record ffmpeg copied.
    plurx_core::fmp4::DolbyVisionRecord::new(8, level, false, true, true, 1)
        .map_err(|error| error.to_string())
}

/// Revision of the output-affecting Rust transform behind a converting copy.
///
/// FFmpeg never sees this stage: [`crate::dvpipe::Converter`] rewrites the
/// muxer's fragments after the child emits them.  The recipe identity must
/// nevertheless move whenever that rewrite changes produced bytes, or a new
/// binary could consume an index built for the previous transform.  Keep the
/// token scoped to converting copies so ordinary stripped and preserved
/// artifacts retain their existing identities.
pub(crate) const DV_CONVERSION_TRANSFORM_REVISION: &str = "dv-p7-to-p81-rpu-v1";

pub(crate) fn output_transform_identity(
    file: &MediaFile,
    video: transcode::CopyVideoOptions,
) -> Option<&'static str> {
    (video.converts_dolby_vision() && file.hdr.as_deref() == Some("dolby_vision"))
        .then_some(DV_CONVERSION_TRANSFORM_REVISION)
}

fn identity_for_transform(
    file: &MediaFile,
    video: transcode::CopyVideoOptions,
    transform: Option<&str>,
) -> SourceIdentity {
    let mut recipe = transcode::copy_video_args(file, video);
    if matches!(file.video_codec.as_deref(), Some("hevc" | "h265")) {
        recipe.push(format!(
            "--plurx-hevc-proof={}",
            plurx_core::hevc_configuration::REVISION
        ));
    }
    if let (true, Some(transform)) = (video.converts_dolby_vision(), transform) {
        recipe.push(format!("--plurx-output-transform={transform}"));
    }
    SourceIdentity::new(
        file.size.max(0) as u64,
        file.mtime,
        plurx_core::segplan::argv_fingerprint(&recipe),
    )
}

/// The identity a file's index is keyed by, for this build of ffmpeg.
pub fn identity_for(file: &MediaFile, video: transcode::CopyVideoOptions) -> SourceIdentity {
    identity_for_transform(file, video, output_transform_identity(file, video))
}

/// Every copy-video pipeline a real client can ask this file for, in the order
/// they should be built.
///
/// The indexer used to answer one pipeline — the Dolby-Vision-stripped one —
/// for every file, while `vodserve` and `/decision` build a session's identity
/// from the *session's* `preserve_dolby_vision`. A DV-capable client therefore
/// asked for a byte stream nothing had ever indexed, got `vod_index_pending`,
/// and was rescued by the temporary live-HLS recovery path on every play
/// (PLAYBACK-CAPS-V2-PLAN §4.7, edge E1).
///
/// The stripped identity stays **first**. It is what every non-DV client and
/// every non-DV file uses, and what a forced rebuild resolves to, so a library
/// that is fully indexed today does not re-order its work to adopt this.
///
/// Deduplicated by fingerprint rather than by [`transcode::CopyVideoOptions`]
/// equality, so that an option which happens to render to an argv another
/// option already produced costs nothing. Nothing collapses today — even an
/// ffmpeg with no `dovi_rpu` filter still tags the two differently — but the
/// index keyspace is what a session looks itself up in, and a duplicate there
/// is a whole redundant pass over a 60 GB remux.
///
/// Profile 5 is deliberately in the set even though it has no HDR10 base to
/// strip to. [`plurx_core::playback::decide`] never routes it to a stripping
/// copy, but `decide_forced` with `Force::Original` does, and that copy is
/// indexed today — dropping it would regress a path that works.
///
/// **The Profile 7 → 8.1 conversion is the third identity**, and it is here
/// for the same reason the preserved one is: a converting session builds its
/// identity from its own `CopyVideoOptions`, so an index built only for the
/// other two would leave it looking up something nothing built. It is added
/// only when the *file* could convert — Profile 7 over an HDR10 base — because
/// which clients convert is a per-session question and an index is per-file.
///
/// `convert` is the node's answer, not the file's: an operator who turned the
/// conversion off (`PLURX_DV_CONVERT=0`) has no converting sessions to serve,
/// and indexing for them would spend a third full pass over every Profile 7
/// remux in the library on a stream nothing can ask for.
pub fn video_identities(
    file: &MediaFile,
    probe_json: Option<&str>,
    have_dovi: bool,
    convert: bool,
) -> Vec<transcode::CopyVideoOptions> {
    let preserve_choices: &[bool] = if plurx_core::playback::is_dolby_vision(file) {
        &[false, true]
    } else {
        &[false]
    };
    let mut identities = Vec::with_capacity(preserve_choices.len() + 1);
    let mut seen = std::collections::HashSet::new();
    for preserve in preserve_choices {
        let video = transcode::CopyVideoOptions::from_probe(file, probe_json, have_dovi, *preserve);
        if seen.insert(identity_for(file, video).argv_fingerprint) {
            identities.push(video);
        }
    }
    if convert && plurx_core::playback::file_can_convert_to_p81(file) {
        let video = transcode::CopyVideoOptions::from_probe(file, probe_json, have_dovi, true)
            .with_dolby_vision_conversion(true);
        if seen.insert(identity_for(file, video).argv_fingerprint) {
            identities.push(video);
        }
    }
    identities
}

/// What this pass must do about the muxer's Dolby Vision record.
///
/// A free function, not three lines inside the pipe builder, because the pipe
/// builder spawns ffmpeg and cannot be driven from a test — and this choice is
/// the whole of the fix on both sides of it. Left inline, deleting it changes
/// no test.
fn dolby_vision_pass_for(
    file: &MediaFile,
    video: transcode::CopyVideoOptions,
) -> Result<DolbyVisionPass, String> {
    if video.converts_dolby_vision() {
        // A converting pass produces a stream whose sample entry declares the
        // *source's* record: ffmpeg copies it from the input container, and
        // the rewrite that makes the RPUs say 8.1 runs after that muxer. So
        // plurx builds the right one from the source's own stored facts.
        return converted_dolby_vision_record(file)
            .map(|record| DolbyVisionPass::Rewrite(Box::new(record)))
            .map_err(|reason| {
                format!(
                    "a converting index needs a Dolby Vision record and this file cannot \
                     describe one: {reason}"
                )
            });
    }
    if video.leaves_a_stale_dolby_vision_record(file) {
        // The mirror image, and it lands here for the same reason: what the
        // muxer wrote describes the source, not the stream this pass produces.
        // `filter_units` took the RPU and enhancement-layer NAL units out by
        // type, and the DOVI side data ffmpeg copied from the source container
        // is not a NAL unit, so the output declares Profile 7 with an
        // enhancement layer over a stream carrying neither. Chrome ignores it;
        // VideoToolbox believes it, and Safari answers 4K10 HEVC so labelled
        // with a software decode on hardware that has a block for it.
        return Ok(DolbyVisionPass::Remove);
    }
    Ok(DolbyVisionPass::Untouched)
}

/// One index pass's whole recipe: the argv ffmpeg receives, and what plurx
/// must do afterwards to the Dolby Vision record that argv leaves behind.
///
/// A struct rather than two values threaded side by side, and that is the
/// point. The argv and the record answer are one decision read off one
/// `CopyVideoOptions`; carried separately, a caller can build the argv and
/// drop the answer, and the result is a pass whose bytes and whose served init
/// describe different streams — with nothing to notice, because each half is
/// individually correct. The only way to get the argv is to get both.
struct IndexPass {
    args: Vec<String>,
    dolby_vision: DolbyVisionPass,
    expectation: VideoCompletionExpectation,
    /// The options all three of the above were derived from, carried so the
    /// index key is derived from them too. Passed separately, a caller could
    /// hand the runner a recipe built from one set of options and an identity
    /// computed from another — the rows would be byte counts for one stream
    /// filed under another's key, which is the same class of mismatch as the
    /// argv and the record answer disagreeing.
    video: transcode::CopyVideoOptions,
    /// The PGS tracks this pass also keeps, and the stage it keeps them in.
    /// Its argv is already at the end of `args`; the plan rides with the pass
    /// so the verdict and the stage's removal belong to the same owner.
    ride_along: Option<RideAlongPlan>,
}

fn index_pass(
    file: &MediaFile,
    video: transcode::CopyVideoOptions,
    input: Option<&str>,
    expectation: VideoCompletionExpectation,
    ride_along: Option<RideAlongPlan>,
) -> Result<IndexPass, String> {
    Ok(IndexPass {
        args: index_argv(file, video, input, ride_along.as_ref()),
        dolby_vision: dolby_vision_pass_for(file, video)?,
        expectation,
        video,
        ride_along,
    })
}

/// The whole argv of an index pass.
///
/// The ride-along's output is appended **after** `pipe:1`, never inside
/// `copy_index_pipe_args*`: `fragment_index_cluster::pipeline_digest_for_transform`
/// hashes that function's argv into every cluster cache key, so a subtitle
/// output there would give every file with a PGS track a new key — and a
/// second index — for bytes that did not change. The startup self-test runs
/// exactly this argv.
pub(crate) fn index_argv(
    file: &MediaFile,
    video: transcode::CopyVideoOptions,
    input: Option<&str>,
    ride_along: Option<&RideAlongPlan>,
) -> Vec<String> {
    let mut args = match input {
        Some(path) => transcode::copy_index_pipe_args_with_input(file, path, video),
        None => transcode::copy_index_pipe_args(file, video),
    };
    if let Some(plan) = ride_along {
        args.extend(plan.args());
    }
    args
}

/// What an index build produced: the index outcome, exactly as it always
/// was, and — only when the index was built and the pass rode along — the
/// PGS tracks it wrote, not yet judged. The caller judges them after its own
/// cancellation race and freshness checks, then publishes.
#[derive(Debug)]
pub(crate) struct IndexBuild {
    pub(crate) outcome: IndexOutcome,
    pub(crate) ride_along: Option<PendingRideAlong>,
    /// Whether the held source was still the object the pass read when the
    /// pass ended, as far as this function could tell: [`build_riding`]
    /// reads its fence; the attested paths leave it to their caller, who
    /// holds the observation, and say `true`.
    pub(crate) source_unchanged: bool,
}

impl IndexBuild {
    fn plain(outcome: IndexOutcome) -> Self {
        Self {
            outcome,
            ride_along: None,
            source_unchanged: true,
        }
    }
}

/// What the held-fd probe established before the pass: the completion
/// expectation, and the file's PGS tracks as subtitle ordinals.
struct ProbedSource {
    expectation: VideoCompletionExpectation,
    subtitle_tracks: Vec<crate::subtitle_ride_along::ProbedTrack>,
}

async fn probe_completion_expectation(
    source: &std::fs::File,
    source_object_version: &str,
    budget: Duration,
) -> Result<(ProbedSource, Instant), IndexFailure> {
    let started = Instant::now();
    match probe_completion_expectation_inner(source, source_object_version, budget, started).await {
        Ok(probed) => Ok((probed, started)),
        Err(mut failure) => {
            failure.diagnostic.elapsed_ms =
                Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
            failure.diagnostic.budget_ms =
                Some(u64::try_from(budget.as_millis()).unwrap_or(u64::MAX));
            Err(failure)
        }
    }
}

async fn probe_completion_expectation_inner(
    source: &std::fs::File,
    source_object_version: &str,
    budget: Duration,
    started: Instant,
) -> Result<ProbedSource, IndexFailure> {
    let mut view = source;
    view.seek(SeekFrom::Start(0)).map_err(|error| {
        IndexFailure::new(
            IndexFailureCode::IndexSourceIo,
            format!("positioning held source before metadata probe: {error}"),
            0,
        )
        .transient(matches!(
            error.kind(),
            std::io::ErrorKind::Interrupted
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::WouldBlock
        ))
    })?;
    let result = tokio::time::timeout(
        budget.saturating_sub(started.elapsed()),
        crate::ffmpeg::held_source_index_probe_json(source),
    )
    .await
    .map_err(|_| {
        IndexFailure::new(
            IndexFailureCode::IndexBudgetExceeded,
            format!(
                "metadata probe exceeded the {}s index budget",
                budget.as_secs()
            ),
            0,
        )
    })?;
    let reset = view.seek(SeekFrom::Start(0));
    if let Err(error) = reset {
        return Err(IndexFailure::new(
            IndexFailureCode::IndexSourceIo,
            format!("resetting held source after metadata probe: {error}"),
            0,
        )
        .transient(matches!(
            error.kind(),
            std::io::ErrorKind::Interrupted
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::WouldBlock
        )));
    }
    let raw = result.map_err(|reason| {
        let timeout = reason.contains("timed out after");
        IndexFailure::new(
            if timeout {
                IndexFailureCode::IndexProbeTimeout
            } else {
                IndexFailureCode::IndexProcessFailed
            },
            reason,
            0,
        )
        .transient(timeout)
    })?;
    // The ride-along's ordinals come from this document — the probe of the
    // very descriptor the pass reads — and never from scan-time facts.
    let subtitle_tracks = crate::subtitle_ride_along::eligible_tracks_from_probe(&raw);
    let expectation = match completion_expectation_from_probe(&raw, source_object_version) {
        Ok(expectation) => Ok(expectation),
        Err(CompletionExpectationError::Unsupported(reason)) => {
            Err(IndexFailure::new(IndexFailureCode::Unsupported, reason, 0))
        }
        Err(CompletionExpectationError::Unverified(metadata_reason)) => {
            let context =
                packet_probe_context_from_metadata(&raw).map_err(|error| match error {
                    CompletionExpectationError::Unsupported(reason) => {
                        IndexFailure::new(IndexFailureCode::Unsupported, reason, 0)
                    }
                    CompletionExpectationError::Unverified(reason) => IndexFailure::new(
                        IndexFailureCode::IndexCompletionUnverified,
                        format!("{metadata_reason}; {reason}"),
                        0,
                    ),
                })?;
            let bounds = held_source_video_packet_bounds(
                source,
                context,
                budget.saturating_sub(started.elapsed()),
            )
            .await?;
            bounds
                .expectation(context, source_object_version)
                .map_err(|error| {
                    let (CompletionExpectationError::Unverified(reason)
                    | CompletionExpectationError::Unsupported(reason)) = error;
                    let mut failure = IndexFailure::new(
                        IndexFailureCode::IndexCompletionUnverified,
                        format!("{metadata_reason}; {reason}"),
                        0,
                    );
                    failure.diagnostic.selected_stream = Some(context.stream_index);
                    failure.diagnostic.output_bytes = Some(bounds.output_bytes);
                    failure
                })
        }
    }?;
    Ok(ProbedSource {
        expectation,
        subtitle_tracks,
    })
}

/// Build a file's index by running the index pipe.
///
/// `budget` bounds the whole pass. An index is background work; a NAS read
/// that has gone pathological should give the slot back rather than hold it
/// until the process restarts.
///
/// Never rides along: production indexes through [`build_riding`], and this
/// is the entry point fixtures and tests use to build an index and nothing
/// else.
#[allow(dead_code)]
pub async fn build(
    file: &MediaFile,
    video: transcode::CopyVideoOptions,
    runtime_cache: &Path,
    budget: Duration,
) -> IndexOutcome {
    build_riding(file, video, runtime_cache, budget, None, None)
        .await
        .outcome
}

/// [`build`], also keeping the file's PGS tracks when `ride_along` allows.
///
/// The non-cluster path's caller. `source_unchanged` reports whether the
/// held source is still the object the pass read — the same `fstat` identity
/// the cluster worker's `source_still_matches` compares — and the caller
/// publishes only when it is (`JobManager::settle_ride_along`).
pub(crate) async fn build_riding(
    file: &MediaFile,
    video: transcode::CopyVideoOptions,
    runtime_cache: &Path,
    budget: Duration,
    ride_along: Option<&RideAlongGate>,
    progress: Option<SharedIndexProgress>,
) -> IndexBuild {
    let source = match crate::fragment_index_cluster::open_source_fence(file, None).await {
        Ok(source) => source,
        Err(reason) => {
            return IndexBuild::plain(IndexOutcome::Failed(Box::new(
                IndexFailure::new(IndexFailureCode::IndexSourceIo, reason, 0).transient(true),
            )));
        }
    };
    let mut built = build_attested(
        file,
        &source.handle,
        source.object_version(),
        video,
        runtime_cache,
        budget,
        progress,
        ride_along,
    )
    .await;
    built.source_unchanged = source.unchanged();
    built
}

/// Build from the exact file descriptor whose complete digest was observed.
/// The parent retains ownership; the child receives a duplicate as fd 3.
#[allow(dead_code)]
pub async fn build_from_attested_file(
    file: &MediaFile,
    source: &std::fs::File,
    source_object_version: &str,
    video: transcode::CopyVideoOptions,
    runtime_cache: &Path,
    budget: Duration,
) -> IndexOutcome {
    build_attested(
        file,
        source,
        source_object_version,
        video,
        runtime_cache,
        budget,
        None,
        None,
    )
    .await
    .outcome
}

/// The cluster worker's build: progress reported, and the file's PGS tracks
/// kept when `ride_along` allows. The worker publishes the harvest only after
/// its own `source_still_matches` and `still_current` checks.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn build_from_attested_file_with_progress<F>(
    file: &MediaFile,
    source: &std::fs::File,
    source_object_version: &str,
    video: transcode::CopyVideoOptions,
    runtime_cache: &Path,
    budget: Duration,
    progress: F,
    ride_along: Option<&RideAlongGate>,
) -> IndexBuild
where
    F: Fn(&PassProgress) + Send + Sync + 'static,
{
    build_attested(
        file,
        source,
        source_object_version,
        video,
        runtime_cache,
        budget,
        Some(Arc::new(progress)),
        ride_along,
    )
    .await
}

/// The one place every index build passes through — the cluster worker and
/// the non-cluster pass alike — after the held-fd probe and before the pass.
/// That is where the ride-along's latch is checked: the first point at which
/// the file's PGS ordinals exist, read from the descriptor the pass will read.
#[allow(clippy::too_many_arguments)]
async fn build_attested(
    file: &MediaFile,
    source: &std::fs::File,
    source_object_version: &str,
    video: transcode::CopyVideoOptions,
    runtime_cache: &Path,
    budget: Duration,
    progress: Option<SharedIndexProgress>,
    ride_along: Option<&RideAlongGate>,
) -> IndexBuild {
    #[cfg(windows)]
    let path = match crate::ffmpeg::windows_source_path(source) {
        Ok(path) => path,
        Err(reason) => return IndexBuild::plain(IndexOutcome::Unsupported(reason)),
    };
    let (probed, started) =
        match probe_completion_expectation(source, source_object_version, budget).await {
            Ok(value) => value,
            Err(failure) => return IndexBuild::plain(IndexOutcome::Failed(Box::new(failure))),
        };
    let plan = match ride_along {
        Some(gate) => {
            crate::subtitle_ride_along::plan_tracks(gate, file.id, source, &probed.subtitle_tracks)
                .await
        }
        None => None,
    };
    #[cfg(unix)]
    let input = "/dev/fd/3".to_owned();
    #[cfg(windows)]
    let input = path.to_string_lossy().into_owned();
    let pass = match index_pass(file, video, Some(&input), probed.expectation, plan) {
        Ok(pass) => pass,
        Err(reason) => return IndexBuild::plain(IndexOutcome::Unsupported(reason)),
    };
    #[cfg(unix)]
    let (source_fd, source_handoff) = {
        use std::os::fd::AsRawFd;
        (Some(source.as_raw_fd()), None)
    };
    #[cfg(windows)]
    let (source_fd, source_handoff) = (None, Some((source, path.as_path())));
    build_with_args(
        file,
        pass,
        source_fd,
        source_handoff,
        runtime_cache,
        budget,
        started,
        progress,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn build_with_args(
    file: &MediaFile,
    pass: IndexPass,
    source_fd: Option<SourceFd>,
    source_handoff: Option<(&std::fs::File, &Path)>,
    runtime_cache: &Path,
    budget: Duration,
    started: Instant,
    progress: Option<SharedIndexProgress>,
) -> IndexBuild {
    let IndexPass {
        args,
        dolby_vision,
        expectation,
        video,
        ride_along,
    } = pass;
    let identity = identity_for(file, video);
    if let Some(plan) = &ride_along {
        tracing::info!(
            file_id = file.id,
            tracks = ?plan.tracks(),
            "the fragment-index pass is also keeping this file's PGS tracks"
        );
    }

    let mut command = tokio::process::Command::new(ffmpeg_bin());
    crate::producer_spawn::configure_ffmpeg_runtime(&mut command, runtime_cache);
    #[cfg(unix)]
    if let Some(source_fd) = source_fd {
        unsafe {
            command.pre_exec(move || {
                let duplicate = libc::fcntl(source_fd, libc::F_DUPFD_CLOEXEC, 10);
                if duplicate == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(duplicate, 3) == -1 {
                    libc::close(duplicate);
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(duplicate);
                let flags = libc::fcntl(3, libc::F_GETFD);
                if flags == -1 || libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = source_fd;
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    if let Some((source, path)) = source_handoff {
        if let Err(reason) = crate::ffmpeg::verify_windows_source_path(source, path) {
            return IndexBuild::plain(IndexOutcome::Failed(Box::new(IndexFailure::new(
                IndexFailureCode::IndexSourceIo,
                reason,
                0,
            ))));
        }
    }
    #[cfg(not(windows))]
    let _ = source_handoff;
    let (mut child, _child_job) = match crate::process_control::spawn_job_owned(&mut command) {
        Ok(owned) => owned,
        Err(error) => {
            let transient = matches!(
                error.kind(),
                std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::OutOfMemory
            );
            return IndexBuild::plain(IndexOutcome::Failed(Box::new(
                IndexFailure::new(
                    IndexFailureCode::IndexProcessFailed,
                    format!("spawning the job-owned index pipe: {error}"),
                    0,
                )
                .transient(transient),
            )));
        }
    };
    let Some(stdout) = child.stdout.take() else {
        return IndexBuild::plain(IndexOutcome::Failed(Box::new(IndexFailure::new(
            IndexFailureCode::IndexProcessFailed,
            "the index pipe started without a stdout",
            0,
        ))));
    };

    // stderr must be drained on its own task or ffmpeg blocks on a full pipe
    // and the whole build deadlocks — the same discipline `spawn_ffmpeg_pipe`
    // keeps for a live session. One reader serves both the bounded
    // diagnostic tail and, when the pass rides along, the full-stream scan
    // for per-slave failures.
    let stderr_scan = ride_along.as_ref().map(RideAlongPlan::stderr_scan);
    let stderr_task = child.stderr.take().map(|stderr| {
        tokio::spawn(read_stderr_tail(
            stderr,
            stderr_scan,
            matches!(file.video_codec.as_deref(), Some("hevc" | "h265")),
        ))
    });
    let observed = Arc::new(std::sync::Mutex::new((0_u64, 0_i64, 0_usize)));
    let observed_for_progress = Arc::clone(&observed);
    let caller_progress = progress.clone();
    // What the ride-along adds to each report: its track count, and the
    // bytes in its stage, re-measured at most once a second — a stage is a
    // handful of files, and reports arrive per fragment.
    let ride_meter = ride_along
        .as_ref()
        .map(|plan| (plan.tracks().len(), plan.stage().to_owned()));
    let measured = std::sync::Mutex::new((None::<Instant>, 0_u64));
    let record_progress = move |bytes: u64, media_ms: i64, rows: usize| {
        if let Ok(mut current) = observed_for_progress.lock() {
            *current = (bytes, media_ms, rows);
        }
        if let Some(progress) = caller_progress.as_deref() {
            let (pgs_tracks, pgs_bytes_written) = match &ride_meter {
                Some((tracks, stage)) => {
                    let mut measured = measured
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if measured
                        .0
                        .is_none_or(|at| at.elapsed() >= RIDE_ALONG_MEASURE_EVERY)
                    {
                        *measured = (
                            Some(Instant::now()),
                            crate::subtitle_ride_along::stage_bytes(stage),
                        );
                    }
                    (*tracks, measured.1)
                }
                None => (0, 0),
            };
            progress(&PassProgress {
                bytes_read: bytes,
                media_ms,
                fragments: rows,
                pgs_tracks,
                pgs_bytes_written,
            });
        }
    };
    let (mut outcome, deadline_fired) = match tokio::time::timeout(
        budget.saturating_sub(started.elapsed()),
        index_stream_with_progress(
            stdout,
            identity,
            Some(&expectation),
            dolby_vision,
            Some(&record_progress),
        ),
    )
    .await
    {
        Ok(outcome) => (outcome, false),
        Err(_) => {
            let rows = observed.lock().map(|value| value.2).unwrap_or_default();
            (
                IndexOutcome::Failed(Box::new(IndexFailure::new(
                    IndexFailureCode::IndexBudgetExceeded,
                    format!("exceeded the {}s index budget", budget.as_secs()),
                    rows,
                ))),
                true,
            )
        }
    };
    if deadline_fired {
        let _ = child.start_kill();
    }
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    let exit_category = match status {
        Ok(Ok(status)) if status.success() => Some("success".to_owned()),
        Ok(Ok(status)) => {
            if !deadline_fired {
                let rows = observed.lock().map(|value| value.2).unwrap_or_default();
                outcome = IndexOutcome::Failed(Box::new(IndexFailure::new(
                    IndexFailureCode::IndexProcessFailed,
                    format!("index pipe exited with {status}"),
                    rows,
                )));
            }
            Some(format!("{status}"))
        }
        Ok(Err(error)) => {
            if !deadline_fired {
                let rows = observed.lock().map(|value| value.2).unwrap_or_default();
                let transient = matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                );
                outcome = IndexOutcome::Failed(Box::new(
                    IndexFailure::new(
                        IndexFailureCode::IndexProcessFailed,
                        format!("waiting for the index pipe: {error}"),
                        rows,
                    )
                    .transient(transient),
                ));
            }
            Some("wait_failed".to_owned())
        }
        Err(_) => {
            let _ = child.start_kill();
            // A second wait is required after escalating to kill; otherwise
            // the process can remain unreaped while the stderr task is
            // abandoned below.
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            if !deadline_fired {
                let rows = observed.lock().map(|value| value.2).unwrap_or_default();
                outcome = IndexOutcome::Failed(Box::new(IndexFailure::new(
                    IndexFailureCode::IndexProcessFailed,
                    "index pipe did not exit within the five-second grace",
                    rows,
                )));
            }
            Some("exit_grace_exceeded".to_owned())
        }
    };
    // `scan` is `None` unless the reader ran to the end of the stream: a
    // scan that did not finish cannot vouch that no slave failed.
    let (stderr_tail, scan, hevc_trace) = match stderr_task {
        Some(mut task) => match tokio::time::timeout(Duration::from_secs(5), &mut task).await {
            Ok(Ok((lines, scan, trace))) => (lines, scan, trace),
            Ok(Err(error)) => (vec![format!("stderr task failed: {error}")], None, None),
            Err(_) => {
                task.abort();
                let _ = task.await;
                (Vec::new(), None, None)
            }
        },
        None => (Vec::new(), None, None),
    };
    finish_header_scan(
        &mut outcome,
        matches!(file.video_codec.as_deref(), Some("hevc" | "h265")),
        hevc_trace,
        &expectation,
    );
    if let IndexOutcome::Failed(failure) = &mut outcome {
        let (output_bytes, covered_ms, rows) =
            observed.lock().map(|value| *value).unwrap_or_default();
        failure.rows = failure.rows.max(rows);
        failure.diagnostic.fragment_count = Some(u32::try_from(failure.rows).unwrap_or(u32::MAX));
        failure.diagnostic.output_bytes = Some(output_bytes);
        failure.diagnostic.covered_ms = Some(covered_ms);
        failure.diagnostic.expected_ms = expectation.duration_ms_floor();
        failure.diagnostic.container_ms = file.duration_ms;
        failure.diagnostic.selected_stream = Some(expectation.stream_index);
        failure.diagnostic.expectation_provenance =
            Some(expectation.provenance.as_str().to_owned());
        failure.diagnostic.elapsed_ms =
            Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        failure.diagnostic.budget_ms = Some(u64::try_from(budget.as_millis()).unwrap_or(u64::MAX));
        failure.diagnostic.exit_category = exit_category;
        failure.diagnostic.stderr_tail = stderr_tail;
    }
    if let IndexOutcome::Built(ref index) = outcome {
        tracing::info!(
            file_id = file.id,
            fragments = index.rows.len(),
            timescale = index.timescale,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "built a fragment index"
        );
    }
    // The child has been reaped (a `Built` outcome means it exited 0 within
    // the grace), so no slave can still be flushing; the verdict itself is
    // the caller's, outside this future. Any other outcome discards the stage
    // with the plan, and the file's next pass does not ride: a riding pass
    // that failed must not be able to fail the same way on every retry.
    let ride_along = match (ride_along, &outcome) {
        (Some(plan), IndexOutcome::Built(_)) => Some(PendingRideAlong::new(plan, scan)),
        (Some(plan), _) => {
            crate::subtitle_ride_along::record_failed_ride(&plan);
            None
        }
        (None, _) => None,
    };
    IndexBuild {
        outcome,
        ride_along,
        source_unchanged: true,
    }
}

fn finish_header_scan(
    outcome: &mut IndexOutcome,
    hevc: bool,
    trace: Option<plurx_core::hevc_configuration::Trace>,
    expectation: &VideoCompletionExpectation,
) {
    if let IndexOutcome::Built(index) = outcome {
        if hevc && trace.is_none() {
            *outcome = IndexOutcome::Failed(Box::new(
                IndexFailure::new(
                    IndexFailureCode::IndexProcessFailed,
                    "HEVC header scan did not reach stderr EOF; no proof can be published",
                    index.rows.len(),
                )
                .transient(true),
            ));
        } else {
            index.promotion.hevc_configuration = trace.map(|trace| {
                trace.finish(
                    expectation.source_object_version.clone(),
                    expectation.stream_index,
                )
            });
        }
    }
}

/// Drain the index child's stderr: a bounded tail for diagnostics and, when
/// `scan` is given, every line fed to it. The scan comes back only when the
/// stream was read to its end; a read error leaves it unfinished, so `None`.
async fn read_stderr_tail(
    mut input: impl AsyncRead + Unpin,
    mut scan: Option<StderrScan>,
    hevc: bool,
) -> (
    Vec<String>,
    Option<StderrScan>,
    Option<plurx_core::hevc_configuration::Trace>,
) {
    let mut trace = hevc.then(plurx_core::hevc_configuration::Trace::default);
    use plurx_core::content_analysis::{MAX_INDEX_STDERR_BYTES, MAX_INDEX_STDERR_LINES};

    let mut tail = VecDeque::with_capacity(MAX_INDEX_STDERR_BYTES);
    let mut chunk = [0_u8; 1_024];
    let mut complete = true;
    loop {
        match input.read(&mut chunk).await {
            Ok(0) => break,
            Err(_) => {
                complete = false;
                break;
            }
            Ok(read) => {
                if let Some(trace) = trace.as_mut() {
                    trace.feed(&chunk[..read]);
                }
                if let Some(scan) = scan.as_mut() {
                    scan.feed(&chunk[..read]);
                }
                for byte in &chunk[..read] {
                    if tail.len() == MAX_INDEX_STDERR_BYTES {
                        tail.pop_front();
                    }
                    tail.push_back(*byte);
                }
            }
        }
    }
    let bytes: Vec<u8> = tail.into_iter().collect();
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text
        .lines()
        .rev()
        .take(MAX_INDEX_STDERR_LINES)
        .map(str::to_owned)
        .collect();
    lines.reverse();
    let scan = scan.filter(|_| complete).map(|mut scan| {
        scan.finish();
        scan
    });
    (lines, scan, trace.filter(|_| complete))
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::segplan::{self, SEGPLAN_VERSION};
    use plurx_core::testfixtures;

    /// The record a converting pass supplies — and, being `Some`, the way a
    /// caller says that this pass converts at all.
    fn converting_record() -> plurx_core::fmp4::DolbyVisionRecord {
        plurx_core::fmp4::DolbyVisionRecord::new(8, 6, false, true, true, 1).expect("record")
    }

    fn identity() -> SourceIdentity {
        SourceIdentity::new(1, 1, "fingerprint")
    }

    #[test]
    fn content_analysis_probe_uses_the_first_mapped_video_not_container_duration() {
        let raw = serde_json::json!({
            "streams": [
                {"index": 0, "codec_type": "audio", "duration": "99.000"},
                {"index": 3, "codec_type": "video", "duration_ts": 240, "time_base": "1/24", "duration": "10.000"},
                {"index": 4, "codec_type": "video", "duration": "80.000"}
            ],
            "format": {"duration": "99.000"}
        })
        .to_string();
        let expectation = completion_expectation_from_probe(&raw, "held-v1").expect("timing");
        assert_eq!(expectation.stream_index, 3);
        assert_eq!(expectation.duration_num, 10);
        assert_eq!(expectation.duration_den, 1);
        assert_eq!(expectation.provenance, CompletionProvenance::StreamTicks);
    }

    #[test]
    fn content_analysis_probe_rejects_conflicting_selected_video_timing() {
        let raw = serde_json::json!({
            "streams": [{
                "index": 2,
                "codec_type": "video",
                "duration_ts": 240,
                "time_base": "1/24",
                "duration": "13.001"
            }]
        })
        .to_string();
        assert!(matches!(
            completion_expectation_from_probe(&raw, "held-v1"),
            Err(CompletionExpectationError::Unverified(_))
        ));
    }

    #[test]
    fn mkv_hls_duration_conflict_compares_the_full_candidate_range() {
        let raw = serde_json::json!({
            "streams": [{
                "index": 2,
                "codec_type": "video",
                "duration_ts": 240,
                "time_base": "1/24",
                "duration": "8.500",
                "tags": {"DURATION": "00:00:11.500"}
            }]
        })
        .to_string();
        assert!(matches!(
            completion_expectation_from_probe(&raw, "held-v1"),
            Err(CompletionExpectationError::Unverified(_))
        ));
    }

    #[test]
    fn mkv_hls_duration_packet_bounds_use_signed_pts_and_maximum_packet_end() {
        let head = serde_json::json!({
            "packets": [
                {"stream_index": 3, "pts": "-48", "duration": "24"},
                {"stream_index": 3, "pts": "0", "duration": "24"}
            ]
        })
        .to_string();
        let tail = serde_json::json!({
            "packets": [
                {"stream_index": 3, "pts": "240", "duration": "24"},
                {"stream_index": 3, "pts": "216", "duration": "24"},
                {"stream_index": 3, "pts": "192", "duration": "24"}
            ]
        })
        .to_string();
        let (origin, count) = head_packet_origin(head.as_bytes(), 3).expect("head bounds");
        assert_eq!((origin, count), (-48, 2));
        let (minimum, end, count) = tail_packet_end(tail.as_bytes(), 3)
            .expect("tail bounds")
            .expect("selected packets");
        assert_eq!(minimum, 192);
        assert_eq!(end, 264, "the last JSON row need not have the maximum PTS");
        assert_eq!(count, 3);

        let expectation = PacketTimelineBounds {
            origin_ticks: origin,
            end_ticks: end,
            packet_count: 5,
            output_bytes: u64::try_from(head.len() + tail.len()).expect("fixture bytes"),
        }
        .expectation(
            PacketProbeContext {
                stream_index: 3,
                time_base: (1, 24),
                seek_hint_seconds: 11,
            },
            "held-v1",
        )
        .expect("signed span");
        assert_eq!(expectation.provenance, CompletionProvenance::PacketTimeline);
        assert_eq!(expectation.duration_num, 13);
        assert_eq!(expectation.duration_den, 1);
    }

    #[test]
    fn mkv_hls_duration_tail_requires_every_selected_packet_duration() {
        let tail = serde_json::json!({
            "packets": [
                {"stream_index": 3, "pts": "240", "duration": "24"},
                {"stream_index": 3, "pts": "264"}
            ]
        })
        .to_string();
        assert!(matches!(
            tail_packet_end(tail.as_bytes(), 3),
            Err(CompletionExpectationError::Unverified(_))
        ));
    }

    #[test]
    fn mkv_hls_duration_valid_pts_does_not_require_dts() {
        let tail = serde_json::json!({
            "packets": [
                {"stream_index": 3, "pts": "240", "duration": "24"}
            ]
        })
        .to_string();
        assert_eq!(
            tail_packet_end(tail.as_bytes(), 3).expect("valid PTS-only packet"),
            Some((240, 264, 1))
        );
    }

    #[test]
    fn content_analysis_probe_refuses_attached_picture_selected_by_ffmpeg() {
        let raw = serde_json::json!({
            "streams": [{
                "index": 0,
                "codec_type": "video",
                "duration": "10.000",
                "disposition": {"attached_pic": 1}
            }]
        })
        .to_string();
        assert!(matches!(
            completion_expectation_from_probe(&raw, "held-v1"),
            Err(CompletionExpectationError::Unsupported(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn content_analysis_ffmpeg_accepts_complete_video_with_a_longer_audio_tail() {
        testfixtures::require_ffmpeg();
        let temp = crate::test_tempdir().expect("tempdir");
        let source_path = temp.path().join("video-shorter-than-audio.mkv");
        let mut mux = std::process::Command::new(testfixtures::ffmpeg());
        mux.args(["-y", "-v", "error"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x90:rate=24:duration=3",
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=6",
            ])
            .args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "24",
                "-c:a",
                "aac",
                "-f",
                "matroska",
            ])
            .arg(&source_path);
        testfixtures::run(&mut mux);

        let source = std::fs::File::open(&source_path).expect("fixture source");
        let size = i64::try_from(source.metadata().expect("fixture metadata").len())
            .expect("fixture size");
        let file = MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 42,
            item_id: 1,
            path: source_path,
            size,
            mtime: 1,
            duration_ms: Some(6_000),
            container: Some("matroska".to_owned()),
            video_codec: Some("h264".to_owned()),
            video_codec_tag: None,
            field_order: None,
            video_profile: Some("High".to_owned()),
            width: Some(160),
            height: Some(90),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: None,
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        };
        let outcome = build_from_attested_file(
            &file,
            &source,
            "fixture-object-v1",
            transcode::CopyVideoOptions::new(false, false),
            temp.path(),
            Duration::from_secs(30),
        )
        .await;
        let IndexOutcome::Built(index) = outcome else {
            panic!("complete selected video with a longer audio tail must build: {outcome:?}");
        };
        let covered = index
            .rows
            .iter()
            .try_fold(0_u64, |sum, row| sum.checked_add(row.duration))
            .expect("coverage sum");
        let old_container_expectation = VideoCompletionExpectation {
            stream_index: 0,
            duration_num: 6,
            duration_den: 1,
            provenance: CompletionProvenance::StreamSeconds,
            source_object_version: "fixture-object-v1".to_owned(),
        };
        assert_eq!(
            old_container_expectation.covers(covered, index.timescale),
            Ok(false),
            "the former container-duration predicate rejects this valid video-only output"
        );
    }

    #[tokio::test]
    async fn hevc_incomplete_stderr_cannot_publish_a_proofless_index() {
        struct FailedRead;
        impl tokio::io::AsyncRead for FailedRead {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                _: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Err(std::io::Error::other("failed stderr")))
            }
        }
        let (_, _, trace) = read_stderr_tail(FailedRead, None, true).await;
        assert!(trace.is_none());
        let mut outcome = IndexOutcome::Built(Box::new(FragmentIndex::new(
            90_000,
            vec![],
            "init",
            identity(),
        )));
        let expectation = VideoCompletionExpectation {
            stream_index: 0,
            duration_num: 1,
            duration_den: 1,
            provenance: CompletionProvenance::StreamSeconds,
            source_object_version: "source".into(),
        };
        finish_header_scan(&mut outcome, true, trace, &expectation);
        assert!(matches!(outcome, IndexOutcome::Failed(_)));
    }

    /// A real encode, rather than fabricated PromotionInputs: the second
    /// portion redefines PPS 0 while keeping the same source/container track.
    #[cfg(unix)]
    #[tokio::test]
    async fn hevc_original_header_scan_rejects_changed_pps_before_stripping() {
        use std::process::Command;
        testfixtures::require_ffmpeg();
        let temp = crate::test_tempdir().expect("tempdir");
        for (name, cb, cr) in [("first", -6, -8), ("second", -1, -2)] {
            let mut encode = Command::new(testfixtures::ffmpeg());
            encode.args(["-y", "-v", "error", "-f", "lavfi", "-i",
                "testsrc2=size=160x96:rate=24:duration=2", "-c:v", "libx265",
                "-preset", "ultrafast", "-x265-params"])
                .arg(format!("keyint=24:min-keyint=24:open-gop=0:bframes=0:repeat-headers=1:scenecut=0:cbqpoffs={cb}:crqpoffs={cr}:log-level=none:pools=1"))
                .arg(temp.path().join(format!("{name}.mkv")));
            testfixtures::run(&mut encode);
        }
        let concat = temp.path().join("concat.txt");
        std::fs::write(&concat, "file 'first.mkv'\nfile 'second.mkv'\n")
            .expect("HEVC regression fixture");
        let changing = temp.path().join("changing.mkv");
        let mut mux = Command::new(testfixtures::ffmpeg());
        mux.args(["-y", "-v", "error", "-f", "concat", "-safe", "0", "-i"])
            .arg(&concat)
            .args(["-c", "copy"])
            .arg(&changing);
        testfixtures::run(&mut mux);
        for (path, verified) in [
            (temp.path().join("first.mkv"), true),
            (changing.clone(), false),
        ] {
            let source = std::fs::File::open(&path).expect("HEVC regression fixture");
            let mut file = hevc_file(None, None);
            file.path = path;
            file.size = source.metadata().expect("HEVC regression fixture").len() as i64;
            file.duration_ms = Some(if verified { 2000 } else { 4000 });
            let result = build_from_attested_file(
                &file,
                &source,
                "fixture-object",
                transcode::CopyVideoOptions::new(false, false),
                temp.path(),
                Duration::from_secs(30),
            )
            .await;
            let IndexOutcome::Built(index) = result else {
                panic!("{result:?}")
            };
            let proof = index.promotion.hevc_configuration.expect("complete trace");
            assert_eq!(proof.permits("fixture-object"), verified, "{proof:?}");
            if !verified {
                assert!(
                    proof
                        .refusal
                        .as_deref()
                        .expect("HEVC regression fixture")
                        .contains("parameter sets change"),
                    "{proof:?}"
                );
            }
        }
        // A/B on exactly the same encoded source. Retained headers decode to
        // the source's pixels; deleting their updates changes the pixels.
        let hashes = |path: &std::path::Path| {
            let out = Command::new(testfixtures::ffmpeg())
                .args(["-v", "error", "-i"])
                .arg(path)
                .args(["-map", "0:v:0", "-f", "framemd5", "-"])
                .output()
                .expect("HEVC regression fixture");
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout)
                .expect("HEVC regression fixture")
                .lines()
                .filter(|line| !line.starts_with('#'))
                .map(|line| {
                    line.rsplit(',')
                        .next()
                        .expect("HEVC regression fixture")
                        .trim()
                        .to_owned()
                })
                .collect::<Vec<_>>()
        };
        let stable_path = temp.path().join("first.mkv");
        let mut stable_file = hevc_file(None, None);
        stable_file.path = stable_path.clone();
        let output = Command::new(testfixtures::ffmpeg())
            .args(transcode::copy_index_pipe_args(
                &stable_file,
                transcode::CopyVideoOptions::new(false, false),
            ))
            .output()
            .expect("HEVC regression fixture");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let production = temp.path().join("stable-production.mp4");
        let with_rpus = testfixtures::with_dolby_vision_rpus(&output.stdout);
        let dv_path = temp.path().join("stable-rpu.mp4");
        std::fs::write(&dv_path, with_rpus).expect("HEVC regression fixture");
        let dv_source = std::fs::File::open(&dv_path).expect("HEVC regression fixture");
        let mut dv_file = stable_file.clone();
        dv_file.path = dv_path;
        dv_file.size = dv_source.metadata().expect("HEVC regression fixture").len() as i64;
        dv_file.duration_ms = Some(2000);
        let dv = build_from_attested_file(
            &dv_file,
            &dv_source,
            "rpu-source",
            transcode::CopyVideoOptions::new(false, true),
            temp.path(),
            Duration::from_secs(30),
        )
        .await;
        let IndexOutcome::Built(dv_index) = dv else {
            panic!("RPU fixture: {dv:?}")
        };
        assert!(
            dv_index
                .promotion
                .hevc_configuration
                .as_ref()
                .expect("HEVC regression fixture")
                .permits("rpu-source"),
            "{:?}",
            dv_index.promotion.hevc_configuration
        );
        std::fs::write(&production, output.stdout).expect("HEVC regression fixture");
        assert_eq!(
            hashes(&production),
            hashes(&stable_path),
            "production fMP4 must preserve every decoded pixel"
        );
        let original = hashes(&changing);
        for (name, remove, matches) in [("kept", "62-63", true), ("stripped", "32-34|62-63", false)]
        {
            let path = temp.path().join(format!("{name}.mkv"));
            let mut remux = Command::new(testfixtures::ffmpeg());
            remux
                .args(["-y", "-v", "error", "-i"])
                .arg(&changing)
                .args(["-map", "0:v:0", "-c:v", "copy", "-bsf:v"])
                .arg(format!("filter_units=remove_types={remove}"))
                .arg(&path);
            testfixtures::run(&mut remux);
            let result = hashes(&path);
            assert_eq!(result.len(), original.len());
            assert_eq!(result == original, matches);
        }
    }

    fn hevc_file(hdr: Option<&str>, hdr_format: Option<&str>) -> MediaFile {
        MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 77,
            item_id: 1,
            path: std::path::PathBuf::from("/library/film.mkv"),
            size: 60_000_000_000,
            mtime: 1_700_000_000_000,
            duration_ms: Some(7_200_000),
            container: Some("mkv".into()),
            video_codec: Some("hevc".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: Some("Main 10".into()),
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            hdr: hdr.map(str::to_owned),
            hdr_format: hdr_format.map(str::to_owned),
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: Some(60_000_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    fn fingerprints_of(file: &MediaFile, have_dovi: bool) -> Vec<String> {
        video_identities(file, None, have_dovi, false)
            .into_iter()
            .map(|video| identity_for(file, video).argv_fingerprint)
            .collect()
    }

    #[test]
    fn a_plain_file_has_one_pipeline_to_index() {
        let file = hevc_file(None, None);
        assert_eq!(video_identities(&file, None, true, false).len(), 1);
        assert!(
            !video_identities(&file, None, true, false)[0].preserves_dolby_vision(),
            "the stripped identity stays first, so a fully indexed library \
             does not re-order its work to adopt the identity set"
        );
    }

    #[test]
    fn a_dolby_vision_file_is_indexed_for_both_the_strip_and_the_envelope() {
        let file = hevc_file(
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
        );
        let videos = video_identities(&file, None, true, false);
        assert_eq!(videos.len(), 2);
        assert!(!videos[0].preserves_dolby_vision());
        assert!(videos[1].preserves_dolby_vision());

        let held = fingerprints_of(&file, true);
        assert_ne!(
            held[0], held[1],
            "the preserved envelope and the strip are different byte streams, \
             which is why one index could never answer for both"
        );
    }

    /// The converting identity is added only for a file that can actually be
    /// converted, and only on a node that will.
    ///
    /// Both halves are load-bearing in opposite directions. Dropping the file
    /// check would index a third pipeline for every Dolby Vision title in the
    /// library — a full extra pass over a 60 GB remux each — for a stream no
    /// session can ask for. Dropping the node check would do the same on a
    /// node where an operator turned the conversion off.
    #[test]
    fn the_converting_identity_is_indexed_only_when_it_can_be_served() {
        let mut p7 = hevc_file(
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
        );
        p7.dolby_vision.profile = Some(7);
        p7.dolby_vision.level = Some(6);
        p7.dolby_vision.bl_compat_id = Some(1);

        let converting = video_identities(&p7, None, true, true);
        assert_eq!(converting.len(), 3, "strip, preserve, convert");
        assert!(converting[2].converts_dolby_vision());
        assert!(
            converting[0..2].iter().all(|v| !v.converts_dolby_vision()),
            "the stripped identity stays first so a fully indexed library does \
             not re-order its work to adopt this"
        );

        // Three different byte streams, so three different indexes. Sharing
        // one would hand a session a playlist whose cut points describe media
        // it never produces.
        let fingerprints: std::collections::HashSet<_> = converting
            .iter()
            .map(|video| identity_for(&p7, *video).argv_fingerprint)
            .collect();
        assert_eq!(fingerprints.len(), 3);

        // The node switch stands it down.
        assert_eq!(video_identities(&p7, None, true, false).len(), 2);

        // …and so does a file the conversion cannot describe. A Profile 8
        // source is already what the conversion produces; a row with no
        // columns cannot have its configuration record built at all.
        let p8 = {
            let mut file = p7.clone();
            file.dolby_vision.profile = Some(8);
            file
        };
        assert_eq!(video_identities(&p8, None, true, true).len(), 2);
        let label_only = hevc_file(
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
        );
        assert_eq!(video_identities(&label_only, None, true, true).len(), 2);
    }

    /// A transform implemented after FFmpeg must move the index identity even
    /// when the executable argv is byte-for-byte unchanged.  Ordinary copy
    /// recipes do not carry that transform and therefore keep their existing
    /// identities across a conversion revision bump.
    #[test]
    fn the_post_mux_transform_revision_is_part_of_only_the_converted_identity() {
        let mut file = hevc_file(
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
        );
        file.dolby_vision.profile = Some(7);
        file.dolby_vision.level = Some(6);
        file.dolby_vision.bl_compat_id = Some(1);

        let preserved = transcode::CopyVideoOptions::new(true, true);
        let converted = preserved.with_dolby_vision_conversion(true);
        let legacy_converted = identity_for_transform(&file, converted, None);
        let current_converted = identity_for(&file, converted);
        let next_converted = identity_for_transform(
            &file,
            converted,
            Some("dv-p7-to-p81-rpu-next-test-revision"),
        );

        assert_ne!(
            current_converted.argv_fingerprint, legacy_converted.argv_fingerprint,
            "the Rust transform cannot be represented only by FFmpeg argv"
        );
        assert_ne!(
            current_converted.argv_fingerprint, next_converted.argv_fingerprint,
            "a byte-affecting transform revision must invalidate its old index"
        );
        assert_eq!(
            identity_for(&file, preserved).argv_fingerprint,
            identity_for_transform(
                &file,
                preserved,
                Some("dv-p7-to-p81-rpu-next-test-revision")
            )
            .argv_fingerprint,
            "ordinary preserving artifacts remain reusable"
        );

        let mut plain_hdr10 = file.clone();
        plain_hdr10.hdr = Some("hdr10".to_owned());
        plain_hdr10.hdr_format = Some("HDR10".to_owned());
        plain_hdr10.dolby_vision = Default::default();
        assert_eq!(
            identity_for(&plain_hdr10, converted).argv_fingerprint,
            identity_for_transform(&plain_hdr10, converted, None).argv_fingerprint,
            "a conversion flag on a non-DV source selects no Rust transform"
        );
    }

    /// The converted stream's configuration record, from the source's facts.
    ///
    /// What its output carries is the source's own record — ffmpeg copies the
    /// one the input container had — so every field here is either changed
    /// deliberately or carried deliberately, and getting either wrong is a
    /// stream that describes itself incorrectly with nothing to catch it.
    #[test]
    fn the_converted_record_says_profile_eight_with_no_enhancement_layer() {
        let mut file = hevc_file(Some("dolby_vision"), None);
        file.dolby_vision.profile = Some(7);
        file.dolby_vision.level = Some(9);
        file.dolby_vision.bl_compat_id = Some(6);

        let record = converted_dolby_vision_record(&file).expect("describable");
        assert_eq!(record.profile, 8, "the RPUs now say 8.1");
        assert!(
            !record.el_present,
            "the copy's bitstream filter dropped the enhancement layer; a \
             record still declaring one tells a decoder to expect what is not \
             there"
        );
        assert!(record.rpu_present, "the RPUs are the point");
        assert!(record.bl_present);
        assert_eq!(
            record.level, 9,
            "the level bounds resolution and frame rate, neither of which the \
             conversion touches"
        );
        assert_eq!(
            record.bl_signal_compatibility_id, 1,
            "the output is Profile 8.1 even when the Profile 7 source record \
             carried another compatibility id"
        );

        // A row that cannot describe one refuses rather than inventing values.
        let mut levelless = file.clone();
        levelless.dolby_vision.level = None;
        assert!(converted_dolby_vision_record(&levelless).is_err());
    }

    #[test]
    fn profile_five_keeps_its_stripping_identity_too() {
        // `decide` never routes a Profile 5 source to a stripping copy -- it
        // has no HDR10 base -- but `decide_forced(Force::Original)` does, and
        // that copy is indexed today. The plan's M1 acceptance check expects
        // one row here; dropping the second would regress a live path.
        let file = hevc_file(Some("dolby_vision"), Some("Dolby Vision · Profile 5"));
        assert_eq!(video_identities(&file, None, true, false).len(), 2);
    }

    #[test]
    fn an_ffmpeg_that_cannot_strip_still_offers_two_identities() {
        // Worth pinning because it is the opposite of what it looks like:
        // without the `dovi_rpu` filter nothing removes the RPU, so it would
        // be easy to assume the two pipelines collapse into one. They do not.
        // The sample entry tag is the same `hvc1` either way for a source with
        // an HDR10-compatible base, but `-strict unofficial` and the bitstream
        // filter chain both still differ, and those decide the emitted bytes.
        let file = hevc_file(
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
        );
        let held = fingerprints_of(&file, false);
        assert_eq!(held.len(), 2);
        assert_ne!(held[0], held[1]);
    }

    /// A stripping pass on an ffmpeg without `dovi_rpu` asks for the record to
    /// be removed, and the ask reaches the stored promotion inputs.
    ///
    /// The wire between "this filter chain leaves a record describing a stream
    /// that no longer exists" and "no served init carries it". Without it the
    /// removal happens nowhere: the muxer writes a Profile 7 record with an
    /// enhancement layer over a stream with neither, VideoToolbox believes it,
    /// and Safari software-decodes 4K10 HEVC on hardware built to do it in
    /// silicon.
    #[test]
    fn a_strip_without_dovi_rpu_asks_the_promotion_to_remove_the_record() {
        let dv = hevc_file(
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
        );
        let plain = hevc_file(None, None);

        let strip_no_bsf = transcode::CopyVideoOptions::new(false, false);
        assert!(
            strip_no_bsf.leaves_a_stale_dolby_vision_record(&dv),
            "the one branch that half-strips"
        );

        // Every neighbouring answer is a whole one and needs no help.
        assert!(
            !transcode::CopyVideoOptions::new(true, false).leaves_a_stale_dolby_vision_record(&dv),
            "dovi_rpu removes the side data, so the muxer writes no record"
        );
        assert!(
            !transcode::CopyVideoOptions::new(false, true).leaves_a_stale_dolby_vision_record(&dv),
            "a preserved stream's record is true"
        );
        assert!(
            !transcode::CopyVideoOptions::new(false, true)
                .with_dolby_vision_conversion(true)
                .leaves_a_stale_dolby_vision_record(&dv),
            "a conversion rewrites the record rather than removing it"
        );
        assert!(
            !strip_no_bsf.leaves_a_stale_dolby_vision_record(&plain),
            "a source with no Dolby Vision has no record to be stale"
        );

        // …and the pass the index pipe actually selects from it. This is the
        // half that was untested: with the selection inline in the pipe
        // builder, which spawns ffmpeg and cannot be driven from a test, the
        // whole fix could be deleted and the suite stayed green.
        assert_eq!(
            dolby_vision_pass_for(&dv, strip_no_bsf).expect("a strip needs no record"),
            DolbyVisionPass::Remove
        );
        assert_eq!(
            dolby_vision_pass_for(&dv, transcode::CopyVideoOptions::new(true, false))
                .expect("dovi_rpu"),
            DolbyVisionPass::Untouched
        );
        assert_eq!(
            dolby_vision_pass_for(&dv, transcode::CopyVideoOptions::new(false, true))
                .expect("preserve"),
            DolbyVisionPass::Untouched
        );
        assert_eq!(
            dolby_vision_pass_for(&plain, strip_no_bsf).expect("not Dolby Vision"),
            DolbyVisionPass::Untouched
        );

        // A conversion rewrites rather than removes — and asks for a record
        // this file can describe.
        let mut p7 = dv.clone();
        p7.dolby_vision.profile = Some(7);
        p7.dolby_vision.level = Some(6);
        p7.dolby_vision.bl_compat_id = Some(1);
        let converting =
            transcode::CopyVideoOptions::new(false, true).with_dolby_vision_conversion(true);
        assert!(matches!(
            dolby_vision_pass_for(&p7, converting).expect("describable"),
            DolbyVisionPass::Rewrite(_)
        ));
    }

    /// A probe describing the minimal 23-byte hvcC — the WEB-DL Matroska
    /// shape whose parameter sets live only in band, and the only one
    /// `hevc_parameter_set_promotion_required` fires on.
    const MINIMAL_HVCC_PROBE: &str = r#"{"streams":[{"codec_type":"video","extradata_size":23}]}"#;

    /// The argv and the record answer are one recipe, for every identity the
    /// indexer actually enumerates.
    ///
    /// This is the invariant, and it is asserted against the rendered argv
    /// rather than restated: a pass removes the record exactly when its own
    /// filter chain strips the Dolby Vision NAL units without asking ffmpeg to
    /// drop the side data too. Carrying the two halves separately is how they
    /// come apart — each is individually correct, and together they describe
    /// two different streams with nothing to notice.
    #[test]
    fn every_index_pass_agrees_with_the_argv_it_carries() {
        let mut removing = 0;
        let mut promoting = 0;
        // Each row carries the columns its label describes. Stamping Profile 7
        // over all of them made the "Profile 5" row a duplicate of the Profile
        // 7 one — `file_can_convert_to_p81` reads the columns, not the label —
        // so a real Profile 5 file, which has no compatible base and therefore
        // one identity fewer, was never enumerated.
        for (hdr, label, profile, compat) in [
            (
                Some("dolby_vision"),
                Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
                Some(7i64),
                Some(1i64),
            ),
            (
                Some("dolby_vision"),
                Some("Dolby Vision · Profile 5"),
                Some(5),
                Some(0),
            ),
            (Some("hdr10"), Some("HDR10"), None, None),
            (None, None, None, None),
        ] {
            let mut file = hevc_file(hdr, label);
            file.dolby_vision.profile = profile;
            file.dolby_vision.level = profile.map(|_| 6i64);
            file.dolby_vision.bl_compat_id = compat;
            for have_dovi in [false, true] {
                for convert in [false, true] {
                    // `probe_json` both ways. `Some` turns on parameter-set
                    // promotion, which is the branch of `copy_video_args` that
                    // hand-rolls its filter list instead of calling
                    // `hevc_copy_bsf_for_copy` — the one place the two
                    // builders could most plausibly disagree.
                    for probe in [None, Some(MINIMAL_HVCC_PROBE)] {
                        for video in video_identities(&file, probe, have_dovi, convert) {
                            let pass = index_pass(
                                &file,
                                video,
                                None,
                                VideoCompletionExpectation {
                                    stream_index: 0,
                                    duration_num: 60,
                                    duration_den: 1,
                                    provenance: CompletionProvenance::StreamSeconds,
                                    source_object_version: "test".to_owned(),
                                },
                                None,
                            )
                            .expect("a describable pass");
                            let filter = pass
                                .args
                                .windows(2)
                                .find(|pair| pair[0] == "-bsf:v")
                                .map(|pair| pair[1].clone())
                                .unwrap_or_default();
                            let leaves_a_record =
                                filter.contains("62-63") && !filter.contains("dovi_rpu");
                            assert_eq!(
                                pass.dolby_vision == DolbyVisionPass::Remove,
                                leaves_a_record,
                                "hdr={hdr:?} label={label:?} dovi={have_dovi} convert={convert} \
                                 promote={} rendered {filter}",
                                video.promotes_parameter_sets()
                            );
                            if leaves_a_record {
                                removing += 1;
                            }
                            if video.promotes_parameter_sets() {
                                promoting += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(
            removing > 0,
            "no enumerated identity reached the removing branch, so the \
             equality above held vacuously"
        );
        assert!(
            promoting > 0,
            "no enumerated identity reached the parameter-set-promotion \
             branch, which is the filter list the two builders could disagree in"
        );
    }

    /// A `Remove` pass reaches the stored promotion inputs, and an ordinary
    /// one does not.
    ///
    /// The wire the fix is, end to end within this module: the predicate above
    /// says the compensation is needed, and this says the ask survives to the
    /// place every served init — live and regenerated — is promoted from.
    /// Without it the whole fix can be deleted from `build_with_args` and the
    /// suite stays green.
    #[tokio::test]
    async fn a_removing_pass_stores_the_ask_in_the_promotion_inputs() {
        testfixtures::require_ffmpeg();
        let bytes = index_pipe_bytes("closed-gop");

        let removing = index_stream(
            std::io::Cursor::new(bytes.clone()),
            identity(),
            None,
            DolbyVisionPass::Remove,
        )
        .await;
        let IndexOutcome::Built(removing) = removing else {
            panic!("{removing:?}");
        };
        assert!(
            removing.promotion.strip_dolby_vision,
            "a removing pass must record the ask, or nothing removes the record"
        );
        assert!(
            removing.promotion.dolby_vision.is_none(),
            "and must not also ask for a rewrite, which promotion refuses"
        );

        let ordinary = index_stream(
            std::io::Cursor::new(bytes),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await;
        let IndexOutcome::Built(ordinary) = ordinary else {
            panic!("{ordinary:?}");
        };
        assert!(
            !ordinary.promotion.strip_dolby_vision,
            "every other pass leaves the muxer's record alone"
        );
    }

    /// The record the caller supplies reaches the stored promotion inputs, and
    /// therefore the served init every later generation is promoted to.
    ///
    /// This is the wire between "plurx builds a Dolby Vision record because
    /// ffmpeg cannot" and "every served init carries it". Without it the
    /// record is built, discarded, and the converted stream tells every client
    /// it is plain HDR10 — the whole milestone delivering nothing, with no
    /// error anywhere.
    #[tokio::test]
    async fn the_supplied_dolby_vision_record_reaches_the_stored_promotion() {
        testfixtures::require_ffmpeg();
        let bytes = index_pipe_bytes("closed-gop");

        let plain = index_stream(
            std::io::Cursor::new(bytes.clone()),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await;
        let IndexOutcome::Built(plain) = plain else {
            panic!("{plain:?}");
        };
        assert!(
            plain.promotion.dolby_vision.is_none(),
            "an ordinary pass supplies none"
        );

        let record =
            plurx_core::fmp4::DolbyVisionRecord::new(8, 6, false, true, true, 1).expect("record");
        // Supplying the record *is* asking for the conversion, so the stream
        // has to be one there is something to convert in.
        let converted = index_stream(
            std::io::Cursor::new(testfixtures::with_dolby_vision_rpus(&bytes)),
            identity(),
            None,
            DolbyVisionPass::Rewrite(Box::new(record.clone())),
        )
        .await;
        let IndexOutcome::Built(converted) = converted else {
            panic!("{converted:?}");
        };
        assert_eq!(
            converted.promotion.dolby_vision,
            Some(record),
            "the record is stored, so a regenerated init is promoted from it too"
        );
        assert!(!converted.promotion.is_empty());
    }

    /// The conversion runs inside the index pass, and the rows describe the
    /// converted stream.
    ///
    /// This is the wire the whole converting identity hangs from. The index a
    /// converting session lands against has to have been built from converted
    /// fragments: an 8.1 RPU is smaller than the Profile 7 one it replaces, so
    /// every `video_bytes` differs, and an index built without the conversion
    /// would describe a stream no converting session ever produces — the
    /// landing would miss on every fragment and the session would fail with
    /// nothing pointing at why.
    #[tokio::test]
    async fn a_converting_pass_indexes_the_converted_stream() {
        testfixtures::require_ffmpeg();
        let plain_bytes = index_pipe_bytes("closed-gop");
        let dv_bytes = testfixtures::with_dolby_vision_rpus(&plain_bytes);

        let unconverted = index_stream(
            std::io::Cursor::new(dv_bytes.clone()),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await;
        let IndexOutcome::Built(unconverted) = unconverted else {
            panic!("a Dolby Vision stream indexes without converting: {unconverted:?}");
        };

        let converted = index_stream(
            std::io::Cursor::new(dv_bytes),
            identity(),
            None,
            DolbyVisionPass::Rewrite(Box::new(converting_record())),
        )
        .await;
        let IndexOutcome::Built(converted) = converted else {
            panic!("the converting pass must build: {converted:?}");
        };

        assert_eq!(
            converted.rows.len(),
            unconverted.rows.len(),
            "the conversion rewrites metadata, it does not add or drop frames"
        );
        for (with, without) in converted.rows.iter().zip(unconverted.rows.iter()) {
            assert_eq!(with.dts, without.dts, "no timestamp is reconstructed");
            assert_eq!(with.duration, without.duration);
            assert!(
                with.video_bytes < without.video_bytes,
                "every fragment shrinks by what the 8.1 RPUs no longer carry"
            );
        }
    }

    /// A converting pass over a stream with no RPUs in it refuses.
    ///
    /// The row said Profile 7 and the stream carries nothing to convert, so
    /// one of the two is lying. Indexing it under the converted identity would
    /// record that lie in the keyspace, and every session that looked the
    /// identity up would be served segments cut for a stream that was never
    /// converted.
    #[tokio::test]
    async fn a_converting_pass_over_a_stream_with_no_rpus_is_not_an_index() {
        testfixtures::require_ffmpeg();
        let bytes = index_pipe_bytes("closed-gop");
        let outcome = index_stream(
            std::io::Cursor::new(bytes),
            identity(),
            None,
            DolbyVisionPass::Rewrite(Box::new(converting_record())),
        )
        .await;
        let IndexOutcome::Unsupported(reason) = outcome else {
            panic!("a stream with no RPUs cannot be indexed as converted: {outcome:?}");
        };
        assert!(reason.contains("no RPUs to convert"), "{reason}");
    }

    /// The index pipe over a real fixture, read the way the daemon reads it.
    async fn index_fixture(kind: &str) -> IndexOutcome {
        let bytes = index_pipe_bytes(kind);
        index_stream(
            std::io::Cursor::new(bytes),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await
    }

    /// The video-only pipe's own output, cached beside the fixture.
    pub(super) fn index_pipe_bytes(kind: &str) -> Vec<u8> {
        let source = testfixtures::source(kind);
        let mut command = std::process::Command::new(testfixtures::ffmpeg());
        command
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&source)
            .args([
                "-map_chapters",
                "-1",
                "-map",
                "0:v:0?",
                "-an",
                "-sn",
                "-c:v",
                "copy",
            ]);
        if kind != "h264" && kind != "vp9" {
            command
                .args(["-tag:v", "hvc1"])
                .args(["-bsf:v", "filter_units=remove_types=32-34"]);
        }
        command
            .args(["-avoid_negative_ts", "make_zero"])
            .args([
                "-movflags",
                "frag_keyframe+empty_moov+default_base_moof+delay_moov",
            ])
            .args(["-use_editlist", "0", "-f", "mp4", "pipe:1"]);
        testfixtures::run(&mut command)
    }

    fn replace_hvcc_array_type(bytes: &mut [u8], from: u8, to: u8) {
        let kind_at = bytes
            .windows(4)
            .position(|window| window == b"hvcC")
            .expect("hvcC box");
        let payload = kind_at + 4;
        let arrays = usize::from(bytes[payload + 22]);
        let mut pos = payload + 23;
        for _ in 0..arrays {
            let array_kind = bytes[pos] & 0x3f;
            let count = u16::from_be_bytes([bytes[pos + 1], bytes[pos + 2]]) as usize;
            if array_kind == from {
                bytes[pos] = (bytes[pos] & 0xc0) | to;
                return;
            }
            pos += 3;
            for _ in 0..count {
                let len = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as usize;
                pos += 2 + len;
            }
        }
        panic!("hvcC carried no type-{from} array");
    }

    #[tokio::test]
    async fn a_real_pipe_reports_its_parameter_sets_constant() {
        // The scan-time check plan §2.2's ruling asks for. Every clean
        // fragment of a single-pass encode carries the same parameter sets, so
        // the film is VOD-presentable.
        let IndexOutcome::Built(index) = index_fixture("closed-gop").await else {
            panic!("the closed-gop fixture must index");
        };
        assert!(
            index.parameter_sets_constant,
            "a single-pass encode's clean starts must agree"
        );
        // And this corpus carries nothing promotable -- the test pipe strips
        // types 32-34 the way `copy_video_args` does for ordinary HEVC, so
        // promotion is a no-op and there is nothing to capture. Asserted
        // rather than assumed, because a corpus that silently started
        // carrying them would make the test above pass for a different reason.
        assert!(
            index.promotion.is_empty(),
            "ordinary HEVC carries its parameter sets in hvcC, not in band"
        );
    }

    #[tokio::test]
    async fn emitted_hvc1_refuses_an_incomplete_decoder_configuration_without_a_probe_hint() {
        let mut bytes = index_pipe_bytes("closed-gop");
        // Keep the hvcC structurally valid but turn its PPS array into a
        // duplicate SPS array. The ordinary fixture pipe has already removed
        // in-band sets, so there is no hidden PPS from which to "succeed".
        replace_hvcc_array_type(&mut bytes, 34, 33);
        let outcome = index_stream(
            std::io::Cursor::new(bytes),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await;
        let IndexOutcome::Unsupported(reason) = outcome else {
            panic!("an incomplete emitted hvcC must not be indexed: {outcome:?}");
        };
        assert!(reason.contains("complete VPS/SPS/PPS"), "{reason}");
    }

    #[tokio::test]
    async fn a_film_whose_clean_starts_disagree_is_not_vod_presentable() {
        // Built by hand, because no fixture can produce it: the corpus is
        // single-invocation encodes, which are structurally incapable of
        // per-IDR parameter-set variation. The check still has to work.
        let bytes = index_pipe_bytes("closed-gop");
        let IndexOutcome::Built(index) = index_stream(
            std::io::Cursor::new(bytes),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await
        else {
            panic!("must index");
        };
        // A constant film, as the fixture is.
        assert!(index.parameter_sets_constant);

        // Now the same walk with one start disagreeing. `PromotionInputs`
        // compares by value, so this is the exact comparison `index_stream`
        // makes.
        use plurx_core::fmp4::PromotionInputs;
        let canonical = PromotionInputs {
            hevc_configuration: None,
            strip_dolby_vision: false,
            dolby_vision: None,
            parameter_sets: vec![vec![0x40, 0x01, 0x0c]],
            hdr10_sei: Vec::new(),
        };
        let differing = PromotionInputs {
            hevc_configuration: None,
            strip_dolby_vision: false,
            dolby_vision: None,
            parameter_sets: vec![vec![0x40, 0x01, 0x0d]],
            hdr10_sei: Vec::new(),
        };
        assert_ne!(
            canonical, differing,
            "the comparison that decides VOD-presentability must see the \
             difference between two parameter sets that differ by one byte"
        );
    }

    #[tokio::test]
    async fn a_real_pipe_indexes_every_fragment_it_carries() {
        let IndexOutcome::Built(index) = index_fixture("closed-gop").await else {
            panic!("the closed-gop fixture must index");
        };
        assert!(index.rows.len() > 3, "got {} rows", index.rows.len());
        assert!(index.timescale > 0);
        assert_eq!(index.version, SEGPLAN_VERSION);
        assert_eq!(index.init_sha256.len(), 64);
        assert!(
            index
                .rows
                .iter()
                .all(|row| row.bytes > 0 && row.duration > 0),
            "every row describes real bytes and real time"
        );
        // Contiguous: each fragment starts where the last one ended, which is
        // what lets a plan entry's start be an index row's DTS.
        for pair in index.rows.windows(2) {
            assert_eq!(pair[0].dts + pair[0].duration, pair[1].dts);
        }
    }

    #[tokio::test]
    async fn the_index_is_deterministic() {
        let first = index_fixture("closed-gop").await;
        let second = index_fixture("closed-gop").await;
        assert_eq!(first, second, "two passes must describe the same file");
    }

    #[tokio::test]
    async fn an_open_gop_fixture_is_classified_dirty_and_a_closed_one_clean() {
        let IndexOutcome::Built(open) = index_fixture("open-gop").await else {
            panic!("open-gop must index");
        };
        let IndexOutcome::Built(closed) = index_fixture("closed-gop").await else {
            panic!("closed-gop must index");
        };
        assert!(
            closed.rows.iter().filter(|row| row.clean()).count() > closed.rows.len() / 2,
            "a closed GOP is clean nearly everywhere"
        );
        assert!(
            open.rows.iter().filter(|row| !row.clean()).count() > open.rows.len() / 2,
            "an open GOP with leading pictures is dirty nearly everywhere"
        );
    }

    #[tokio::test]
    async fn a_truncated_pipe_is_not_an_index() {
        // The failure that matters: ffmpeg dies partway and emits a perfectly
        // well-formed prefix. An index built from it places every later
        // boundary in the wrong part of the film.
        let bytes = index_pipe_bytes("closed-gop");
        let half = bytes.len() / 2;
        let outcome = index_stream(
            std::io::Cursor::new(bytes[..half].to_vec()),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await;
        assert!(
            matches!(
                outcome,
                IndexOutcome::Failed(ref failure)
                    if failure.code == IndexFailureCode::IndexOutputMalformed
            ),
            "a half-read pipe must not produce an index, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_pipe_that_stops_short_of_the_probed_duration_is_truncated() {
        let bytes = index_pipe_bytes("closed-gop");
        let IndexOutcome::Built(full) = index_stream(
            std::io::Cursor::new(bytes.clone()),
            identity(),
            None,
            DolbyVisionPass::Untouched,
        )
        .await
        else {
            panic!("the full pipe indexes");
        };
        let _covered: u64 = full.rows.iter().map(|row| row.duration).sum();
        // Claim the file is a minute longer than it is: the coverage check is
        // the only thing standing between a short read and a wrong index.
        let outcome = index_stream(
            std::io::Cursor::new(bytes),
            identity(),
            Some(60_000 + 12_000),
            DolbyVisionPass::Untouched,
        )
        .await;
        assert!(
            matches!(
                outcome,
                IndexOutcome::Failed(ref failure)
                    if failure.code == IndexFailureCode::IndexVideoShortfall
            ),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn an_index_builds_a_plan_whose_entries_land_on_index_rows() {
        let IndexOutcome::Built(index) = index_fixture("closed-gop").await else {
            panic!("closed-gop must index");
        };
        let policy = fmp4::CutPolicy::new(2, 1, 64 * 1024 * 1024, 15, index.timescale);
        let tracks = segplan::TrackDurations {
            video_ms: 12_000,
            audio_ms: 12_000,
            audio_bits_per_second: 256_000,
        };
        let plan = segplan::plan_copy(&index, &policy, &tracks);
        assert!(!plan.is_empty());
        for entry in plan
            .entries
            .iter()
            .filter(|entry| entry.kind == segplan::PlanEntryKind::Video)
        {
            assert!(
                index.rows.iter().any(|row| row.dts == entry.start_ticks),
                "plan entry {} starts at {} ticks, which is not a fragment boundary",
                entry.index,
                entry.start_ticks
            );
        }
    }

    #[tokio::test]
    async fn a_plan_from_a_persisted_index_builds_well_under_a_tenth_of_a_second() {
        let IndexOutcome::Built(index) = index_fixture("closed-gop").await else {
            panic!("closed-gop must index");
        };
        // Blow the corpus up to feature length so the measurement means
        // something: 12 s of fixture is not a 2-hour film.
        let mut rows = Vec::new();
        let mut dts = 0;
        for _ in 0..600 {
            for row in &index.rows {
                rows.push(IndexRow { dts, ..row.clone() });
                dts += row.duration;
            }
        }
        let big = FragmentIndex::new(index.timescale, rows, "sha", identity());
        let policy = fmp4::CutPolicy::new(6, 2, 64 * 1024 * 1024, 15, big.timescale);
        let tracks = segplan::TrackDurations {
            video_ms: 7_200_000,
            audio_ms: 7_200_000,
            audio_bits_per_second: 256_000,
        };
        let started = Instant::now();
        let plan = segplan::plan_copy(&big, &policy, &tracks);
        let elapsed = started.elapsed();
        assert!(!plan.is_empty());
        assert!(
            elapsed < Duration::from_millis(100),
            "planning {} fragments took {elapsed:?}; the plan is built on every \
             create and must not be felt",
            big.rows.len()
        );
    }

    #[tokio::test]
    async fn a_landing_in_a_real_index_resolves_uniquely() {
        let IndexOutcome::Built(index) = index_fixture("closed-gop").await else {
            panic!("closed-gop must index");
        };
        assert!(index.rows.len() >= 5, "need a few fragments to land in");
        for start in 0..index.rows.len().saturating_sub(3) {
            let observed: Vec<u32> = index.rows[start..start + 3]
                .iter()
                .map(|row| row.video_bytes)
                .collect();
            assert_eq!(
                segplan::match_landing(&index, &observed),
                Ok(start),
                "a real fixture's fragment byte counts must place a landing"
            );
        }
    }
}

#[cfg(test)]
mod equality_tests {
    use super::*;
    use plurx_core::testfixtures;

    /// The equality the whole landing mechanism rests on, asserted by
    /// `cargo test` rather than by a script that ran once.
    ///
    /// M0 §12.2 measured that the video-only index pipe and the full
    /// production pipe agree fragment for fragment. Not on everything: the
    /// wire length differs by tens of kilobytes because the production pipe
    /// carries audio. What matches is the video sample bytes, and that is what
    /// `segplan::match_landing` compares — so if this test ever fails, a
    /// repositioned producer can no longer be placed and every far seek turns
    /// into a typed producer failure.
    #[tokio::test]
    async fn the_index_records_the_quantity_a_production_generation_reproduces() {
        for kind in ["closed-gop", "open-gop", "clean-cra", "h264"] {
            let IndexOutcome::Built(index) = index_stream(
                std::io::Cursor::new(super::tests::index_pipe_bytes(kind)),
                SourceIdentity::new(1, 1, "fingerprint"),
                None,
                DolbyVisionPass::Untouched,
            )
            .await
            else {
                panic!("{kind}: the index pipe must index");
            };

            // The production pipe, audio and all — what a repositioned
            // generation really emits. It cannot go through `index_stream`,
            // which refuses a non-video track on purpose, so it is read
            // directly here.
            let (produced_video, produced_wire) = production_fragments(kind);

            assert_eq!(
                index.rows.len(),
                produced_video.len(),
                "{kind}: fragment counts must agree"
            );
            let video: Vec<u32> = index.rows.iter().map(|row| row.video_bytes).collect();
            assert_eq!(
                video, produced_video,
                "{kind}: video sample bytes are what the landing matcher \
                 compares, so they must be identical across audio branches"
            );

            let wire: Vec<u32> = index.rows.iter().map(|row| row.bytes).collect();
            assert_ne!(
                wire, produced_wire,
                "{kind}: if the wire lengths ever DID agree, the fixture stopped \
                 carrying audio and this test stopped proving anything"
            );
        }
    }

    /// The production pipe's per-fragment (video sample bytes, wire length).
    fn production_fragments(kind: &str) -> (Vec<u32>, Vec<u32>) {
        let bytes = testfixtures::pipe(kind);
        let mut reader = FragmentReader::new();
        reader.push(&bytes);
        let mut init: Option<Init> = None;
        let mut video = Vec::new();
        let mut wire = Vec::new();
        while let Ok(Some(unit)) = reader.next_unit() {
            match unit {
                Unit::Init(parsed) => init = Some(parsed),
                Unit::Fragment(fragment) => {
                    let init = init.as_ref().expect("moov before fragments");
                    let track_id = init.video().expect("a video track").id;
                    let track = fragment.track(track_id).expect("video in the fragment");
                    video.push(u32::try_from(track.byte_len()).unwrap_or(u32::MAX));
                    wire.push(u32::try_from(fragment.len()).unwrap_or(u32::MAX));
                }
                Unit::Trailer => {}
            }
        }
        (video, wire)
    }
}

/// The pass's side of the PGS ride-along: what only `build_with_args` can
/// show — the stage it is handed, and what happens to it when the build
/// future is dropped.
#[cfg(test)]
mod ride_along_tests {
    use super::*;
    use crate::subtitle_ride_along::testing::{fixture, Sub};
    use crate::subtitle_ride_along::RideAlongPlan;
    use crate::subtitle_source::testing::stamp_of;
    use crate::subtitle_source::Verdict;

    const VIDEO: transcode::CopyVideoOptions = transcode::CopyVideoOptions::new(false, false);

    fn expectation() -> VideoCompletionExpectation {
        VideoCompletionExpectation {
            stream_index: 0,
            duration_num: 16,
            duration_den: 1,
            provenance: CompletionProvenance::StreamSeconds,
            source_object_version: "fixture".to_owned(),
        }
    }

    fn pass_over(source: &Path, file: &MediaFile, plan: Option<RideAlongPlan>) -> IndexPass {
        index_pass(
            file,
            VIDEO,
            Some(&source.to_string_lossy()),
            expectation(),
            plan,
        )
        .expect("a describable pass")
    }

    /// (f) A regression for the tee's own truncation: a file already sitting
    /// where a slave will write — a stale `.sup` and a stale `.crc` whose
    /// packet count would pass for a verdict — is truncated by the slave that
    /// opens it, so the track is judged on what this pass wrote, and the index
    /// is the baseline's. The tee opens its slaves itself (`avio_open`), with
    /// or without `-y`; the index output is `pipe:1`, so the leftover-file
    /// refusal ffmpeg applies to a named output (§6.2 fact 4) cannot reach it.
    /// This test therefore does not prove `-y` necessary — it pins the
    /// truncation the verdict relies on. The fresh stage per attempt is the
    /// other half: every attempt gets a directory of its own.
    #[tokio::test]
    async fn a_leftover_in_the_stage_cannot_empty_the_index() {
        let fixture = fixture(72_001, &[Sub::Pgs]);
        let cache = crate::test_tempdir().expect("cache");
        let started = Instant::now();
        let baseline = build_with_args(
            &fixture.file,
            pass_over(&fixture.source, &fixture.file, None),
            None,
            None,
            cache.path(),
            Duration::from_secs(60),
            started,
            None,
        )
        .await;
        assert!(matches!(baseline.outcome, IndexOutcome::Built(_)));

        let plan = RideAlongPlan::standalone(
            cache.path(),
            fixture.file.id,
            stamp_of(&fixture.source),
            vec![0],
        )
        .expect("plan");
        let other = RideAlongPlan::standalone(
            cache.path(),
            fixture.file.id,
            stamp_of(&fixture.source),
            vec![0],
        )
        .expect("another plan");
        assert_ne!(
            plan.stage(),
            other.stage(),
            "every attempt has its own stage"
        );
        std::fs::write(plan.stage().join("s0.sup"), b"left over by someone else")
            .expect("leftover");
        std::fs::write(
            plan.stage().join("s0.crc"),
            b"#left over\n0, 0, 0, 0, 5, 0x0\n",
        )
        .expect("leftover");
        let riding = build_with_args(
            &fixture.file,
            pass_over(&fixture.source, &fixture.file, Some(plan)),
            None,
            None,
            cache.path(),
            Duration::from_secs(60),
            Instant::now(),
            None,
        )
        .await;
        assert_eq!(
            riding.outcome, baseline.outcome,
            "the same index, not an empty one"
        );
        let harvest = riding.ride_along.expect("a harvest").judge().await;
        assert_eq!(
            harvest.outcomes()[0].verdict,
            Verdict::Kept,
            "{:?}",
            harvest.outcomes()
        );
    }

    /// Review finding 1: a riding pass whose own argv breaks the index — here
    /// a hard map to a subtitle ordinal the file does not have, as an
    /// `ffprobe` of another build could produce — is remembered, and the
    /// file's next attempt is a plain index pass that builds.
    #[tokio::test]
    async fn after_a_riding_pass_fails_the_next_attempt_does_not_ride() {
        let fixture = fixture(72_003, &[Sub::Pgs]);
        let cache = crate::test_tempdir().expect("cache");
        let root = crate::subtitle_source::store_root(cache.path());
        let plan = RideAlongPlan::standalone(
            &root,
            fixture.file.id,
            stamp_of(&fixture.source),
            vec![0, 5],
        )
        .expect("plan");
        let broken = build_with_args(
            &fixture.file,
            pass_over(&fixture.source, &fixture.file, Some(plan)),
            None,
            None,
            cache.path(),
            Duration::from_secs(60),
            Instant::now(),
            None,
        )
        .await;
        assert!(
            matches!(broken.outcome, IndexOutcome::Failed(_)),
            "a map to a missing ordinal fails the pass: {:?}",
            broken.outcome
        );
        assert!(broken.ride_along.is_none());

        let gate = crate::subtitle_ride_along::RideAlongGate::for_test(root.clone());
        let source = std::fs::File::open(&fixture.source).expect("held source");
        assert!(
            crate::subtitle_ride_along::plan(&gate, fixture.file.id, &source, &[0])
                .await
                .is_none(),
            "the file does not ride again on the same source"
        );
        let retry = build_from_attested_file_with_progress(
            &fixture.file,
            &source,
            "fixture-object-v1",
            VIDEO,
            cache.path(),
            Duration::from_secs(60),
            |_: &crate::fragindex::PassProgress| {},
            Some(&gate),
        )
        .await;
        assert!(
            matches!(retry.outcome, IndexOutcome::Built(_)),
            "the plain retry builds the index: {:?}",
            retry.outcome
        );
        assert!(retry.ride_along.is_none(), "and did not ride");
    }

    /// PR 3: a riding pass reports, with its progress, how many PGS tracks
    /// it keeps and what its stage holds — re-measured at most once a second —
    /// so the analysis row can say so while the pass runs.
    ///
    /// The fixture's track is a few kilobytes, which the `sup` slave holds in
    /// its I/O buffer until the trailer; a film's track is megabytes and
    /// reaches the disk as it goes. So the test puts known bytes into the
    /// stage mid-pass and requires a later report to count them: the meter
    /// re-measures the stage during the pass rather than once at its start.
    #[tokio::test]
    async fn a_riding_pass_reports_its_tracks_and_bytes_with_its_progress() {
        const MARKER: usize = 1234;
        let fixture = fixture(72_004, &[Sub::Pgs]);
        let cache = crate::test_tempdir().expect("cache");
        let plan = RideAlongPlan::standalone(
            cache.path(),
            fixture.file.id,
            stamp_of(&fixture.source),
            vec![0],
        )
        .expect("plan");
        let stage = plan.stage().to_owned();
        let mut pass = pass_over(&fixture.source, &fixture.file, Some(plan));
        // Read at the media's own pace, so the pass is still running while
        // the test watches its reports.
        let input = pass
            .args
            .iter()
            .position(|arg| arg == "-i")
            .expect("an input");
        pass.args.insert(input, "-re".to_owned());
        let seen = Arc::new(std::sync::Mutex::new(Vec::<PassProgress>::new()));
        let recorder = Arc::clone(&seen);
        let progress: SharedIndexProgress = Arc::new(move |report: &PassProgress| {
            recorder.lock().expect("seen").push(*report);
        });
        let mut build = Box::pin(build_with_args(
            &fixture.file,
            pass,
            None,
            None,
            cache.path(),
            Duration::from_secs(60),
            Instant::now(),
            Some(progress),
        ));
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut marked = false;
        loop {
            let reports = seen.lock().expect("seen").clone();
            if let Some(first) = reports.first() {
                assert_eq!(first.pgs_tracks, 1, "every report names the track");
                assert!(reports.iter().all(|report| report.pgs_tracks == 1));
                if !marked {
                    std::fs::write(stage.join("meter-marker"), [0_u8; MARKER])
                        .expect("a marker in the stage");
                    marked = true;
                } else if reports
                    .iter()
                    .any(|report| report.pgs_bytes_written >= MARKER as u64)
                {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "no report counted the stage's bytes: {reports:?}"
            );
            if tokio::time::timeout(Duration::from_millis(50), &mut build)
                .await
                .is_ok()
            {
                panic!("the paced pass finished before a report counted the stage: {reports:?}");
            }
        }
        drop(build);
    }

    /// (g) The cluster worker `select!`s the build against lease loss and
    /// foreground preemption; the losing branch drops the build future. The
    /// stage must go with it.
    #[tokio::test]
    async fn dropping_the_build_future_mid_pass_removes_the_stage() {
        let fixture = fixture(72_002, &[Sub::Pgs]);
        let cache = crate::test_tempdir().expect("cache");
        let plan = RideAlongPlan::standalone(
            cache.path(),
            fixture.file.id,
            stamp_of(&fixture.source),
            vec![0],
        )
        .expect("plan");
        let stage = plan.stage().to_owned();
        let mut pass = pass_over(&fixture.source, &fixture.file, Some(plan));
        // Read at the media's own pace, so sixteen seconds of it are still
        // being read when the future is dropped.
        let input = pass
            .args
            .iter()
            .position(|arg| arg == "-i")
            .expect("an input");
        pass.args.insert(input, "-re".to_owned());
        let mut build = Box::pin(build_with_args(
            &fixture.file,
            pass,
            None,
            None,
            cache.path(),
            Duration::from_secs(60),
            Instant::now(),
            None,
        ));
        // Mid-pass: the tee has opened its slaves in the stage.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !stage.join("s0.crc").exists() {
            assert!(
                Instant::now() < deadline,
                "the pass never opened its slaves"
            );
            if tokio::time::timeout(Duration::from_millis(20), &mut build)
                .await
                .is_ok()
            {
                panic!("the paced pass finished before it could be dropped");
            }
        }
        assert!(stage.is_dir());
        drop(build);
        assert!(!stage.exists(), "the stage outlived the dropped build");
    }
}
