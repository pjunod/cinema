//! Concrete ordinary AVC writer. No caller future, descriptor export, or
//! detached drain can enter this adapter. Completion remains metadata: the
//! graph must join the original task and root and close every other owner.
use super::{FiniteFile, FiniteOutput};
use crate::adaptive_resources::physical_graph::finite_pipes::FiniteRootPipes;
#[cfg(target_os = "linux")]
use crate::adaptive_resources::{FamilyIdentity, ResourceLedger};
use crate::fmp4::{CutPolicy, PromotionInputs, VideoCodec};
use crate::process::priority::RegisteredChildControl;
use crate::segplan::{PlanEntry, SegmentPlan};
use crate::vod_generation::{
    self, identity::InitIdentity, Failure, Generation, GenerationMemoryLimits, Outcome, Sink,
};
use std::{
    io,
    mem::size_of,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, sync::mpsc};
#[cfg(target_os = "linux")]
mod telemetry;
#[cfg(target_os = "linux")]
use telemetry::FiniteTelemetry;
#[cfg(target_os = "linux")]
pub(in crate::adaptive_resources::runtime) use telemetry::{history_basis, HistoryBasis};

/// Planning data only. Its fields cannot carry callbacks or native close facts.
pub struct SoftwareAvcPumpPlan {
    plan: SegmentPlan,
    start_entry: u32,
    audio_anchor: Option<u64>,
    frame_ticks: u32,
    video_origin: Option<u64>,
    policy: CutPolicy,
    memory: GenerationMemoryLimits,
    diagnostic_bytes: usize,
    metadata_bytes: usize,
    encoded_process: String,
}
impl SoftwareAvcPumpPlan {
    /// Planning allocations already owned by this request. Parser, segmenter,
    /// and future output workspaces are accounted separately at admission.
    pub(in crate::adaptive_resources::runtime) fn requested_heap_bytes(&self) -> Option<usize> {
        self.plan
            .requested_heap_bytes()?
            .checked_add(self.encoded_process.capacity())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        plan: SegmentPlan,
        start_entry: u32,
        grid: crate::transcode::VodFrameGrid,
        policy: CutPolicy,
        memory: GenerationMemoryLimits,
        diagnostic_bytes: usize,
        metadata_bytes: usize,
        encoded_process: String,
    ) -> io::Result<Self> {
        memory.validate().map_err(io::Error::other)?;
        if plan.entries.is_empty()
            || plan.timescale == 0
            || crate::transcode::VodFrameGrid::new(grid.numerator, grid.denominator) != Some(grid)
            || plan.timescale != grid.numerator
            || plan.entry(start_entry).is_none()
            || plan.entries[0].start_ticks != 0
            || diagnostic_bytes == 0
            || diagnostic_bytes > 64 * 1024
            || !(1024..=64 * 1024).contains(&metadata_bytes)
            || encoded_process.is_empty()
            || encoded_process.len() > 1024
            || encoded_process.contains(['\0', '\n', '\r'])
            || policy.max_bytes == 0
        {
            return Err(invalid("invalid finite AVC pump plan"));
        }
        let mut previous_end = 0;
        for (index, entry) in plan.entries.iter().enumerate() {
            let end = entry
                .start_ticks
                .checked_add(entry.duration_ticks)
                .ok_or_else(|| invalid("plan time overflow"))?;
            if usize::try_from(entry.index).ok() != Some(index)
                || entry.duration_ticks == 0
                || (index > 0 && entry.start_ticks != previous_end)
            {
                return Err(invalid("noncontiguous finite VOD plan"));
            }
            previous_end = end;
        }
        let start_seconds = plan
            .entry(start_entry)
            .expect("validated entry")
            .start_ticks as f64
            / f64::from(plan.timescale);
        let result = Self {
            plan,
            start_entry,
            audio_anchor: Some(crate::transcode::vod_audio_anchor(start_seconds)),
            frame_ticks: grid.denominator,
            video_origin: None,
            policy,
            memory,
            diagnostic_bytes,
            metadata_bytes,
            encoded_process,
        };
        result.minimum_memory_bytes()?;
        Ok(result)
    }

