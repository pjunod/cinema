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
    let (numerator, denominator) = parse_decimal(value)?;
    // `reduce_rational` refuses a zero numerator: a duration must be positive.
    reduce_rational(numerator, denominator)
}

/// A plain non-negative decimal as an unreduced `numerator / 10^digits`.
/// Zero is a value here; whether zero is acceptable is the caller's question,
/// because a component of a larger value (the seconds of `00:01:00.000`) may
/// be zero when the whole is not.
fn parse_decimal(value: &str) -> Option<(u128, u128)> {
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
    Some((
        whole.checked_mul(denominator)?.checked_add(fraction)?,
        denominator,
    ))
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
    // The seconds field alone may be zero — an exactly-N-minute title is
    // `00:01:00.000` — so it is parsed as a non-negative decimal and only the
    // whole duration has to be positive (`reduce_rational` refuses zero).
    let (seconds_num, seconds_den) = parse_decimal(seconds)?;
    if seconds_num >= 60 * seconds_den {
        return None;
    }
    let whole_seconds = hours
        .checked_mul(3_600)?
        .checked_add(minutes.checked_mul(60)?)?;
    let numerator = whole_seconds
        .checked_mul(seconds_den)?
        .checked_add(seconds_num)?;
    reduce_rational(numerator, seconds_den)
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
    let mut hdr10 = fmp4::Hdr10IndexMetadata::default();
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
                    // CLL is a per-sequence maximum, not a decoder parameter.
                    // Scan every sample so even a non-clean fragment contributes
                    // to the immutable title-wide metadata. In-band SEIs stay.
                    if hdr10.observe_fragment(&fragment, init).is_err() {
                        parameter_sets_constant = false;
                    }
                    if class.is_clean() {
                        let mut here = PromotionInputs::from_fragment(&fragment, init);
                        here.hdr10_sei.clear();
                        match promotion {
                            None => promotion = Some(here),
                            Some(ref canonical) => {
                                if !canonical.matches_later_parameter_sets(&here) {
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
    promotion.hdr10_sei = hdr10.promotion_nals();
    promotion.indexed_hdr10_metadata = true;
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
        // A failed pre-repair P5 index is a terminal persisted refusal.
        // Move only the affected recipe so it can rebuild after upgrading.
        if !video.retains_hevc_parameter_sets()
            && transcode::hevc_copy_tag_for_source(file, video.preserves_dolby_vision()) == "dvh1"
        {
            recipe.push("--plurx-dvh1-completeness=1".into());
        }
        recipe.push(format!(
            "--plurx-hdr10-index={}",
            fmp4::HDR10_INDEX_METADATA_REVISION
        ));
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
    /// Every identity this pass writes, in argv order: `outputs[0]` on stdout
    /// with the HEVC header trace, `outputs[i]` on inherited fd `3 + i`
    /// ([`transcode::copy_index_shared_args_with_input`]). One read of the
    /// source, one index per output.
    outputs: Vec<IndexOutput>,
    expectation: VideoCompletionExpectation,
    /// The PGS tracks this pass also keeps, and the stage it keeps them in.
    /// Its argv is already at the end of `args`; the plan rides with the pass
    /// so the verdict and the stage's removal belong to the same owner.
    ride_along: Option<RideAlongPlan>,
}

/// The content-analysis pass also yields the title's Dolby Vision aggregate.
/// While the pass reads the file for its index, the complete-title scanner
/// reads the same held source in its ordinary standalone mode: the same
/// unfiltered access units, the same checks, the same aggregate as the
/// `dv_title_scan` job, which never has to read the file again. The two read
/// the same pages in step, so the page cache serves the slower one and the
/// file is read once from storage in practice.
///
/// The scan can only ever add an answer. It never touches the pass's argv,
/// descriptors or exit status; it is kept only when the pass's lead output
/// built, and killed otherwise.
#[derive(Clone)]
pub(crate) struct DvTitleRide {
    tools: Arc<crate::vodencode::dv_runtime::ToolSet>,
    stream_index: u32,
}

/// The companion scan's task. Dropping it — the pass future dropped by a
/// preemption, an early return — aborts the task, and the task's child is
/// killed with it; a bare `JoinHandle` would leave the scan reading on.
#[cfg(target_os = "linux")]
struct DvTitleScan(tokio::task::JoinHandle<Result<plurx_core::process::bounded::Output, String>>);

#[cfg(target_os = "linux")]
impl Drop for DvTitleScan {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A complete-title aggregate the pass's companion scan produced, not yet
/// published. The caller publishes it after its own freshness checks.
#[derive(Debug)]
pub(crate) struct PendingDvTitle {
    pub(crate) scanner_digest: String,
    pub(crate) stats: plurx_core::scan::dv_title::DvTitleStats,
}

impl DvTitleRide {
    /// A title the scanner covers, on a node with the processing bundle.
    #[cfg(target_os = "linux")]
    async fn for_file(file: &MediaFile, stream_index: u32) -> Option<Self> {
        let facts = &file.dolby_vision;
        if !matches!(file.video_codec.as_deref(), Some("hevc"))
            || !matches!(facts.profile, Some(5 | 7 | 8))
            || facts.rpu_present != Some(true)
        {
            return None;
        }
        match crate::vodencode::dv_runtime::ToolSet::capture().await {
            Ok(Some(tools)) => Some(Self {
                tools,
                stream_index,
            }),
            Ok(None) => None,
            Err(error) => {
                tracing::debug!(file_id = file.id, %error, "title scanner unavailable to the index pass");
                None
            }
        }
    }

    /// Start the scanner on a duplicate of the pass's held source. The task
    /// owns the scanner: dropping or aborting it kills the child.
    #[cfg(target_os = "linux")]
    fn spawn(
        &self,
        source: &std::fs::File,
        wall_time: Duration,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Option<DvTitleScan> {
        let held = match source.try_clone() {
            Ok(held) => held,
            Err(error) => {
                tracing::info!(%error, "no descriptor for the index pass's title scan");
                return None;
            }
        };
        Some(DvTitleScanInput::spawn(
            held,
            self.tools.scanner.clone(),
            self.stream_index.to_string(),
            wall_time,
            cancel,
        ))
    }

    /// The scan's aggregate when the pass's lead output built; anything else
    /// keeps nothing, and the title's own job reads the file itself.
    #[cfg(target_os = "linux")]
    async fn finish(
        &self,
        file_id: i64,
        mut task: DvTitleScan,
        lead_built: bool,
        wait: Duration,
    ) -> Option<PendingDvTitle> {
        if !lead_built {
            task.0.abort();
            let _ = (&mut task.0).await;
            return None;
        }
        let refused = |reason: String| {
            tracing::info!(file_id, %reason, "the index pass kept no Dolby Vision title aggregate");
            None
        };
        let output = match tokio::time::timeout(wait, &mut task.0).await {
            Ok(Ok(Ok(output))) => output,
            Ok(Ok(Err(error))) => return refused(format!("scanner did not run: {error}")),
            Ok(Err(error)) => return refused(format!("scanner task failed: {error}")),
            Err(_) => {
                task.0.abort();
                let _ = (&mut task.0).await;
                return refused("scanner did not finish within the pass budget".to_owned());
            }
        };
        dv_title_aggregate(&output, self.tools.is_current().await)
            .map(|stats| PendingDvTitle {
                scanner_digest: self.tools.title_scanner_digest(),
                stats,
            })
            .map_err(refused)
            .ok()
    }
}

// Field order closes the original held descriptor before releasing its
// original admission, including when the spawned future was never polled.
#[cfg(target_os = "linux")]
struct DvTitleScanInput {
    held: std::fs::File,
    scanner: std::path::PathBuf,
    index: String,
    cancel: Option<tokio_util::sync::CancellationToken>,
    keeper: Option<Arc<dyn plurx_core::process::bounded::OriginalWorkKeeper>>,
}

#[cfg(target_os = "linux")]
impl DvTitleScanInput {
    fn spawn(
        held: std::fs::File,
        scanner: std::path::PathBuf,
        index: String,
        wall_time: Duration,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> DvTitleScan {
        let input = Self {
            held,
            scanner,
            index,
            cancel: cancel.cloned(),
            keeper: plurx_core::process::bounded::original_keeper(),
        };
        DvTitleScan(tokio::spawn(async move { input.run(wall_time).await }))
    }

    async fn run(
        self,
        wall_time: Duration,
    ) -> Result<plurx_core::process::bounded::Output, String> {
        use plurx_core::process::bounded;
        use std::os::fd::AsRawFd;
        let keeper = self.keeper.clone();
        let cancel = self.cancel.clone();
        let work = async move {
            let mut command = tokio::process::Command::new(&self.scanner);
            command.args(["/proc/self/fd/3", &self.index]);
            crate::ffmpeg::inherit_raw_file_descriptors_with_cwd(
                &mut command,
                &[(self.held.as_raw_fd(), 3)],
                None,
            );
            let output = bounded::output_command_clean_env(
                &mut command,
                wall_time,
                64 * 1024,
                plurx_core::process::ChildWork::background(
                    "complete-title Dolby Vision scan (content-analysis pass)",
                ),
            )
            .await
            .map_err(|error| error.to_string());
            drop(self); // Keep the whole input, not independently captured fields.
            output
        };
        let work = async move {
            match cancel {
                Some(cancel) => bounded::cancellable(cancel, work).await,
                None => work.await,
            }
        };
        match keeper {
            Some(keeper) => bounded::with_original_work_keeper(keeper, work).await,
            None => work.await,
        }
    }
}

/// Judge the companion scan's output exactly as the title job judges its own.
fn dv_title_aggregate(
    output: &plurx_core::process::bounded::Output,
    scanner_current: bool,
) -> Result<plurx_core::scan::dv_title::DvTitleStats, String> {
    use plurx_core::scan::dv_title::{DvTitleStats, TitleEligibility};
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("scanner refused: {}", stderr.trim()));
    }
    if !scanner_current {
        return Err("the installed scanner changed during the pass".to_owned());
    }
    let stats: DvTitleStats = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("scanner aggregate: {error}"))?;
    if !stats.complete || stats.eligibility() == TitleEligibility::InvalidAggregate {
        return Err("incomplete or inconsistent aggregate".to_owned());
    }
    Ok(stats)
}

/// One identity's half of an index pass: what to do about the Dolby Vision
/// record its muxer leaves behind, and the options both that answer and the
/// argv block were derived from — carried so the index key is derived from
/// them too. Passed separately, a caller could hand the runner a recipe built
/// from one set of options and an identity computed from another — the rows
/// would be byte counts for one stream filed under another's key, which is
/// the same class of mismatch as the argv and the record answer disagreeing.
struct IndexOutput {
    video: transcode::CopyVideoOptions,
    dolby_vision: DolbyVisionPass,
}

impl IndexOutput {
    fn new(file: &MediaFile, video: transcode::CopyVideoOptions) -> Result<Self, String> {
        Ok(Self {
            video,
            dolby_vision: dolby_vision_pass_for(file, video)?,
        })
    }
}

/// How long a built pass waits for its companion title scan beyond the pass
/// budget. The scan reads what the pass just read, from the page cache.
#[cfg(target_os = "linux")]
const DV_TITLE_SCAN_GRACE: Duration = Duration::from_secs(10 * 60);

/// The most identities one pass writes. A file has at most three (stripped,
/// preserved, converted); the bound keeps the child's fd plumbing fixed-size.
const MAX_PASS_OUTPUTS: usize = 4;

fn index_pass(
    file: &MediaFile,
    video: transcode::CopyVideoOptions,
    input: Option<&str>,
    expectation: VideoCompletionExpectation,
    ride_along: Option<RideAlongPlan>,
) -> Result<IndexPass, String> {
    Ok(shared_index_pass(
        file,
        vec![IndexOutput::new(file, video)?],
        input,
        expectation,
        ride_along,
    ))
}

fn shared_index_pass(
    file: &MediaFile,
    outputs: Vec<IndexOutput>,
    input: Option<&str>,
    expectation: VideoCompletionExpectation,
    ride_along: Option<RideAlongPlan>,
) -> IndexPass {
    let videos = outputs
        .iter()
        .map(|output| output.video)
        .collect::<Vec<_>>();
    IndexPass {
        args: shared_index_argv(file, &videos, input, ride_along.as_ref()),
        outputs,
        expectation,
        ride_along,
    }
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
    shared_index_argv(file, &[video], input, ride_along)
}

/// [`index_argv`] for several identities read once: the ride-along follows
/// the last output block, so the tracks are kept once per pass.
fn shared_index_argv(
    file: &MediaFile,
    videos: &[transcode::CopyVideoOptions],
    input: Option<&str>,
    ride_along: Option<&RideAlongPlan>,
) -> Vec<String> {
    let mut args = match input {
        Some(path) => transcode::copy_index_shared_args_with_input(file, path, videos),
        None => {
            transcode::copy_index_shared_args_with_input(file, &file.path.to_string_lossy(), videos)
        }
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
    /// The Dolby Vision title aggregate the pass's companion scan read, on
    /// the pass's first built output.
    pub(crate) dv_title: Option<PendingDvTitle>,
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
            dv_title: None,
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
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<(ProbedSource, Instant), IndexFailure> {
    let started = Instant::now();
    match probe_completion_expectation_inner(source, source_object_version, budget, started, cancel)
        .await
    {
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
    cancel: Option<&tokio_util::sync::CancellationToken>,
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
    let result = crate::ffmpeg::held_source_index_probe_json(
        source,
        budget.saturating_sub(started.elapsed()),
        cancel,
    )
    .await;
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

trait SourceIndexHooks: std::any::Any + Send + Sync {
    fn before_spawn(&self) -> crate::seam_hooks::HookFuture<'_>;
    fn after_spawn(&self, pid: u32) -> crate::seam_hooks::HookFuture<'_>;
    fn before_reap_retry(&self) -> crate::seam_hooks::HookFuture<'_>;
    fn inject_wait_failure(&self) -> bool;
    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any;
}
struct NoSourceIndexHooks;
impl SourceIndexHooks for NoSourceIndexHooks {
    fn before_spawn(&self) -> crate::seam_hooks::HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }
    fn after_spawn(&self, _: u32) -> crate::seam_hooks::HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }
    fn before_reap_retry(&self) -> crate::seam_hooks::HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }
    fn inject_wait_failure(&self) -> bool {
        false
    }
    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
static NO_SOURCE_INDEX_HOOKS: NoSourceIndexHooks = NoSourceIndexHooks;
/// Per actual manager, with identical no-op hook fields/awaits in production.
pub(crate) struct SourceIndexHookOwner {
    slot: crate::seam_hooks::HookSlot<dyn SourceIndexHooks>,
}
impl Default for SourceIndexHookOwner {
    fn default() -> Self {
        Self {
            slot: crate::seam_hooks::HookSlot::new(&NO_SOURCE_INDEX_HOOKS),
        }
    }
}
#[cfg(test)]
#[derive(Default)]
struct PausingSourceIndexHooks {
    before: crate::seam_hooks::PauseSlot,
    after: crate::seam_hooks::PauseSlot,
    retry: crate::seam_hooks::PauseSlot,
    failed_wait: std::sync::atomic::AtomicBool,
    spawned_pid: std::sync::atomic::AtomicU32,
    closed_parents: std::sync::atomic::AtomicUsize,
    open_parent_at_settlement: std::sync::atomic::AtomicBool,
}
#[cfg(test)]
impl SourceIndexHooks for PausingSourceIndexHooks {
    fn before_spawn(&self) -> crate::seam_hooks::HookFuture<'_> {
        self.before.hold()
    }
    fn after_spawn(&self, pid: u32) -> crate::seam_hooks::HookFuture<'_> {
        self.spawned_pid
            .store(pid, std::sync::atomic::Ordering::Release);
        self.after.hold()
    }
    fn before_reap_retry(&self) -> crate::seam_hooks::HookFuture<'_> {
        self.retry.hold()
    }
    fn inject_wait_failure(&self) -> bool {
        self.failed_wait
            .swap(false, std::sync::atomic::Ordering::AcqRel)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
impl SourceIndexHookOwner {
    fn record_parent_closed(&self, _closed: bool) {
        #[cfg(all(test, unix))]
        {
            let hooks = self.test_hooks();
            hooks
                .open_parent_at_settlement
                .fetch_or(!_closed, std::sync::atomic::Ordering::Release);
            hooks
                .closed_parents
                .fetch_add(1, std::sync::atomic::Ordering::Release);
        }
    }
    #[cfg(all(test, unix))]
    pub(crate) fn assert_closed_parent_settlements(&self) {
        let hooks = self.test_hooks();
        assert!(
            hooks
                .closed_parents
                .load(std::sync::atomic::Ordering::Acquire)
                > 0
        );
        assert!(!hooks
            .open_parent_at_settlement
            .load(std::sync::atomic::Ordering::Acquire));
    }
}
#[cfg(test)]
impl SourceIndexHookOwner {
    fn test_hooks(&self) -> &PausingSourceIndexHooks {
        self.slot
            .get_or_install(|| Box::new(PausingSourceIndexHooks::default()))
            .as_any()
            .downcast_ref()
            .expect("Source index test hooks")
    }
    pub(crate) fn pause_before_spawn(&self) -> Arc<crate::seam_hooks::AsyncPause> {
        self.test_hooks().before.arm("source_index_before_spawn")
    }
    pub(crate) fn pause_after_spawn(&self) -> Arc<crate::seam_hooks::AsyncPause> {
        self.test_hooks().after.arm("source_index_after_spawn")
    }
    pub(crate) fn pause_after_wait_failure(&self) -> Arc<crate::seam_hooks::AsyncPause> {
        self.test_hooks()
            .failed_wait
            .store(true, std::sync::atomic::Ordering::Release);
        self.test_hooks().retry.arm("source_index_reap_retry")
    }
    pub(crate) fn spawned_pid(&self) -> u32 {
        self.test_hooks()
            .spawned_pid
            .load(std::sync::atomic::Ordering::Acquire)
    }
}
struct SourceIndexExecution<'a> {
    authority: &'a plurx_core::sharing_source_sessions::SourceSessionWriteAuthority,
    store: &'a dyn plurx_core::store::Store,
    source: &'a crate::fragment_index_cluster::SourceFence,
    deadline: Instant,
    hooks: &'a SourceIndexHookOwner,
}

/// Private admitted scan ownership; retained by the Source viewer actor until
/// the actual child and pipe tasks have settled. No wire constructor.
pub(crate) struct SourceIndexOperation {
    assignment: plurx_core::sharing_source_sessions::SourceDispatchAssignment,
    state: Arc<SourceIndexState>,
    cancel: tokio_util::sync::CancellationToken,
}
/// Minted only after this actual scan's child and pipe tasks have settled.
/// It is neither route authority nor a general capacity-release capability.
pub(crate) struct SourceIndexSettlement {
    assignment: plurx_core::sharing_source_sessions::SourceDispatchAssignment,
}
impl SourceIndexSettlement {
    pub(crate) fn matches(
        &self,
        assignment: &plurx_core::sharing_source_sessions::SourceDispatchAssignment,
    ) -> bool {
        self.assignment.same_identity(assignment)
    }
}
struct SourceIndexState {
    permit: std::sync::Mutex<Option<crate::vodencode::EncodePermit>>,
    result: std::sync::Mutex<Option<IndexBuild>>,
    settled: std::sync::atomic::AtomicBool,
    changed: tokio::sync::Notify,
}
impl SourceIndexOperation {
    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }
    pub(crate) async fn settle(&self) -> SourceIndexSettlement {
        loop {
            let notified = self.state.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self
                .state
                .settled
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return SourceIndexSettlement {
                    assignment: self.assignment.clone(),
                };
            }
            notified.await;
        }
    }
    pub(crate) async fn outcome(&self) -> Result<IndexBuild, String> {
        let _receipt = self.settle().await;
        self.state
            .result
            .lock()
            .map_err(|_| "Source index result poisoned".to_owned())?
            .take()
            .ok_or_else(|| "Source index result was already consumed".to_owned())
    }
}

/// The first Source index lane uses current bounded stored H264 completion
/// evidence. Missing/ambiguous evidence never starts a fallback live probe.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_source_index(
    file: MediaFile,
    source: crate::fragment_index_cluster::SourceFence,
    stored_probe: String,
    video: transcode::CopyVideoOptions,
    runtime_cache: std::path::PathBuf,
    deadline: Instant,
    permit: crate::vodencode::EncodePermit,
    authority: Box<plurx_core::sharing_source_sessions::SourceSessionWriteAuthority>,
    store: Arc<dyn plurx_core::store::Store>,
    hooks: Arc<SourceIndexHookOwner>,
) -> Result<SourceIndexOperation, String> {
    if file.video_codec.as_deref() != Some("h264")
        || Instant::now() >= deadline
        || file.id.to_string() != authority.assignment().binding().file_id().as_str()
    {
        return Err("Source index supports bounded H264 copy only".into());
    }
    if stored_probe.len() > 1024 * 1024 {
        return Err("Source index has no bounded stored completion evidence".into());
    }
    let raw = stored_probe.as_str();
    let expectation = completion_expectation_from_probe(raw, source.object_version()).map_err(
        |error| match error {
            CompletionExpectationError::Unsupported(reason)
            | CompletionExpectationError::Unverified(reason) => reason,
        },
    )?;
    #[cfg(unix)]
    let input = "/dev/fd/3".to_owned();
    #[cfg(windows)]
    let input = crate::ffmpeg::windows_source_path(&source.handle)?
        .to_string_lossy()
        .into_owned();
    let mut pass = index_pass(&file, video, Some(&input), expectation, None)?;
    // Same conservative CPU4 resource estimate as Source copy: one decoder,
    // one audio encoder and explicitly bounded filter pools.
    let mut at = 0;
    while at < pass.args.len() {
        if pass.args[at] == "-i" {
            pass.args
                .splice(at..at, ["-threads".to_owned(), "1".to_owned()]);
            at += 2;
        }
        at += 1;
    }
    pass.args.splice(
        0..0,
        [
            "-filter_threads".to_owned(),
            "1".to_owned(),
            "-filter_complex_threads".to_owned(),
            "1".to_owned(),
        ],
    );
    let output = pass.args.len().saturating_sub(1);
    pass.args
        .splice(output..output, ["-threads:a".to_owned(), "1".to_owned()]);
    let state = Arc::new(SourceIndexState {
        permit: std::sync::Mutex::new(Some(permit)),
        result: std::sync::Mutex::new(None),
        settled: std::sync::atomic::AtomicBool::new(false),
        changed: tokio::sync::Notify::new(),
    });
    let cancel = tokio_util::sync::CancellationToken::new();
    let assignment = authority.assignment().clone();
    let owned = Arc::clone(&state);
    let owned_cancel = cancel.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        let (fd, handoff) = {
            use std::os::fd::AsRawFd;
            (Some(source.handle.as_raw_fd()), None)
        };
        #[cfg(windows)]
        let (fd, handoff) = (None, Some((&source.handle, std::path::Path::new(&input))));
        let mut result = if Instant::now() >= deadline
            || !source.unchanged()
            || authority
                .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
                .is_err()
        {
            IndexBuild::plain(IndexOutcome::Unsupported(
                "Source index authority or start deadline expired".into(),
            ))
        } else {
            let execution = SourceIndexExecution {
                authority: &authority,
                store: store.as_ref(),
                source: &source,
                deadline,
                hooks: &hooks,
            };
            first_build(
                build_with_args_owned(
                    &file,
                    pass,
                    fd,
                    handoff,
                    &runtime_cache,
                    deadline.saturating_duration_since(Instant::now()),
                    Instant::now(),
                    None,
                    Some(&owned_cancel),
                    Some(&execution),
                )
                .await,
            )
        };
        result.source_unchanged = source.unchanged();
        *owned.result.lock().expect("Source index result") = Some(result);
        crate::transcode::source_preparation::close_source_before_settlement(source, |closed| {
            hooks.record_parent_closed(closed);
        });
        // Publish settlement only after dropping the parent descriptor and actual permit.
        drop(owned.permit.lock().expect("Source index permit").take());
        owned
            .settled
            .store(true, std::sync::atomic::Ordering::Release);
        owned.changed.notify_waiters();
    });
    Ok(SourceIndexOperation {
        assignment,
        state,
        cancel,
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
    first_build(
        build_riding_shared(file, &[video], runtime_cache, budget, ride_along, progress).await,
    )
}

/// [`build_riding`] for several identities from one read of the held source,
/// in `videos` order. Every build carries the same `source_unchanged`: one
/// held source, one fence.
pub(crate) async fn build_riding_shared(
    file: &MediaFile,
    videos: &[transcode::CopyVideoOptions],
    runtime_cache: &Path,
    budget: Duration,
    ride_along: Option<&RideAlongGate>,
    progress: Option<SharedIndexProgress>,
) -> Vec<IndexBuild> {
    let source = match crate::fragment_index_cluster::open_source_fence(file, None).await {
        Ok(source) => source,
        Err(reason) => {
            return videos
                .iter()
                .map(|_| {
                    IndexBuild::plain(IndexOutcome::Failed(Box::new(
                        IndexFailure::new(IndexFailureCode::IndexSourceIo, reason.clone(), 0)
                            .transient(true),
                    )))
                })
                .collect();
        }
    };
    let mut builds = build_shared_attested(
        file,
        &source.handle,
        source.object_version(),
        videos,
        runtime_cache,
        budget,
        progress,
        ride_along,
        None,
    )
    .await;
    let unchanged = source.unchanged();
    for built in &mut builds {
        built.source_unchanged = unchanged;
    }
    builds
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
        None,
    )
    .await
    .outcome
}