    pub(super) fn matches_clock(
        &self,
        grid: crate::transcode::VodFrameGrid,
        start_seconds: f64,
    ) -> bool {
        self.plan.timescale == grid.numerator
            && self.frame_ticks == grid.denominator
            && self.video_origin.is_none()
            && self.plan.entry(self.start_entry).is_some_and(|entry| {
                entry.start_ticks as f64 / f64::from(self.plan.timescale) == start_seconds
            })
            && self.audio_anchor == Some(crate::transcode::vod_audio_anchor(start_seconds))
    }

    pub(super) fn segment_count(&self) -> usize {
        self.plan.entries.len()
    }

    /// Explicit retained partitions, not the final end-to-end profile. Engine
    /// Parser metadata, native process/launcher and runtime/journal state still
    /// need their own partition before a production demand or receipt relies on it.
    pub(in crate::adaptive_resources) fn minimum_memory_bytes(&self) -> io::Result<u64> {
        let total = size_of::<Self>()
            .checked_add(
                self.plan
                    .entries
                    .capacity()
                    .checked_mul(size_of::<PlanEntry>())
                    .ok_or_else(overflow)?,
            )
            .and_then(|n| n.checked_add(self.encoded_process.capacity()))
            .and_then(|n| n.checked_add(self.memory.reader.buffer_bytes))
            .and_then(|n| n.checked_add(self.memory.read_chunk_bytes))
            .and_then(|n| n.checked_add(self.memory.retained_fragment_memory_bytes))
            // Muxer init, served init and engine replay can coexist. The
            // initial parser itself is dropped before the engine is entered.
            .and_then(|n| n.checked_add(self.memory.init_memory_bytes))
            .and_then(|n| n.checked_add(self.memory.segmenter.merge.workspace_bytes))
            .and_then(|n| n.checked_add(self.memory.segmenter.merge.output_bytes))
            .and_then(|n| n.checked_add(self.memory.segmenter.prefix_memory_bytes))
            .and_then(|n| n.checked_add(self.memory.segmenter.split_memory_bytes))
            .and_then(|n| n.checked_add(self.memory.segmenter.published_batch_bytes))
            .and_then(|n| {
                self.plan
                    .entries
                    .len()
                    .checked_mul(size_of::<u64>())
                    .and_then(|b| n.checked_add(b))
            })
            .and_then(|n| n.checked_add(self.diagnostic_bytes))
            .and_then(|n| n.checked_add(self.metadata_bytes))
            .and_then(|n| n.checked_add(8192))
            .ok_or_else(overflow)?;
        u64::try_from(total).map_err(|_| overflow())
    }
}

/// Bounded-channel metadata; never a terminal receipt or a resource refund.
#[allow(
    clippy::large_enum_variant,
    reason = "Keep the bounded event payload inline; boxing InitIdentity adds an uncharged heap allocation. The separate Tokio block/channel and cloned-payload inventory remains an explicit open implementation item."
)]
pub enum SoftwareVodEvent {
    Initialized(InitIdentity),
    Materialized {
        entry: u32,
        bytes: u64,
        digest: [u8; 32],
    },
}

pub(in crate::adaptive_resources) struct SoftwarePump {
    output: FiniteOutput,
    plan: SoftwareAvcPumpPlan,
    events: mpsc::Sender<SoftwareVodEvent>,
    #[cfg(target_os = "linux")]
    history_context: Option<(telemetry::HistoryBasis, ResourceLedger, FamilyIdentity)>,
}
pub(in crate::adaptive_resources) struct PumpFinished {
    pub outcome: Outcome,
    pub diagnostics: Diagnostics,
    pub completed_output: bool,
    pub kill_error: Option<io::ErrorKind>,
}

/// Copy-only classification retained by the graph after the original writer
/// task is joined. It contains no owned diagnostic text or physical proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FiniteWriterOutcome {
    NotStarted,
    Ran,
    Failed(FiniteWriterFailureKind),
    TaskAbandoned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FiniteWriterFailureKind {
    InitDrift,
    Landing,
    Unplaced,
    Stream,
    Sink,
    EngineChanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FiniteWriterCompletion {
    pub outcome: FiniteWriterOutcome,
    pub produced_through: Option<u32>,
    pub completed_output: bool,
    pub diagnostic_bytes: usize,
    pub diagnostics_truncated: bool,
    pub diagnostic_read_error: Option<io::ErrorKind>,
    pub kill_error: Option<io::ErrorKind>,
}

impl PumpFinished {
    pub(in crate::adaptive_resources) fn summary(&self) -> FiniteWriterCompletion {
        let (outcome, produced_through) = match &self.outcome {
            Outcome::Ran { produced_through } => (FiniteWriterOutcome::Ran, *produced_through),
            Outcome::Failed(failure) => {
                let kind = match failure {
                    Failure::InitDrift(_) => FiniteWriterFailureKind::InitDrift,
                    Failure::Landing(_) => FiniteWriterFailureKind::Landing,
                    Failure::Unplaced { .. } => FiniteWriterFailureKind::Unplaced,
                    Failure::Stream(_) => FiniteWriterFailureKind::Stream,
                    Failure::Sink(_) => FiniteWriterFailureKind::Sink,
                    Failure::EngineChanged(_) => FiniteWriterFailureKind::EngineChanged,
                };
                (FiniteWriterOutcome::Failed(kind), None)
            }
        };
        FiniteWriterCompletion {
            outcome,
            produced_through,
            completed_output: self.completed_output,
            diagnostic_bytes: self.diagnostics.bytes.len(),
            diagnostics_truncated: self.diagnostics.truncated,
            diagnostic_read_error: self.diagnostics.read_error,
            kill_error: self.kill_error,
        }
    }
}
pub(in crate::adaptive_resources) struct Diagnostics {
    pub bytes: Vec<u8>,
    pub truncated: bool,
    pub read_error: Option<io::ErrorKind>,
}

impl SoftwarePump {
    #[cfg(all(test, target_os = "linux"))]
    pub(in crate::adaptive_resources) fn new(
        output: FiniteOutput,
        plan: SoftwareAvcPumpPlan,
    ) -> io::Result<(Self, mpsc::Receiver<SoftwareVodEvent>)> {
        if !output.has_encoded_process_metadata() {
            return Err(invalid("ordinary AVC requires encoded-process metadata"));
        }
        Ok(Self::from_encoded_output(output, plan))
    }

    /// The fixed preparation builder validates the encoded marker before R
    /// acceptance. Binding that same output performs no fallible validation.
    pub(in crate::adaptive_resources::runtime) fn from_encoded_output(
        output: FiniteOutput,
        plan: SoftwareAvcPumpPlan,
    ) -> (Self, mpsc::Receiver<SoftwareVodEvent>) {
        let (events, receiver) = mpsc::channel(1);
        (
            Self {
                output,
                plan,
                events,
                #[cfg(target_os = "linux")]
                history_context: None,
            },
            receiver,
        )
    }
    pub(in crate::adaptive_resources) fn minimum_memory_bytes(&self) -> io::Result<u64> {
        self.plan.minimum_memory_bytes()
    }
    #[cfg(target_os = "linux")]
    pub(in crate::adaptive_resources::runtime) fn bind_history_context(
        &mut self,
        basis: telemetry::HistoryBasis,
        ledger: ResourceLedger,
        selected_family: FamilyIdentity,
    ) {
        self.history_context = Some((basis, ledger, selected_family));
    }
    pub(in crate::adaptive_resources) async fn run(
        self,
        pipes: FiniteRootPipes,
        control: RegisteredChildControl,
    ) -> PumpFinished {
        let diagnostic_bytes = self.plan.diagnostic_bytes;
        let (production, diagnostics) = tokio::join!(
            self.produce(pipes.stdout, control),
            drain(pipes.stderr, diagnostic_bytes, control)
        );
        let (outcome, completed_output, kill_error) = production;
        PumpFinished {
            outcome,
            diagnostics,
            completed_output,
            kill_error,
        }
    }
    async fn produce(
        self,
        mut stdout: super::super::super::physical_graph::finite_pipes::FinitePipeReader,
        control: RegisteredChildControl,
    ) -> (Outcome, bool, Option<io::ErrorKind>) {
        let complete = AtomicBool::new(false);
        let outcome = match self.produce_inner(&mut stdout, &complete, control).await {
            Ok(outcome) => outcome,
            Err(error) => Outcome::Failed(Failure::Sink(error)),
        };
        // Signal the original launcher registration before waiting for stderr.
        // The original ChildJob stays in graph custody; a PID lookup is absent.
        let kill_error =
            if matches!(outcome, Outcome::Failed(_)) || !complete.load(Ordering::Acquire) {
                control.start_kill().err().map(|error| error.kind())
            } else {
                None
            };
        (outcome, complete.load(Ordering::Acquire), kill_error)
    }
    async fn produce_inner(
        self,
        stdout: &mut crate::adaptive_resources::physical_graph::finite_pipes::FinitePipeReader,
        complete: &AtomicBool,
        control: RegisteredChildControl,
    ) -> io::Result<Outcome> {
        #[cfg(target_os = "linux")]
        let telemetry = FiniteTelemetry::new(
            self.history_context,
            control,
            std::sync::Arc::clone(&self.output.runtime.history),
        );
        #[cfg(not(target_os = "linux"))]
        let _ = control;
        let mut muxer = super::initial_unit::read_initial_unit(
            stdout,
            self.plan.memory.reader,
            self.plan.memory.read_chunk_bytes,
        )
        .await?;
        if muxer
            .resident_memory_bytes()
            .ok()
            .and_then(|n| n.checked_mul(4))
            .is_none_or(|n| n > self.plan.memory.init_memory_bytes)
        {
            return Err(invalid("finite AVC init partition exceeded"));
        }
        if muxer
            .video()
            .is_none_or(|video| video.codec != Some(VideoCodec::H264))
        {
            return Err(invalid(
                "software AVC pump received a different video codec",
            ));
        }
        let identity = InitIdentity::establish(&muxer, PromotionInputs::default())
            .map_err(io::Error::other)?;
        let mut served = identity
            .served_init_for(&muxer)
            .map_err(|error| io::Error::other(error.to_string()))?;
        let mut marker = Vec::with_capacity(self.plan.encoded_process.len() + 1);
        marker.extend_from_slice(self.plan.encoded_process.as_bytes());
        marker.push(b'\n');
        publish(
            &self.output,
            FiniteFile::EncodedProcessMetadata,
            &mut marker,
        )
        .await?;
        let mut metadata = BoundedBytes::new(self.plan.metadata_bytes)?;
        #[derive(serde::Serialize)]
        struct Identity<'a> {
            muxer_init: &'a str,
            served_init: &'a str,
            promotion: &'a PromotionInputs,
        }
        serde_json::to_writer(
            &mut metadata,
            &Identity {
                muxer_init: &identity.muxer_init,
                served_init: &identity.served_init,
                promotion: &identity.promotion,
            },
        )
        .map_err(io::Error::other)?;
        publish(
            &self.output,
            FiniteFile::IdentityMetadata,
            &mut metadata.bytes,
        )
        .await?;
        publish(&self.output, FiniteFile::Init, &mut served.bytes).await?;
        drop(served);
        self.events
            .send(SoftwareVodEvent::Initialized(identity.clone()))
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "finite VOD observer dropped")
            })?;
        let generation = Generation {
            plan: self.plan.plan,
            index: None,
            landing_clock: None,
            encoded_audio_anchor: self.plan.audio_anchor,
            encoded_frame_ticks: Some(self.plan.frame_ticks),
            encoded_video_origin: self.plan.video_origin,
            identity,
            convert_dolby_vision: false,
            retain_hevc_parameter_sets: false,
            start_entry: self.plan.start_entry,
            policy: self.plan.policy,
        };
        // Replay precisely the actual init; the bounded initial reader has not
        // consumed the first fragment. No reopened source or reconstructed init.
        let replay = std::io::Cursor::new(std::mem::take(&mut muxer.bytes)).chain(stdout);
        drop(muxer);
        let sink = FiniteSink {
            output: &self.output,
            events: &self.events,
            complete,
            #[cfg(target_os = "linux")]
            telemetry: std::sync::Mutex::new(telemetry),
        };
        let outcome = vod_generation::run_bounded(
            replay,
            generation,
            &sink,
            "finite-software-vod",
            self.plan.memory,
        )
        .await;
        #[cfg(target_os = "linux")]
        let recorded_history = {
            sink.telemetry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recorded_history()
        };
        #[cfg(target_os = "linux")]
        if let Some(history) = recorded_history {
            telemetry::persist_history(history).await;
        }
        Ok(outcome)
    }
}