/// One identity, progress reported and PGS tracks kept: the shape tests drive
/// a single pass through. The worker builds through
/// [`build_shared_from_attested_file_with_progress`].
#[cfg(test)]
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
    cancel: &tokio_util::sync::CancellationToken,
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
        Some(cancel),
    )
    .await
}

/// The cluster worker's build: every identity in `videos` from one read of the
/// held source ([`build_shared_attested`]), in `videos` order, with progress
/// reported and the file's PGS tracks kept when `ride_along` allows. The
/// worker publishes the harvest only after its own `source_still_matches` and
/// `still_current` checks.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn build_shared_from_attested_file_with_progress<F>(
    file: &MediaFile,
    source: &std::fs::File,
    source_object_version: &str,
    videos: &[transcode::CopyVideoOptions],
    runtime_cache: &Path,
    budget: Duration,
    progress: F,
    ride_along: Option<&RideAlongGate>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Vec<IndexBuild>
where
    F: Fn(&PassProgress) + Send + Sync + 'static,
{
    build_shared_attested(
        file,
        source,
        source_object_version,
        videos,
        runtime_cache,
        budget,
        Some(Arc::new(progress)),
        ride_along,
        Some(cancel),
    )
    .await
}

/// One identity from one read of the held source: [`build_attested_pass`]
/// with a single output, which is today's pass byte for byte.
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
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> IndexBuild {
    first_build(
        build_attested_pass(
            file,
            source,
            source_object_version,
            &[video],
            runtime_cache,
            budget,
            progress,
            ride_along,
            cancel,
        )
        .await,
    )
}