struct FiniteSink<'a> {
    output: &'a FiniteOutput,
    events: &'a mpsc::Sender<SoftwareVodEvent>,
    complete: &'a AtomicBool,
    #[cfg(target_os = "linux")]
    telemetry: std::sync::Mutex<FiniteTelemetry>,
}
impl Sink for FiniteSink<'_> {
    async fn materialize(&self, entry: u32, mut bytes: Vec<u8>) -> io::Result<()> {
        use sha2::Digest as _;
        let count = u64::try_from(bytes.len()).map_err(|_| overflow())?;
        let digest: [u8; 32] = sha2::Sha256::digest(&bytes).into();
        let entered_publish = Instant::now();
        publish(self.output, FiniteFile::Segment(entry), &mut bytes).await?;
        #[cfg(target_os = "linux")]
        self.telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .durable_unit(entered_publish, Instant::now());
        #[cfg(not(target_os = "linux"))]
        let _ = entered_publish;
        let send_started = Instant::now();
        let result = self
            .events
            .send(SoftwareVodEvent::Materialized {
                entry,
                bytes: count,
                digest,
            })
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "finite VOD observer dropped"));
        #[cfg(target_os = "linux")]
        self.telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .delivered(send_started, Instant::now());
        #[cfg(not(target_os = "linux"))]
        let _ = send_started;
        result
    }
    async fn completed_output(&self) {
        self.complete.store(true, Ordering::Release);
    }
}

async fn publish(output: &FiniteOutput, file: FiniteFile, bytes: &mut Vec<u8>) -> io::Result<()> {
    output.validate_sources().await.map_err(io::Error::other)?;
    let ticket = output
        .queue_file_wait(file, bytes)
        .await
        .map_err(io::Error::other)?;
    output
        .finish(&ticket, deadline()?)
        .await
        .map_err(io::Error::other)?;
    let ticket = output
        .queue_sync_one_wait(file)
        .await
        .map_err(io::Error::other)?;
    output
        .finish(&ticket, deadline()?)
        .await
        .map_err(io::Error::other)?;
    output.validate_sources().await.map_err(io::Error::other)
}
fn deadline() -> io::Result<Instant> {
    Instant::now()
        .checked_add(Duration::from_secs(30))
        .ok_or_else(overflow)
}

async fn drain(
    mut source: crate::adaptive_resources::physical_graph::finite_pipes::FinitePipeReader,
    limit: usize,
    control: RegisteredChildControl,
) -> Diagnostics {
    let mut result = Diagnostics {
        bytes: Vec::new(),
        truncated: false,
        read_error: None,
    };
    if result.bytes.try_reserve_exact(limit).is_err() || result.bytes.capacity() > limit {
        result.read_error = Some(io::ErrorKind::OutOfMemory);
        let _ = control.start_kill();
        return result;
    }
    let mut buffer = [0u8; 8192];
    loop {
        match source.read(&mut buffer).await {
            Ok(0) => break,
            Ok(count) => {
                let keep = count.min(limit - result.bytes.len());
                result.bytes.extend_from_slice(&buffer[..keep]);
                result.truncated |= keep < count;
            }
            Err(error) => {
                result.read_error = Some(error.kind());
                let _ = control.start_kill();
                break;
            }
        }
    }
    result
}
struct BoundedBytes {
    bytes: Vec<u8>,
    limit: usize,
}
impl BoundedBytes {
    fn new(limit: usize) -> io::Result<Self> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(limit).map_err(io::Error::other)?;
        if bytes.capacity() > limit {
            return Err(overflow());
        }
        Ok(Self { bytes, limit })
    }
}
impl io::Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "identity metadata partition exhausted",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn overflow() -> io::Error {
    invalid("finite pump memory accounting overflow")
}