/// The one build of a one-identity pass.
fn first_build(builds: Vec<IndexBuild>) -> IndexBuild {
    builds.into_iter().next().unwrap_or_else(|| {
        IndexBuild::plain(IndexOutcome::Failed(Box::new(IndexFailure::new(
            IndexFailureCode::IndexProcessFailed,
            "an index pass returned no build for its identity",
            0,
        ))))
    })
}

/// Every identity in `videos` from one read of the held source: one probe,
/// one ffmpeg child with an output per identity, one index per output, in
/// `videos` order.
///
/// What changes is execution, never identity. Each output block is the
/// one-identity argv's, so each index is keyed, proved and published exactly
/// as a pass of its own would be — `pipeline_digest_for_transform` still
/// hashes the one-identity argv. An identity whose Dolby Vision record
/// cannot be described is refused alone; the rest still share the read.
#[cfg(not(windows))]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn build_shared_attested(
    file: &MediaFile,
    source: &std::fs::File,
    source_object_version: &str,
    videos: &[transcode::CopyVideoOptions],
    runtime_cache: &Path,
    budget: Duration,
    progress: Option<SharedIndexProgress>,
    ride_along: Option<&RideAlongGate>,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Vec<IndexBuild> {
    build_attested_pass(
        file,
        source,
        source_object_version,
        videos,
        runtime_cache,
        budget,
        progress,
        ride_along,
        cancel,
    )
    .await
}

/// Windows builds one identity per pass: its source reaches ffmpeg by path,
/// not by inherited descriptor, and so would the extra outputs. Only the
/// first pass rides along, so the tracks are still kept once.
#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn build_shared_attested(
    file: &MediaFile,
    source: &std::fs::File,
    source_object_version: &str,
    videos: &[transcode::CopyVideoOptions],
    runtime_cache: &Path,
    budget: Duration,
    progress: Option<SharedIndexProgress>,
    ride_along: Option<&RideAlongGate>,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Vec<IndexBuild> {
    let mut builds = Vec::with_capacity(videos.len());
    for (at, video) in videos.iter().enumerate() {
        builds.extend(
            build_attested_pass(
                file,
                source,
                source_object_version,
                std::slice::from_ref(video),
                runtime_cache,
                budget,
                progress.clone(),
                ride_along.filter(|_| at == 0),
                cancel,
            )
            .await,
        );
    }
    builds
}

/// The one place every index build passes through — the cluster worker and
/// the non-cluster pass alike — after the held-fd probe and before the pass.
/// That is where the ride-along's latch is checked: the first point at which
/// the file's PGS ordinals exist, read from the descriptor the pass will read.
#[allow(clippy::too_many_arguments)]
async fn build_attested_pass(
    file: &MediaFile,
    source: &std::fs::File,
    source_object_version: &str,
    videos: &[transcode::CopyVideoOptions],
    runtime_cache: &Path,
    budget: Duration,
    progress: Option<SharedIndexProgress>,
    ride_along: Option<&RideAlongGate>,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Vec<IndexBuild> {
    let refuse_all = |outcome: IndexOutcome| {
        videos
            .iter()
            .map(|_| IndexBuild::plain(outcome.clone()))
            .collect::<Vec<_>>()
    };
    if videos.is_empty() {
        return Vec::new();
    }
    if videos.len() > MAX_PASS_OUTPUTS {
        return refuse_all(IndexOutcome::Unsupported(format!(
            "one index pass writes at most {MAX_PASS_OUTPUTS} identities, not {}",
            videos.len()
        )));
    }
    #[cfg(windows)]
    let path = match crate::ffmpeg::windows_source_path(source) {
        Ok(path) => path,
        Err(reason) => return refuse_all(IndexOutcome::Unsupported(reason)),
    };
    let (probed, started) =
        match probe_completion_expectation(source, source_object_version, budget, cancel).await {
            Ok(value) => value,
            Err(failure) => return refuse_all(IndexOutcome::Failed(Box::new(failure))),
        };
    // An identity whose recipe cannot be described is refused on its own;
    // that says nothing about its siblings, which still share the read.
    let mut builds: Vec<Option<IndexBuild>> = videos.iter().map(|_| None).collect();
    let mut outputs = Vec::with_capacity(videos.len());
    let mut slots = Vec::with_capacity(videos.len());
    for (slot, video) in videos.iter().enumerate() {
        match IndexOutput::new(file, *video) {
            Ok(output) => {
                outputs.push(output);
                slots.push(slot);
            }
            Err(reason) => {
                builds[slot] = Some(IndexBuild::plain(IndexOutcome::Unsupported(reason)));
            }
        }
    }
    if !outputs.is_empty() {
        let plan = match ride_along {
            Some(gate) => {
                crate::subtitle_ride_along::plan_tracks(
                    gate,
                    file.id,
                    source,
                    &probed.subtitle_tracks,
                )
                .await
            }
            None => None,
        };
        #[cfg(unix)]
        let input = "/dev/fd/3".to_owned();
        #[cfg(windows)]
        let input = path.to_string_lossy().into_owned();
        #[cfg(target_os = "linux")]
        let dv_title = match DvTitleRide::for_file(file, probed.expectation.stream_index).await {
            Some(ride) => ride.spawn(source, budget, cancel).map(|task| (ride, task)),
            None => None,
        };
        let pass = shared_index_pass(file, outputs, Some(&input), probed.expectation, plan);
        #[cfg(unix)]
        let (source_fd, source_handoff) = {
            use std::os::fd::AsRawFd;
            (Some(source.as_raw_fd()), None)
        };
        #[cfg(windows)]
        let (source_fd, source_handoff) = (None, Some((source, path.as_path())));
        let built = build_with_args_shared(
            file,
            pass,
            source_fd,
            source_handoff,
            runtime_cache,
            budget,
            started,
            progress,
            cancel,
        )
        .await;
        #[allow(unused_mut)]
        let mut built = built;
        #[cfg(target_os = "linux")]
        if let Some((ride, task)) = dv_title {
            let lead_built = matches!(
                built.first().map(|build| &build.outcome),
                Some(IndexOutcome::Built(_))
            );
            let wait = budget
                .saturating_sub(started.elapsed())
                .max(DV_TITLE_SCAN_GRACE);
            let pending = ride.finish(file.id, task, lead_built, wait).await;
            if let Some(first) = built.first_mut() {
                first.dv_title = pending;
            }
        }
        for (slot, build) in slots.into_iter().zip(built) {
            builds[slot] = Some(build);
        }
    }
    builds
        .into_iter()
        .map(|build| {
            build.unwrap_or_else(|| {
                IndexBuild::plain(IndexOutcome::Failed(Box::new(IndexFailure::new(
                    IndexFailureCode::IndexProcessFailed,
                    "the shared index pass returned no build for this identity",
                    0,
                ))))
            })
        })
        .collect()
}

#[cfg(test)]
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
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> IndexBuild {
    first_build(
        build_with_args_shared(
            file,
            pass,
            source_fd,
            source_handoff,
            runtime_cache,
            budget,
            started,
            progress,
            cancel,
        )
        .await,
    )
}

#[allow(clippy::too_many_arguments)]
async fn build_with_args_shared(
    file: &MediaFile,
    pass: IndexPass,
    source_fd: Option<SourceFd>,
    source_handoff: Option<(&std::fs::File, &Path)>,
    runtime_cache: &Path,
    budget: Duration,
    started: Instant,
    progress: Option<SharedIndexProgress>,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Vec<IndexBuild> {
    build_with_args_owned(
        file,
        pass,
        source_fd,
        source_handoff,
        runtime_cache,
        budget,
        started,
        progress,
        cancel,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn build_with_args_owned(
    file: &MediaFile,
    pass: IndexPass,
    source_fd: Option<SourceFd>,
    source_handoff: Option<(&std::fs::File, &Path)>,
    runtime_cache: &Path,
    budget: Duration,
    started: Instant,
    progress: Option<SharedIndexProgress>,
    cancel: Option<&tokio_util::sync::CancellationToken>,
    source_owned: Option<&SourceIndexExecution<'_>>,
) -> Vec<IndexBuild> {
    let IndexPass {
        args,
        outputs,
        expectation,
        ride_along,
    } = pass;
    let identities = outputs
        .iter()
        .map(|output| identity_for(file, output.video))
        .collect::<Vec<_>>();
    // Every output of one pass ends the same way unless its own stream says
    // otherwise: one child, one exit status, one budget.
    let all = |outcome: IndexOutcome| {
        identities
            .iter()
            .map(|_| IndexBuild::plain(outcome.clone()))
            .collect::<Vec<_>>()
    };
    if outputs.is_empty() {
        return Vec::new();
    }
    if outputs.len() > MAX_PASS_OUTPUTS || (source_owned.is_some() && outputs.len() > 1) {
        return all(IndexOutcome::Unsupported(format!(
            "this index pass cannot write {} identities at once",
            outputs.len()
        )));
    }
    if let Some(plan) = &ride_along {
        tracing::info!(
            file_id = file.id,
            tracks = ?plan.tracks(),
            "the fragment-index pass is also keeping this file's PGS tracks"
        );
    }
    if outputs.len() > 1 {
        tracing::info!(
            file_id = file.id,
            identities = outputs.len(),
            "one fragment-index pass is building every identity of this file from one read"
        );
    }

    if cancel.is_some_and(|token| token.is_cancelled()) {
        return all(IndexOutcome::Unsupported("index cancelled".into()));
    }
    // The siblings' pipes: output `i` writes to fd `3 + i` in the child.
    // Both ends are close-on-exec, so no other child inherits a writer and
    // keeps a reader here from ever seeing EOF; the child gets its writers
    // only through the explicit `dup2` below.
    #[cfg(unix)]
    let mut sibling_pipes = Vec::with_capacity(outputs.len().saturating_sub(1));
    #[cfg(unix)]
    for _ in 1..outputs.len() {
        match sibling_pipe() {
            Ok(pipe) => sibling_pipes.push(pipe),
            Err(error) => {
                return all(IndexOutcome::Failed(Box::new(
                    IndexFailure::new(
                        IndexFailureCode::IndexProcessFailed,
                        format!("creating a shared index pass's output pipe: {error}"),
                        0,
                    )
                    .transient(true),
                )));
            }
        }
    }
    #[cfg(not(unix))]
    if outputs.len() > 1 {
        return all(IndexOutcome::Unsupported(
            "this platform builds one identity per index pass".into(),
        ));
    }
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    if ride_along.is_some() {
        if let Err(error) = crate::subtitle_ride_along::configure_stage_limits(&mut command) {
            return all(IndexOutcome::Unsupported(format!(
                "bounded subtitle output unavailable: {error}"
            )));
        }
    }
    crate::producer_spawn::configure_ffmpeg_runtime(&mut command, runtime_cache);
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let mut inherited = InheritedFds::default();
        if let Some(source_fd) = source_fd {
            inherited.push(source_fd, 3);
        }
        for (at, (_, writer)) in sibling_pipes.iter().enumerate() {
            inherited.push(writer.as_raw_fd(), 4 + at as SourceFd);
        }
        if inherited.len > 0 {
            // SAFETY: `inherit` makes only async-signal-safe calls (`fcntl`,
            // `dup2`, `close`) over a fixed-size array; it allocates nothing.
            unsafe {
                command.pre_exec(move || inherited.inherit());
            }
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
            return all(IndexOutcome::Failed(Box::new(IndexFailure::new(
                IndexFailureCode::IndexSourceIo,
                reason,
                0,
            ))));
        }
    }
    #[cfg(not(windows))]
    let _ = source_handoff;
    if let Some(execution) = source_owned {
        execution.hooks.slot.get().before_spawn().await;
        let now = crate::fragment_index_cluster::unix_ms();
        if Instant::now() >= execution.deadline
            || cancel.is_some_and(|token| token.is_cancelled())
            || execution
                .authority
                .validate_observation_freshness(now)
                .is_err()
        {
            return all(IndexOutcome::Unsupported(
                "Source scan permission expired before spawn".into(),
            ));
        }
        let physical = crate::fragment_index_cluster::open_source_playback_fence(
            file,
            Some(execution.source.object_version()),
        )
        .await;
        if physical.is_err() {
            return all(IndexOutcome::Unsupported(
                "Source scan physical identity changed".into(),
            ));
        }
        if !matches!(
            execution
                .store
                .authorize_source_index_preparation(execution.authority)
                .await,
            Ok(true)
        ) || execution
            .authority
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .is_err()
            || Instant::now() >= execution.deadline
            || cancel.is_some_and(|token| token.is_cancelled())
        {
            return all(IndexOutcome::Unsupported(
                "Source scan current authority unavailable".into(),
            ));
        }
    }
    let (mut child, _child_job) = match crate::process_control::spawn_job_owned(
        &mut command,
        crate::process_control::ChildWork::background("fragment index"),
    ) {
        Ok(owned) => owned,
        Err(error) => {
            let transient = matches!(
                error.kind(),
                std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::OutOfMemory
            );
            return all(IndexOutcome::Failed(Box::new(
                IndexFailure::new(
                    IndexFailureCode::IndexProcessFailed,
                    format!("spawning the job-owned index pipe: {error}"),
                    0,
                )
                .transient(transient),
            )));
        }
    };
    // The child holds its own copies now. Ours must close, or a sibling's
    // reader would wait forever for the EOF our copy keeps from it.
    #[cfg(unix)]
    let sibling_readers = sibling_pipes
        .into_iter()
        .map(|(reader, writer)| {
            drop(writer);
            tokio::net::unix::pipe::Receiver::from_owned_fd(reader)
        })
        .collect::<Result<Vec<_>, _>>();
    if let Some(execution) = source_owned {
        execution
            .hooks
            .slot
            .get()
            .after_spawn(child.id().unwrap_or(0))
            .await;
    }
    let stdout = child.stdout.take();
    #[cfg(unix)]
    let sibling_readers = match (stdout.is_some(), sibling_readers) {
        (true, Ok(readers)) => Some(readers),
        _ => None,
    };
    #[cfg(not(unix))]
    let sibling_readers: Option<Vec<tokio::process::ChildStdout>> = stdout.is_some().then(Vec::new);
    let (Some(stdout), Some(sibling_readers)) = (stdout, sibling_readers) else {
        if source_owned.is_some() {
            let _ = child.start_kill();
            loop {
                match child.wait().await {
                    Ok(_) => break,
                    Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
                }
            }
        }
        return all(IndexOutcome::Failed(Box::new(IndexFailure::new(
            IndexFailureCode::IndexProcessFailed,
            "the index pipe started without every output it was given",
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
    // Every output is read at once: ffmpeg writes them in lockstep from one
    // demuxer, so an output nobody reads would stall the others behind a full
    // pipe. The lead's stream reports progress for the pass.
    //
    // In a shared pass an output whose own stream is refused — a converting
    // identity over a stream with no RPUs, say — keeps being read to EOF, so
    // its refusal stays its own and its siblings still build, exactly as each
    // would have in a pass of its own. A one-identity pass keeps today's
    // behaviour: its reader closes with the refusal and the child ends early.
    let shared = identities.len() > 1;
    let mut dolby_vision = outputs.into_iter().map(|output| output.dolby_vision);
    let mut streams: Vec<OutputRead<'_>> = Vec::with_capacity(identities.len());
    streams.push(Box::pin(index_output(
        stdout,
        identities[0].clone(),
        &expectation,
        dolby_vision.next().unwrap_or_default(),
        Some(&record_progress),
        shared,
    )));
    for (reader, identity) in sibling_readers.into_iter().zip(identities.iter().skip(1)) {
        streams.push(Box::pin(index_output(
            reader,
            identity.clone(),
            &expectation,
            dolby_vision.next().unwrap_or_default(),
            None,
            shared,
        )));
    }
    let stream = tokio::time::timeout(
        budget.saturating_sub(started.elapsed()),
        futures_util::future::join_all(streams),
    );
    tokio::pin!(stream);
    let result = tokio::select! {
        result = &mut stream => Some(result),
        () = async { match cancel { Some(token) => token.cancelled().await, None => std::future::pending().await } } => None,
    };
    let mut early = vec![false; identities.len()];
    let (mut outcomes, deadline_fired) = match result {
        Some(Ok(answers)) => {
            let mut outcomes = Vec::with_capacity(answers.len());
            for (at, (outcome, refused_early)) in answers.into_iter().enumerate() {
                early[at] = refused_early;
                outcomes.push(outcome);
            }
            (outcomes, false)
        }
        Some(Err(_)) | None => {
            let rows = observed.lock().map(|value| value.2).unwrap_or_default();
            (
                identities
                    .iter()
                    .map(|_| {
                        IndexOutcome::Failed(Box::new(IndexFailure::new(
                            IndexFailureCode::IndexBudgetExceeded,
                            format!("exceeded the {}s index budget", budget.as_secs()),
                            rows,
                        )))
                    })
                    .collect::<Vec<_>>(),
                true,
            )
        }
    };
    if deadline_fired {
        let _ = child.start_kill();
    }
    let status =
        if source_owned.is_some_and(|execution| execution.hooks.slot.get().inject_wait_failure()) {
            Ok(Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "injected initial Source scan wait failure",
            )))
        } else {
            tokio::time::timeout(Duration::from_secs(5), child.wait()).await
        };
    let rows = observed.lock().map(|value| value.2).unwrap_or_default();
    let exit_category = match status {
        Ok(Ok(status)) if status.success() => Some("success".to_owned()),
        Ok(Ok(status)) => {
            if !deadline_fired {
                // One exit status for every output: a pass that failed did
                // not build any of them, whatever a stream managed to parse.
                // A stream's own refusal stands: it is a statement about the
                // content, and a reader that refused closes its pipe, which is
                // itself why ffmpeg exits non-zero.
                for outcome in outcomes
                    .iter_mut()
                    .zip(&early)
                    .filter(|(outcome, early)| !is_refusal(outcome, **early))
                    .map(|(outcome, _)| outcome)
                {
                    *outcome = IndexOutcome::Failed(Box::new(IndexFailure::new(
                        IndexFailureCode::IndexProcessFailed,
                        format!("index pipe exited with {status}"),
                        rows,
                    )));
                }
            }
            Some(format!("{status}"))
        }
        Ok(Err(error)) => {
            if !deadline_fired {
                let transient = matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                );
                for outcome in outcomes
                    .iter_mut()
                    .zip(&early)
                    .filter(|(outcome, early)| !is_refusal(outcome, **early))
                    .map(|(outcome, _)| outcome)
                {
                    *outcome = IndexOutcome::Failed(Box::new(
                        IndexFailure::new(
                            IndexFailureCode::IndexProcessFailed,
                            format!("waiting for the index pipe: {error}"),
                            rows,
                        )
                        .transient(transient),
                    ));
                }
            }
            Some("wait_failed".to_owned())
        }
        Err(_) => {
            let _ = child.start_kill();
            // A second wait is required after escalating to kill; otherwise
            // the process can remain unreaped while the stderr task is
            // abandoned below.
            let _ = child.wait().await;
            if !deadline_fired {
                for outcome in outcomes
                    .iter_mut()
                    .zip(&early)
                    .filter(|(outcome, early)| !is_refusal(outcome, **early))
                    .map(|(outcome, _)| outcome)
                {
                    *outcome = IndexOutcome::Failed(Box::new(IndexFailure::new(
                        IndexFailureCode::IndexProcessFailed,
                        "index pipe did not exit within the five-second grace",
                        rows,
                    )));
                }
            }
            Some("exit_grace_exceeded".to_owned())
        }
    };
    if source_owned.is_some()
        && matches!(
            exit_category.as_deref(),
            Some("wait_failed" | "exit_grace_exceeded")
        )
    {
        // The private Source owner retains its actual permit and ChildJob here.
        // Killing or a timed-out wait is never physical settlement.
        let _ = child.start_kill();
        if let Some(execution) = source_owned {
            execution.hooks.slot.get().before_reap_retry().await;
        }
        loop {
            match child.wait().await {
                Ok(_) => break,
                Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
            }
        }
    }
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
    finish_header_scans(
        &mut outcomes,
        matches!(file.video_codec.as_deref(), Some("hevc" | "h265")),
        hevc_trace,
        &expectation,
    );
    for outcome in &mut outcomes {
        if let IndexOutcome::Failed(failure) = outcome {
            // The lead's progress stands for the pass: every output is cut
            // from the same packets, in lockstep.
            let (output_bytes, covered_ms, rows) =
                observed.lock().map(|value| *value).unwrap_or_default();
            failure.rows = failure.rows.max(rows);
            failure.diagnostic.fragment_count =
                Some(u32::try_from(failure.rows).unwrap_or(u32::MAX));
            failure.diagnostic.output_bytes = Some(output_bytes);
            failure.diagnostic.covered_ms = Some(covered_ms);
            failure.diagnostic.expected_ms = expectation.duration_ms_floor();
            failure.diagnostic.container_ms = file.duration_ms;
            failure.diagnostic.selected_stream = Some(expectation.stream_index);
            failure.diagnostic.expectation_provenance =
                Some(expectation.provenance.as_str().to_owned());
            failure.diagnostic.elapsed_ms =
                Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
            failure.diagnostic.budget_ms =
                Some(u64::try_from(budget.as_millis()).unwrap_or(u64::MAX));
            failure.diagnostic.exit_category = exit_category.clone();
            failure.diagnostic.stderr_tail = stderr_tail.clone();
        }
        if let IndexOutcome::Built(index) = &*outcome {
            tracing::info!(
                file_id = file.id,
                fragments = index.rows.len(),
                timescale = index.timescale,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "built a fragment index"
            );
        }
    }
    // The child has been reaped (a `Built` outcome means it exited 0 within
    // the grace), so no slave can still be flushing; the verdict itself is
    // the caller's, outside this future. The tracks belong to the pass, and
    // they travel with its first built output: kept when one built,
    // discarded with the plan otherwise, and the file's next pass does not ride — a riding
    // pass that failed must not be able to fail the same way on every retry.
    let carrier = outcomes
        .iter()
        .position(|outcome| matches!(outcome, IndexOutcome::Built(_)));
    let mut ride_along = match (ride_along, carrier) {
        (Some(plan), Some(_)) => Some(PendingRideAlong::new(plan, scan)),
        (Some(plan), None) => {
            crate::subtitle_ride_along::record_failed_ride(&plan);
            None
        }
        (None, _) => None,
    };
    outcomes
        .into_iter()
        .enumerate()
        .map(|(at, outcome)| IndexBuild {
            outcome,
            ride_along: if Some(at) == carrier {
                ride_along.take()
            } else {
                None
            },
            dv_title: None,
            source_unchanged: true,
        })
        .collect()
}

/// One output's read, boxed so a pass can read stdout and pipe outputs side
/// by side: the outcome, and whether it was a refusal before the output ended.
type OutputRead<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = (IndexOutcome, bool)> + Send + 'a>>;

/// One output of an index pass: its index, and — in a shared pass — the rest
/// of its pipe read and discarded once the index has its answer, so a refused
/// output never holds its siblings behind a full pipe.
async fn index_output<R: AsyncRead + Unpin>(
    mut reader: R,
    identity: SourceIdentity,
    expectation: &VideoCompletionExpectation,
    dolby_vision: DolbyVisionPass,
    progress: Option<&IndexProgress>,
    drain: bool,
) -> (IndexOutcome, bool) {
    let outcome = index_stream_with_progress(
        &mut reader,
        identity,
        Some(expectation),
        dolby_vision,
        progress,
    )
    .await;
    // Whether the stream answered before its output ended: a refusal of
    // content the stream saw, not the empty or short output of a child that
    // failed. Only the former may stand against a failed exit status.
    let early = !matches!(outcome, IndexOutcome::Built(_)) && {
        let mut byte = [0_u8; 1];
        matches!(reader.read(&mut byte).await, Ok(read) if read > 0)
    };
    if drain {
        let _ = tokio::io::copy(&mut reader, &mut tokio::io::sink()).await;
    }
    (outcome, early)
}

/// A stream's own refusal of content it saw before its output ended, which
/// no exit status overrides: the refusing reader closing its pipe is itself
/// why a one-identity child exits non-zero.
fn is_refusal(outcome: &IndexOutcome, early: bool) -> bool {
    early && matches!(outcome, IndexOutcome::Unsupported(_))
}

/// The read and write ends of one sibling output's pipe, both close-on-exec
/// (`std::io::pipe` sets it on every platform, including macOS, which has no
/// `pipe2`).
#[cfg(unix)]
fn sibling_pipe() -> std::io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    let (reader, writer) = std::io::pipe()?;
    Ok((reader.into(), writer.into()))
}

/// The descriptors an index child receives at fixed numbers: the held source
/// at 3 and each sibling output's writer at 4, 5, …. Fixed-size, so the
/// `pre_exec` hook that installs them allocates nothing.
#[cfg(unix)]
#[derive(Clone, Copy, Default)]
struct InheritedFds {
    from: [SourceFd; MAX_PASS_OUTPUTS + 1],
    to: [SourceFd; MAX_PASS_OUTPUTS + 1],
    len: usize,
}

#[cfg(unix)]
impl InheritedFds {
    fn push(&mut self, from: SourceFd, to: SourceFd) {
        self.from[self.len] = from;
        self.to[self.len] = to;
        self.len += 1;
    }

    /// Runs in the child between `fork` and `exec`. Every source is first
    /// duplicated above the target range, so installing one target can never
    /// close another's source — a writer that happens to be fd 4 is not lost
    /// when the held source lands on 3.
    fn inherit(&self) -> std::io::Result<()> {
        let mut high = [0 as SourceFd; MAX_PASS_OUTPUTS + 1];
        for (slot, from) in high.iter_mut().zip(&self.from).take(self.len) {
            // SAFETY: plain descriptor syscalls on descriptors this process
            // holds; async-signal-safe.
            let duplicate = unsafe { libc::fcntl(*from, libc::F_DUPFD_CLOEXEC, 10) };
            if duplicate == -1 {
                return Err(std::io::Error::last_os_error());
            }
            *slot = duplicate;
        }
        for (duplicate, to) in high.iter().zip(&self.to).take(self.len) {
            // SAFETY: as above.
            unsafe {
                if libc::dup2(*duplicate, *to) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(*duplicate);
                let flags = libc::fcntl(*to, libc::F_GETFD);
                if flags == -1 || libc::fcntl(*to, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }
        Ok(())
    }
}

/// Attach the pass's one HEVC header proof to every index it built. The
/// trace was taken from the lead output, before any other filter in its
/// chain, so it describes the source's parameter sets — which every output
/// of the pass copies.
fn finish_header_scans(
    outcomes: &mut [IndexOutcome],
    hevc: bool,
    trace: Option<plurx_core::hevc_configuration::Trace>,
    expectation: &VideoCompletionExpectation,
) {
    if !outcomes
        .iter()
        .any(|outcome| matches!(outcome, IndexOutcome::Built(_)))
    {
        return;
    }
    let proof = trace.map(|trace| {
        trace.finish(
            expectation.source_object_version.clone(),
            expectation.stream_index,
        )
    });
    for outcome in outcomes {
        if let IndexOutcome::Built(index) = outcome {
            if hevc && proof.is_none() {
                *outcome = IndexOutcome::Failed(Box::new(
                    IndexFailure::new(
                        IndexFailureCode::IndexProcessFailed,
                        "HEVC header scan did not reach stderr EOF; no proof can be published",
                        index.rows.len(),
                    )
                    .transient(true),
                ));
            } else {
                index.promotion.hevc_configuration = proof.clone();
            }
        }
    }
}

#[cfg(test)]
fn finish_header_scan(
    outcome: &mut IndexOutcome,
    hevc: bool,
    trace: Option<plurx_core::hevc_configuration::Trace>,
    expectation: &VideoCompletionExpectation,
) {
    finish_header_scans(std::slice::from_mut(outcome), hevc, trace, expectation);
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
mod tests;

#[cfg(test)]
mod equality_tests;

/// The pass's side of the PGS ride-along: what only `build_with_args` can
/// show — the stage it is handed, and what happens to it when the build
/// future is dropped.
#[cfg(test)]
mod ride_along_tests;
