//! The VOD generation runner: film-addressed segments a plan already promised.
//!
//! Sibling of the copy segmenter, built from the same [`FragmentReader`] and
//! [`Segmenter`] machinery, with the one difference that changes everything
//! downstream: **the plan decides where the cuts fall, not the stream**
//! (ledger D10). A copy session invents its boundaries as it reads and
//! authors a playlist to announce them; a VOD generation is handed a
//! [`SegmentPlan`] whose boundaries are already in a playlist a client holds,
//! cuts exactly there via [`Segmenter::following`], and hands each segment to
//! a [`Sink`] under the plan's own film index. No playlist is written here —
//! the rendition rendered it once at attach — and no init either: the
//! rendition owns `init.mp4`, wrote it at establish time, and this runner only
//! proves that this generation's pipe still produces it
//! ([`InitIdentity::served_init_for`]).
//!
//! The other difference is what failure means. `copyseg` keeps a fallback
//! door open — until its playlist is out, a stream it cannot follow respawns
//! on ffmpeg's own muxer and the viewer never learns anything happened. A VOD
//! generation has no such door, because the client is already holding a plan
//! playlist declared VOD and closed with ENDLIST: bytes that do not follow
//! the plan are not an alternative presentation, they are the wrong film. So
//! a generation that cannot follow its plan **fails typed** — [`Failure`],
//! every variant a `producer_failed` the serving layer reports — and never
//! falls back to legacy.
//!
//! A generation that did not start at entry 0 carries no film time in its
//! own timestamps (plan §12.3, ruling A1), so before anything is cut it is
//! *landed*: fragments are buffered until their video-sample byte sequence
//! places them in the [`FragmentIndex`] ([`match_landing`]) at or before the
//! row that begins the target entry ([`entry_row`]), the discard to that
//! boundary is counted, and only then is
//! the segmenter built and fed. `-noaccurate_seek -ss` lands at the RAP
//! at-or-before the boundary by design, so the discard only ever runs
//! forward. When the byte sequence repeats — every fragment of a static
//! picture weighs the same — the generation asks its [`LandingClock`] where
//! the demuxer landed and places the landing by that time instead
//! ([`match_landing_at`]); the bytes must still agree with it.

use self::identity::{InitIdentity, InitRefused};
use crate::fmp4::{CutPolicy, Fragment, FragmentReader, Init, Published, Segmenter, Unit};
use crate::segplan::{
    entry_row, match_landing, match_landing_at, FragmentIndex, LandingError, PlanEntryKind,
    SegmentPlan, LANDING_WINDOW,
};
use sha2::Digest;
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncReadExt};

mod bounded;
pub mod conversion;
pub use bounded::GenerationMemoryLimits;
pub mod identity;

/// Stable non-secret correlation for bearer session capabilities.
/// Raw session UUIDs authorize playback and never belong in diagnostics.
fn session_log_id(session_id: &str) -> String {
    let digest = Sha256::digest(session_id.as_bytes());
    format!("s-{}", hex::encode(digest))
}

/// Remove a Dolby Vision file-type claim after ffmpeg has removed the Dolby
/// configuration and RPUs from the video sample entry.
///
/// ffmpeg 7.1's `dovi_rpu=strip=1` correctly removes `dvcC`/`dvvC` and the RPU
/// NALs, but movenc has already copied `dby1` into `ftyp` by the time the
/// bitstream filter's output parameters reach it. AVPlayer treats that as a
/// contradictory initialization segment and fails the item immediately. The
/// base layer is ordinary `hvc1` HDR10, so replace only the stale file-type
/// brand with the ISO fragmented-MP4 brand already used by this muxer. A real
/// Dolby Vision sample entry keeps its brand untouched.
pub fn sanitize_stale_dolby_brand(init: &mut Init) -> bool {
    if init.video().is_none_or(|video| video.dolby_vision_config) {
        return false;
    }

    // The rewrite itself lives in `fmp4` because a second caller needs it:
    // `remove_dolby_vision_record` creates this same contradiction when it
    // deletes a record the muxer had written, and it has to fix the brand in
    // the same call rather than depending on this one running again.
    crate::fmp4::replace_dolby_file_type_brand(&mut init.bytes)
}

/// How much pipe is read at a time, following the copy segmenter's `READ_CHUNK`
/// reasoning, unchanged: big enough that a fast copy is not a syscall storm,
/// small enough that a SIGSTOPped producer leaves the reader parked in one
/// `read` rather than holding a large buffer.
const READ_CHUNK: usize = 256 * 1024;

/// Mirror of the copy segmenter’s `MEMORY_WARN_BYTES`, with the landing buffer
/// counted in: before the landing resolves nothing can be published, so the
/// buffered fragments are held memory exactly as pending segments are. The
/// landing itself is bounded — [`LANDING_WINDOW`] fragments decide it, and an
/// ambiguous one reads further only while it holds less than the policy's
/// segment byte ceiling — so a generation that trips this is one whose plan
/// has stopped cutting, not one that is still finding its feet.
const MEMORY_WARN_BYTES: usize = 160 * 1024 * 1024;

/// Why a generation could not serve its plan. Every variant is a typed
/// producer failure the serving layer reports; none of them is a fallback.
#[derive(Debug)]
pub enum Failure {
    /// The muxer init drifted from the rendition's identity, or promotion
    /// stopped being pure — the string says which. Muxer drift is real
    /// pipeline change under a rendition a client already holds a playlist
    /// for; promotion drift should be unreachable and is logged loudly
    /// before this is returned (VOD-M3-HANDOFF §4).
    InitDrift(String),
    /// The stream could not be located against the fragment index.
    Landing(String),
    /// The landing at this generation's start position is undetermined:
    /// its fragments' bytes repeat from more than one row, and the landing
    /// time could not settle it. A statement about this one seek position,
    /// not about the rendition — a generation positioned anywhere else lands
    /// on its own evidence — so the serving layer scopes it to the start
    /// entry rather than failing the rendition.
    ///
    /// `transient` when the landing time was never measured (the probe failed
    /// or timed out): the bytes alone did not settle it, but a later attempt
    /// at the same entry may get its time, so the refusal is not kept.
    Unplaced { why: String, transient: bool },
    /// The pipe broke or was unparseable mid-generation.
    Stream(String),
    /// The sink refused a write.
    Sink(std::io::Error),
    /// A publication fence refused because the fragment-index engine moved
    /// under a rendition planned against it. Distinct from `Stream`, which is
    /// the producer's output genuinely failing: filing this as a read fault
    /// would name the wrong subsystem in the class a client acts on.
    ///
    /// The sibling source-moved fence has no variant here because it is
    /// reached from the sink, which can only answer an `io::Error`; it records
    /// its own class directly and `record_failure` is first-wins, so the
    /// consequence vodgen reports afterwards cannot overwrite it.
    EngineChanged(String),
}

/// How a generation ended.
#[derive(Debug)]
pub enum Outcome {
    /// The generation ran (to EOF or a killed pipe). `produced_through` is
    /// the highest film entry index the sink accepted, `None` if none were.
    Ran {
        produced_through: Option<u32>,
    },
    Failed(Failure),
}

/// Where a repositioned copy generation's demuxer landed, in index ticks.
///
/// Measured by the caller with the generation's own seek: the producer's
/// output cannot say, because `-avoid_negative_ts make_zero` rebases every
/// generation to zero. Asked at most once, and only when the fragments' byte
/// sequence leaves more than one landing possible.
pub type LandingClock = Box<
    dyn FnOnce() -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<LandingTime, String>> + Send>,
        > + Send,
>;

/// A [`LandingClock`]'s answer: the landing keyframe's time counted from the
/// film's first video keyframe — the index's own zero — and one frame's
/// duration, the most the two clocks may disagree by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LandingTime {
    pub ticks: u64,
    pub frame_ticks: u64,
}

/// Why the landing time did not place an ambiguous landing.
#[derive(Debug, Clone)]
enum TimeRefusal {
    /// The time was measured and named no row the bytes agree with.
    Disagrees(String),
    /// The time could not be measured.
    Unmeasured(String),
}

impl std::fmt::Display for TimeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TimeRefusal::Disagrees(why) => write!(f, "the landing time disagrees: {why}"),
            TimeRefusal::Unmeasured(why) => {
                write!(f, "the landing time could not be measured: {why}")
            }
        }
    }
}

/// An undetermined landing, scoped to its start entry. Transient only when
/// the time that could have settled it was never measured.
fn unplaced(ambiguous: &LandingError, refusal: Option<&TimeRefusal>) -> Outcome {
    Outcome::Failed(Failure::Unplaced {
        why: match refusal {
            Some(refusal) => format!("{ambiguous}; {refusal}"),
            None => ambiguous.to_string(),
        },
        transient: matches!(refusal, Some(TimeRefusal::Unmeasured(_))),
    })
}

/// Everything a generation needs to know, owned so the task is `'static`.
pub struct Generation {
    pub plan: SegmentPlan,
    /// Copy pipes locate their demuxer landing against the indexed source.
    /// Encoded pipes start on the exact frame grid and have no copy index.
    pub index: Option<FragmentIndex>,
    /// The demuxer's landing time, for a byte sequence that repeats. `None`
    /// leaves the byte sequence as the only evidence.
    pub landing_clock: Option<LandingClock>,
    /// Encoded AAC begins before video so its transform/overlap state is
    /// warm at the cut. Its origin is a film-global 1024-sample boundary.
    pub encoded_audio_anchor: Option<u64>,
    /// Frozen output cadence; copy generations have no encoded grid.
    pub encoded_frame_ticks: Option<u32>,
    /// Explicit reconstructed video phase on the encoder's AAC-preroll
    /// clock, validated before this publisher sees the private output.
    pub encoded_video_origin: Option<u64>,
    pub identity: InitIdentity,
    /// Rewrite this generation's Dolby Vision RPUs to Profile 8.1 as its
    /// fragments arrive.
    ///
    /// The conversion lives here, after the muxer, because the alternative —
    /// converting a raw elementary stream between two ffmpegs — destroyed the
    /// video timeline: ffmpeg's raw HEVC demuxer emits packets with no
    /// timestamps and the muxer fabricates a decode-order grid, so
    /// presentation order is erased. See the conversion module.
    ///
    /// It is a property of the generation rather than of the run because the
    /// index this generation is matched against was built with the same
    /// answer: a converted stream's fragments are a different size, so an
    /// index built the other way describes media this generation never
    /// produces.
    pub convert_dolby_vision: bool,
    /// Keep the video samples' in-band HEVC parameter sets when merging.
    ///
    /// The recipe's own answer, for the same reason as
    /// `convert_dolby_vision`: the index this generation is matched against
    /// was built by a pipe that kept them, so a segmenter that deleted them
    /// would serve bytes the index never described — and pictures decoded
    /// against the stale definitions the source redefined.
    pub retain_hevc_parameter_sets: bool,
    /// Plan entry the spawner positioned this generation at (its `-ss` landed
    /// at or before this entry's start).
    pub start_entry: u32,
    pub policy: CutPolicy,
}

/// Where film-indexed segments go. Implemented by the rendition layer
/// (`RenditionDir::materialize` under its manifest lock); kept as a trait so
/// this module tests against memory.
pub trait Sink: Send + Sync {
    /// Metadata-only notification after a verified trailer and every final
    /// planned/tail materialization succeeded. A killed pipe never calls it.
    fn completed_output(&self) -> impl std::future::Future<Output = ()> + Send {
        std::future::ready(())
    }
    fn materialize(
        &self,
        entry: u32,
        bytes: Vec<u8>,
    ) -> impl std::future::Future<Output = std::io::Result<()>> + Send;
}

/// Did this write fail because the rendition was torn down under us?
///
/// Same rule as `copyseg`'s `session_gone`, for the same scar: teardown
/// removes the directory while a generation can still be materializing, and
/// that is a session ending normally, not a fault to shout about.
fn session_gone(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
}

/// Read one generation's pipe to exhaustion, materializing plan entries into
/// `sink`.
///
/// Generic over the source so the tests can drive a whole generation from a
/// byte slice, exactly as the copy segmenter is.
pub async fn run<R, S>(src: R, generation: Generation, sink: &S, session_log: &str) -> Outcome
where
    R: AsyncRead + Unpin,
    S: Sink,
{
    run_inner(src, generation, sink, session_log, None).await
}

/// Finite parser/retention path; the caller reserves this partition separately
/// from native output payloads, plans, task state and process memory.
pub async fn run_bounded<R, S>(
    src: R,
    generation: Generation,
    sink: &S,
    session_log: &str,
    limits: GenerationMemoryLimits,
) -> Outcome
where
    R: AsyncRead + Unpin,
    S: Sink,
{
    run_inner(src, generation, sink, session_log, Some(limits)).await
}

async fn run_inner<R, S>(
    mut src: R,
    mut generation: Generation,
    sink: &S,
    session_log: &str,
    memory_limits: Option<GenerationMemoryLimits>,
) -> Outcome
where
    R: AsyncRead + Unpin,
    S: Sink,
{
    if let Some(limits) = memory_limits {
        if let Err(error) = limits.validate() {
            return Outcome::Failed(Failure::Stream(error.to_string()));
        }
    }
    if memory_limits.is_some()
        && (generation.index.is_some()
            || generation.landing_clock.is_some()
            || generation.convert_dolby_vision
            || generation.retain_hevc_parameter_sets
            || generation.encoded_audio_anchor.is_none()
            || generation.encoded_frame_ticks.is_none()
            || generation.identity.promotion != crate::fmp4::PromotionInputs::default())
    {
        return Outcome::Failed(Failure::Stream(
            "finite generation requires ordinary encoded AVC".into(),
        ));
    }
    let (landing, observed, mut buf) = if let Some(limits) = memory_limits {
        let allocated = (|| {
            Ok((
                bounded::fixed_vec::<Fragment>(limits.retained_fragments)?,
                bounded::fixed_vec::<u32>(limits.retained_fragments)?,
                bounded::fixed_vec::<u8>(limits.read_chunk_bytes)?,
            ))
        })();
        match allocated {
            Ok(mut vectors) => {
                vectors.2.resize(limits.read_chunk_bytes, 0);
                vectors
            }
            Err(outcome) => return outcome,
        }
    } else {
        (Vec::new(), Vec::new(), vec![0u8; READ_CHUNK])
    };
    let mut reader = match bounded::Reader::new(memory_limits) {
        Ok(reader) => reader,
        Err(error) => return Outcome::Failed(Failure::Stream(error.to_string())),
    };
    let encoded_entry = generation.start_entry;
    let landing_clock = generation.landing_clock.take();
    let mut state = GenerationRun {
        generation,
        landing_clock,
        time_refusal: None,
        sink,
        session_log,
        served: None,
        video_id: None,
        converter: None,
        landing,
        landing_bytes: 0,
        observed,
        memory_limits,
        transferring_fragment_memory_bytes: 0,
        transferring_fragments: 0,
        discard_remaining: 0,
        verify_row: None,
        segmenter: None,
        produced_through: None,
        warned_memory: false,
        encoded_entry,
        encoded_init: None,
        audio_segmenter: None,
    };

    loop {
        let capacity = reader.read_capacity(buf.len());
        if capacity == 0 {
            return Outcome::Failed(Failure::Stream(
                "finite parser input partition exhausted".into(),
            ));
        }
        let n = match src.read(&mut buf[..capacity]).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) => {
                // A killed producer closes the pipe under us. That is a
                // generation ending; finish with what was produced.
                tracing::debug!(
                    session = %session_log_id(session_log),
                    "vod generation pipe read: {error}"
                );
                break;
            }
        };
        if let Err(error) = reader.push(&buf[..n]) {
            return Outcome::Failed(Failure::Stream(error.to_string()));
        }

        loop {
            let unit = match reader.next_unit() {
                Ok(Some(unit)) => unit,
                Ok(None) => break,
                Err(error) => {
                    // No fallback door here — see the module header. A stream
                    // this reader cannot follow is a typed producer failure.
                    return Outcome::Failed(Failure::Stream(format!(
                        "lost the fragment stream: {error}"
                    )));
                }
            };
            let step = match unit {
                Unit::Init(muxer) => state.on_init(muxer),
                Unit::Fragment(fragment) => state.on_fragment(fragment, reader.buffered()).await,
                // ffmpeg's random-access index, written at EOF. Never
                // published; a plan entry's bytes are already complete.
                Unit::Trailer => Ok(()),
            };
            if let Err(outcome) = step {
                return outcome;
            }
        }
    }

    // A generation's end is trustworthy only when ffmpeg said so: the `mfra`
    // trailer. `copyseg` also accepts "buffer drained on a fragment
    // boundary", and for a live session that is harmless — its playlist only
    // ever names what it actually published. Here the plan already named
    // every entry, and ffmpeg flushes the pipe per fragment, so a SIGKILL
    // routinely leaves the pipe drained exactly on a boundary in the middle
    // of an entry: publishing the pending rump would materialize a fraction
    // of an entry's media under its real index — a permanent cache hit,
    // which admission can then make durable — while dropping it merely
    // leaves a segment unmaterialized for a later generation to redo. So:
    // trailer or nothing.
    let complete = reader.saw_trailer();
    state.finish(complete).await
}

/// One generation's mutable state, so the phases — identity, landing,
/// producing — read as methods rather than as one loop with six locals.
struct GenerationRun<'a, S> {
    generation: Generation,
    memory_limits: Option<GenerationMemoryLimits>,
    transferring_fragment_memory_bytes: usize,
    transferring_fragments: usize,
    sink: &'a S,
    session_log: &'a str,
    /// The served init — muxer init with the stored promotion applied — held
    /// until the landing resolves and the segmenter is built from it.
    served: Option<Init>,
    video_id: Option<u32>,
    /// Built from the served init the first time one arrives, and only when
    /// this generation converts. Held so every fragment is rewritten with the
    /// same NAL framing the init declared.
    converter: Option<conversion::Converter>,
    /// Taken from the generation; consumed by the first ambiguous landing.
    landing_clock: Option<LandingClock>,
    /// What the landing time said when it could not place the landing, kept
    /// for the refusal should the bytes not settle it either.
    time_refusal: Option<TimeRefusal>,
    /// Fragments buffered while the landing is undecided.
    landing: Vec<Fragment>,
    landing_bytes: usize,
    /// Video-sample byte counts of the buffered fragments, in order — the
    /// quantity [`match_landing`] compares (its tests say so: index
    /// `video_bytes`, never the wire length).
    observed: Vec<u32>,
    /// Video-carrying fragments still to drop before the target entry
    /// begins. The landing can sit further before the target than the
    /// buffer that decided it reaches, so the discard outlives the buffer.
    discard_remaining: usize,
    /// The index row the next video-carrying fragment must weigh, from the
    /// landing until the first segment is cut. A landing is a claim about
    /// where the stream is; every fragment read before the first publication
    /// is evidence for or against it, and the first that disagrees is the
    /// typed landing failure instead of a segment of the wrong film.
    verify_row: Option<usize>,
    segmenter: Option<Segmenter>,
    produced_through: Option<u32>,
    warned_memory: bool,
    encoded_entry: u32,
    encoded_init: Option<Init>,
    audio_segmenter: Option<crate::fmp4::PlannedAudioSegmenter>,
}

impl<S: Sink> GenerationRun<'_, S> {
    /// Prove this generation's pipe still produces the rendition's init, and
    /// keep the *served* form for the segmenter.
    ///
    /// This runs before a single segment is written, and the init is never
    /// written anywhere — the rendition owns `init.mp4` and wrote it at
    /// establish time. Rejection is immediate: a generation whose muxer init
    /// drifted would materialize bytes the stored init does not describe, and
    /// an evicted-and-regenerated URI must fail loudly rather than serve
    /// quietly incompatible media (plan §2.2).
    fn on_init(&mut self, mut muxer: Init) -> Result<(), Outcome> {
        if let Some(limits) = self.memory_limits {
            if muxer
                .video()
                .is_none_or(|video| video.codec != Some(crate::fmp4::VideoCodec::H264))
                || muxer
                    .resident_memory_bytes()
                    .ok()
                    .and_then(|n| n.checked_mul(4))
                    .is_none_or(|n| n > limits.init_memory_bytes)
            {
                return Err(Outcome::Failed(Failure::Stream(
                    "finite AVC init partition exceeded".into(),
                )));
            }
        }
        sanitize_stale_dolby_brand(&mut muxer);
        match self.generation.identity.served_init_for(&muxer) {
            Ok(served) => {
                self.video_id = served.video().map(|video| video.id);
                if self.generation.index.is_none() {
                    self.encoded_init = Some(served.clone());
                }
                if self.generation.index.is_none()
                    && served.video().is_none()
                    && !self.generation.plan.entries.is_empty()
                    && self
                        .generation
                        .plan
                        .entries
                        .iter()
                        .all(|entry| entry.kind == PlanEntryKind::AudioTail)
                {
                    if self.generation.encoded_audio_anchor.is_none() {
                        return Err(landing_failed(
                            "shared AAC generation has no film-clock anchor".into(),
                        ));
                    }
                    self.audio_segmenter = Some(
                        crate::fmp4::PlannedAudioSegmenter::new(
                            served.clone(),
                            self.generation.plan.clone(),
                            self.generation.start_entry,
                        )
                        .map_err(|error| landing_failed(error.to_string()))?,
                    );
                }
                if self.generation.convert_dolby_vision {
                    match conversion::Converter::for_init(&served) {
                        Ok(converter) => self.converter = Some(converter),
                        Err(refused) => {
                            // Refuse before a single fragment is published.
                            // Serving unrewritten Profile 7 RPUs under a
                            // Profile 8.1 label is the one outcome worse than
                            // failing this generation.
                            return Err(Outcome::Failed(Failure::Stream(format!(
                                "this stream cannot be converted: {refused}"
                            ))));
                        }
                    }
                }
                self.served = Some(served);
                Ok(())
            }
            Err(refused) => {
                if matches!(refused, InitRefused::PromotionDrift { .. }) {
                    // The impossible one: promotion stopped being a pure
                    // function of stored facts. Loud, per the handoff — a
                    // quiet refusal here would look like ordinary drift.
                    tracing::error!(
                        session = %session_log_id(self.session_log),
                        "promotion is no longer a pure function of stored \
                         inputs: {refused}"
                    );
                }
                Err(Outcome::Failed(Failure::InitDrift(refused.to_string())))
            }
        }
    }

    async fn on_fragment(&mut self, fragment: Fragment, reader_held: usize) -> Result<(), Outcome> {
        self.check_fragment_retention(&fragment)?;
        // Before anything measures it. The landing matcher compares video
        // byte counts against the index, and the index for a converting
        // identity was built from converted fragments — so the count taken
        // here has to be the converted one or the match is against the wrong
        // stream.
        let mut fragment = fragment;
        self.place_reconstructed_video(&mut fragment)?;
        self.validate_encoded_fragment(&fragment)?;
        self.place_encoded_audio(&mut fragment)?;
        if let Some(segmenter) = self.audio_segmenter.as_mut() {
            if fragment.tracks.is_empty() {
                return Ok(());
            }
            // place_encoded_audio uses the same relative origin as muxed VOD;
            // the independent soundtrack cutter requires absolute film ticks.
            let origin = self
                .generation
                .plan
                .entry(self.generation.start_entry)
                .expect("audio plan validated at init")
                .start_ticks;
            for track in &mut fragment.tracks {
                track.base_decode_time = track
                    .base_decode_time
                    .checked_add(origin)
                    .ok_or_else(|| landing_failed("shared AAC film clock overflow".into()))?;
            }
            let published = segmenter
                .push(fragment)
                .map_err(|error| landing_failed(error.to_string()))?;
            for segment in published {
                self.deliver(segment).await?;
            }
            return Ok(());
        }
        if let Some(converter) = self.converter.as_mut() {
            let before = converter.report().source_profile;
            if let Err(refused) = converter.convert(&mut fragment) {
                return Err(Outcome::Failed(Failure::Stream(format!(
                    "this stream cannot be converted: {refused}"
                ))));
            }
            // Said once, on the fragment that first carried an RPU, because
            // this is the only place the answer exists. What the conversion
            // costs a viewer is not in the file's columns and not in the
            // decision: MEL carries no picture detail of its own so dropping
            // it is lossless, while FEL carries real residual detail and
            // dropping it is not, and only the RPU says which. Until the badge
            // carries it (M5a's client half), this line is what an operator
            // reading a "why does this look softer" report has to go on.
            let report = converter.report();
            if before.is_none() && report.source_profile.is_some() {
                tracing::info!(
                    session = %session_log_id(self.session_log),
                    source_profile = report.source_profile,
                    enhancement_layer = ?report.enhancement_layer,
                    "converting Dolby Vision to Profile 8.1: {}",
                    report.enhancement_layer.reason()
                );
            }
        }
        let fragment = fragment;
        if self.segmenter.is_some() {
            self.feed_landed(fragment).await?;
        } else {
            if self.served.is_none() {
                return Err(Outcome::Failed(Failure::Stream(
                    "a fragment arrived before the moov".into(),
                )));
            }
            if let Some(bytes) = self.video_bytes_of(&fragment) {
                self.observed.push(bytes);
            }
            self.landing_bytes += fragment.len();
            self.landing.push(fragment);
            // A full window is answered at once unless it is ambiguous, in
            // which case `engage` keeps the buffer and every further fragment
            // asks again with a longer probe. A shorter window is only
            // consulted at end of stream, where "the pipe ran out" is itself
            // evidence.
            if self.observed.len() >= LANDING_WINDOW
                || (self.generation.index.is_none() && !self.observed.is_empty())
            {
                self.engage(false).await?;
            }
        }
        self.warn_memory(reader_held);
        Ok(())
    }

    /// Encoder flags are an instruction, not proof. Refuse off-grid or
    /// non-random-access output before publishing even the first URI.
    fn validate_encoded_fragment(&mut self, fragment: &Fragment) -> Result<(), Outcome> {
        let Some(init) = &self.encoded_init else {
            return Ok(());
        };
        let Some(_) = init.video().and_then(|video| fragment.track(video.id)) else {
            return Ok(());
        };
        let Some(entry) = self.generation.plan.entry(self.encoded_entry) else {
            return Err(landing_failed(
                "encoded video exceeded its immutable plan".into(),
            ));
        };
        let origin = self
            .generation
            .plan
            .entry(self.generation.start_entry)
            .expect("start entry")
            .start_ticks;
        let frame_ticks = self
            .generation
            .encoded_frame_ticks
            .ok_or_else(|| landing_failed("encoded recipe has no frozen frame cadence".into()))?;
        let start = entry
            .start_ticks
            .checked_sub(origin)
            .ok_or_else(|| landing_failed("encoded entry precedes generation origin".into()))?;
        crate::fmp4::validate_encoded_grid(
            fragment,
            init,
            self.generation.plan.timescale,
            start,
            entry.duration_ticks,
            frame_ticks,
        )
        .map_err(|reason| landing_failed(format!("encoded entry {}: {reason}", entry.index)))?;
        self.encoded_entry += 1;
        Ok(())
    }

    fn place_reconstructed_video(&self, fragment: &mut Fragment) -> Result<(), Outcome> {
        let Some(origin) = self.generation.encoded_video_origin else {
            return Ok(());
        };
        let init = self
            .encoded_init
            .as_ref()
            .ok_or_else(|| landing_failed("reconstructed video has no encoded init".into()))?;
        let video = init
            .video()
            .ok_or_else(|| landing_failed("reconstructed output has no video track".into()))?;
        if video.timescale != self.generation.plan.timescale {
            return Err(landing_failed(
                "reconstructed video clock differs from the plan".into(),
            ));
        }
        for track in &mut fragment.tracks {
            if track.track_id == video.id {
                track.base_decode_time =
                    track.base_decode_time.checked_sub(origin).ok_or_else(|| {
                        landing_failed("reconstructed video precedes its validated origin".into())
                    })?;
            }
        }
        Ok(())
    }

    fn place_encoded_audio(&self, fragment: &mut Fragment) -> Result<(), Outcome> {
        use crate::transcode::{VOD_AAC_FRAME_SAMPLES, VOD_AUDIO_RATE};
        let Some(anchor) = self.generation.encoded_audio_anchor else {
            return Ok(());
        };
        let Some(init) = &self.encoded_init else {
            return Ok(());
        };
        let start = self
            .generation
            .plan
            .entry(self.generation.start_entry)
            .expect("start entry")
            .start_ticks;
        // This is also the common shift Segmenter::following applies from
        // video's zero tfdt to the film clock. Match its outward rounding.
        let shift = start
            .saturating_mul(u64::from(VOD_AUDIO_RATE))
            .div_ceil(u64::from(self.generation.plan.timescale));
        let first = shift.div_ceil(VOD_AAC_FRAME_SAMPLES) * VOD_AAC_FRAME_SAMPLES;
        for track in &mut fragment.tracks {
            if !init
                .tracks
                .iter()
                .any(|item| item.id == track.track_id && item.kind == crate::fmp4::TrackKind::Audio)
            {
                continue;
            }
            if init
                .tracks
                .iter()
                .find(|item| item.id == track.track_id)
                .is_none_or(|item| item.timescale != VOD_AUDIO_RATE)
            {
                return Err(landing_failed(
                    "encoded AAC did not retain its declared 48kHz clock".into(),
                ));
            }
            let mut absolute = i128::from(anchor) + i128::from(track.base_decode_time)
                - i128::from(VOD_AAC_FRAME_SAMPLES);
            for run in &mut track.runs {
                let mut discard = 0;
                for sample in &run.samples {
                    if absolute >= i128::from(first) {
                        break;
                    }
                    absolute += i128::from(sample.duration);
                    run.data_offset += sample.size as usize;
                    discard += 1;
                }
                run.samples.drain(..discard);
                if !run.samples.is_empty() {
                    break;
                }
            }
            if track.sample_count() > 0 {
                if absolute < i128::from(shift) || absolute % i128::from(VOD_AAC_FRAME_SAMPLES) != 0
                {
                    return Err(landing_failed(
                        "encoded AAC lost its global sample phase".into(),
                    ));
                }
                track.base_decode_time = (absolute - i128::from(shift)) as u64;
            }
        }
        fragment.tracks.retain(|track| track.sample_count() > 0);
        Ok(())
    }

    /// The landing: place the buffered fragments in the index, discard
    /// forward to the target entry's boundary, and start cutting on the
    /// plan's boundaries from there.
    ///
    /// An ambiguous landing is first put to the [`LandingClock`]: the time
    /// the demuxer landed at names the row, and the bytes must agree with it.
    /// Without a clock, or when it cannot answer, an ambiguous landing before
    /// the end of the pipe is not an answer yet: the buffer is kept and the
    /// next fragment asks again with one more fragment of evidence, while the
    /// buffer holds less than one segment's byte ceiling — the memory the
    /// segmenter is entitled to hold pending anyway. Past that, or at the end
    /// of the pipe, it is the typed failure.
    async fn engage(&mut self, at_end: bool) -> Result<(), Outcome> {
        let start_entry = self.generation.start_entry;
        let Some(entry) = self.generation.plan.entry(start_entry) else {
            return Err(landing_failed(format!(
                "start entry {start_entry} is not in the plan, which has {} \
                 entries",
                self.generation.plan.len()
            )));
        };
        let entry_start_ticks = entry.start_ticks;
        let mut landed = None;
        let discards = if let Some(index) = &self.generation.index {
            let Some(target) = entry_row(index, entry) else {
                return Err(landing_failed(format!(
                    "entry {start_entry} at {entry_start_ticks} ticks does not \
                     begin at an index row"
                )));
            };
            // Copy's demuxer may land before, but never after, the plan RAP,
            // so the target row bounds every possible landing.
            let readable = !at_end && self.landing_bytes < self.generation.policy.max_bytes;
            let row = match match_landing(index, &self.observed, target) {
                Ok(row) => row,
                Err(ambiguous @ LandingError::Ambiguous { .. }) if self.landing_clock.is_some() => {
                    let clock = self.landing_clock.take().expect("checked by the guard");
                    let placed = match clock().await {
                        Ok(time) => match_landing_at(
                            index,
                            &self.observed,
                            target,
                            time.ticks,
                            time.frame_ticks,
                        )
                        .map_err(|error| TimeRefusal::Disagrees(error.to_string())),
                        Err(why) => Err(TimeRefusal::Unmeasured(why)),
                    };
                    match placed {
                        Ok(row) => row,
                        // The time is one more piece of evidence, not a
                        // verdict on the rendition: a repeat the next
                        // fragments settle still lands by its bytes.
                        Err(refusal) if readable => {
                            tracing::warn!(
                                session = %session_log_id(self.session_log),
                                "{refusal}; reading further"
                            );
                            self.time_refusal = Some(refusal);
                            return Ok(());
                        }
                        Err(refusal) => return Err(unplaced(&ambiguous, Some(&refusal))),
                    }
                }
                Err(LandingError::Ambiguous { .. }) if readable => return Ok(()),
                Err(ambiguous @ LandingError::Ambiguous { .. }) => {
                    return Err(unplaced(&ambiguous, self.time_refusal.as_ref()))
                }
                Err(error) => return Err(landing_failed(error.to_string())),
            };
            landed = Some(row);
            target - row
        } else {
            if self
                .served
                .as_ref()
                .and_then(Init::video)
                .is_none_or(|video| video.timescale != self.generation.plan.timescale)
            {
                return Err(landing_failed(
                    "encoded video timescale differs from the plan".into(),
                ));
            }
            0
        };
        // The boundaries this generation will cut on: the plan's VIDEO
        // entries from the start entry onward. Audio-tail entries have no
        // video boundary — `Segmenter::finish` splits the tail at the
        // policy's ceiling exactly as the plan did, so the tail entries come
        // out of `finish` under the right indexes.
        let selected = || {
            self.generation
                .plan
                .entries
                .iter()
                .filter(|entry| entry.index >= start_entry && entry.kind == PlanEntryKind::Video)
                .map(|entry| entry.start_ticks)
        };
        let starts: Vec<u64> = if self.memory_limits.is_some() {
            let mut starts = bounded::fixed_vec(selected().count())?;
            starts.extend(selected());
            starts
        } else {
            selected().collect()
        };
        if starts.is_empty() {
            return Err(landing_failed(format!(
                "no video boundary at or after entry {start_entry}; a \
                 generation cannot start inside the audio tail"
            )));
        }
        let init = self
            .served
            .take()
            .expect("the landing buffer fills only after the init");
        let placed = if let Some(limits) = self.memory_limits {
            Segmenter::following_encoded_bounded(
                init,
                self.generation.policy,
                u64::from(start_entry),
                starts,
                limits.segmenter,
            )
        } else if self.generation.encoded_audio_anchor.is_some() {
            Segmenter::following_encoded(
                init,
                self.generation.policy,
                u64::from(start_entry),
                starts,
            )
        } else {
            Segmenter::following(init, self.generation.policy, u64::from(start_entry), starts)
        };
        let segmenter = placed
            .map_err(|error| {
                Outcome::Failed(Failure::Stream(format!(
                    "placing the generation against its plan: {error}"
                )))
            })?
            .retaining_hevc_parameter_sets(self.generation.retain_hevc_parameter_sets);
        self.segmenter = Some(segmenter);

        // Drop the first `discards` video-carrying fragments — the index
        // counts fragments of the video-only pipe, so a fragment without
        // video (a leading audio-only flush) does not count against the
        // discard — and feed everything after them onward. The count runs on
        // past the buffer into the fragments still to come, and every one of
        // them is checked against the index until the first cut.
        self.discard_remaining = discards;
        self.verify_row = landed;
        let buffered = std::mem::take(&mut self.landing);
        self.landing_bytes = 0;
        if self.memory_limits.is_some() {
            self.transferring_fragment_memory_bytes = buffered
                .capacity()
                .checked_mul(std::mem::size_of::<Fragment>())
                .ok_or_else(|| landing_failed("landing transfer capacity overflow".into()))?;
            self.transferring_fragments = buffered.len();
            for fragment in &buffered {
                let heap = fragment
                    .resident_memory_bytes()
                    .map_err(|e| landing_failed(e.to_string()))?
                    .checked_sub(std::mem::size_of::<Fragment>())
                    .ok_or_else(|| landing_failed("landing transfer size mismatch".into()))?;
                self.transferring_fragment_memory_bytes = self
                    .transferring_fragment_memory_bytes
                    .checked_add(heap)
                    .ok_or_else(|| landing_failed("landing transfer memory overflow".into()))?;
            }
        }
        for fragment in buffered {
            if self.memory_limits.is_some() {
                let heap = fragment
                    .resident_memory_bytes()
                    .map_err(|e| landing_failed(e.to_string()))?
                    .checked_sub(std::mem::size_of::<Fragment>())
                    .ok_or_else(|| landing_failed("landing transfer size mismatch".into()))?;
                self.transferring_fragment_memory_bytes = self
                    .transferring_fragment_memory_bytes
                    .checked_sub(heap)
                    .ok_or_else(|| landing_failed("landing transfer memory underflow".into()))?;
                self.transferring_fragments = self
                    .transferring_fragments
                    .checked_sub(1)
                    .ok_or_else(|| landing_failed("landing transfer count underflow".into()))?;
            }
            self.feed_landed(fragment).await?;
        }
        self.transferring_fragment_memory_bytes = 0;
        Ok(())
    }

    /// One fragment after the landing: verify it against the index while the
    /// landing is still unproven by a publication, drop it while the discard
    /// to the target entry is unfinished, and otherwise cut it.
    async fn feed_landed(&mut self, fragment: Fragment) -> Result<(), Outcome> {
        let video_bytes = self.video_bytes_of(&fragment);
        if let (Some(bytes), Some(row)) = (video_bytes, self.verify_row) {
            let expected = self
                .generation
                .index
                .as_ref()
                .and_then(|index| index.rows.get(row))
                .map(|row| row.video_bytes);
            if expected != Some(bytes) {
                return Err(landing_failed(format!(
                    "the landing did not hold: the fragment placed at index \
                     row {row} weighs {bytes} video bytes and the index says \
                     {expected:?}"
                )));
            }
            self.verify_row = Some(row + 1);
        }
        if self.discard_remaining > 0 {
            if video_bytes.is_some() {
                self.discard_remaining -= 1;
            }
            return Ok(());
        }
        self.push_to_segmenter(fragment).await?;
        if self.produced_through.is_some() {
            self.verify_row = None;
        }
        Ok(())
    }

    async fn push_to_segmenter(&mut self, fragment: Fragment) -> Result<(), Outcome> {
        self.check_fragment_retention(&fragment)?;
        let segmenter = self
            .segmenter
            .as_mut()
            .expect("only called once the landing built the segmenter");
        let published = match segmenter.push(fragment) {
            Ok(published) => published,
            Err(error) => {
                return Err(Outcome::Failed(Failure::Stream(format!(
                    "merging a segment: {error}"
                ))))
            }
        };
        if let Some(published) = published {
            self.deliver(published).await?;
        }
        Ok(())
    }

    /// Hand one plan entry's bytes to the sink.
    ///
    /// `NotFound` is the rendition being torn down under a running
    /// generation — a session ending, not a fault (`copyseg` learned this
    /// within an hour of its first deploy); anything else is a typed sink
    /// failure. `produced_through` counts only what the sink accepted, so
    /// the caller can resume from the entry after it.
    async fn deliver(&mut self, published: Published) -> Result<(), Outcome> {
        let entry = u32::try_from(published.index).unwrap_or(u32::MAX);
        // One generation's segments leave the segmenter in plan order; the
        // sink is entitled to rely on that.
        debug_assert!(
            self.produced_through.is_none_or(|through| entry > through),
            "entry {entry} would arrive out of order after {:?}",
            self.produced_through
        );
        match self.sink.materialize(entry, published.segment.bytes).await {
            Ok(()) => {
                self.produced_through = Some(entry);
                Ok(())
            }
            Err(error) if session_gone(&error) => {
                tracing::debug!(
                    session = %session_log_id(self.session_log),
                    "rendition went away mid-materialize; stopping"
                );
                Err(Outcome::Ran {
                    produced_through: self.produced_through,
                })
            }
            Err(error) => Err(Outcome::Failed(Failure::Sink(error))),
        }
    }

    /// End of pipe: land whatever is still undecided, then publish the tail
    /// if — and only if — ffmpeg's trailer says the film really ended.
    async fn finish(&mut self, complete: bool) -> Outcome {
        if let Some(segmenter) = self.audio_segmenter.take() {
            if complete && !segmenter.complete() {
                return landing_failed(
                    "shared AAC ended before its promised sample interval".into(),
                );
            }
            return Outcome::Ran {
                produced_through: self.produced_through,
            };
        }
        if self.served.is_none() && self.segmenter.is_none() {
            return Outcome::Failed(Failure::Stream(
                "the pipe ended before its moov arrived".into(),
            ));
        }
        if self.segmenter.is_none() {
            // Fewer fragments than a full landing window: `match_landing`
            // accepts a short probe only at the index's own tail, where the
            // stream genuinely ran out. Anywhere else this is a typed
            // landing failure, not a guess.
            if let Err(outcome) = self.engage(true).await {
                return outcome;
            }
        }
        let Some(mut segmenter) = self.segmenter.take() else {
            unreachable!("engage constructs the segmenter or fails typed");
        };
        if !complete {
            // Killed without a trailer — whether mid-fragment or exactly on
            // a fragment boundary. The pending media is real but its entry
            // is not whole, and a plan entry is served complete or not at
            // all — the wait pool blocks on it and a later generation
            // produces it.
            tracing::debug!(
                session = %session_log_id(self.session_log),
                "pipe ended without its trailer; the tail entries stay \
                 unmaterialized"
            );
            return Outcome::Ran {
                produced_through: self.produced_through,
            };
        }
        // One index past the plan is a shape `finish` can legitimately emit:
        // the source's audio outruns the last planned entry by a tail the
        // plan skipped (plan_copy plans nothing under its 50 ms threshold,
        // and its probe can call the tracks equal), while the segmenter
        // splits at the exact video boundary and hands the trailing audio
        // back as its own chunk. The playlist never names that index, so
        // nothing can ever fetch it — dropping it with a warning is the
        // honest end of a complete film, where handing it to the sink would
        // be refused as out-of-plan and poison the whole rendition at the
        // end of every complete watch. Any other out-of-plan index stays the
        // hard sink failure it is.
        let planned = self.generation.plan.len() as u64;
        match segmenter.finish() {
            Ok(published) => {
                for segment in published {
                    if segment.index == planned {
                        tracing::warn!(
                            session = %session_log_id(self.session_log),
                            bytes = segment.segment.bytes.len(),
                            "dropping an unplanned sub-threshold audio rump \
                             one past the plan's last entry"
                        );
                        continue;
                    }
                    if let Err(outcome) = self.deliver(segment).await {
                        return outcome;
                    }
                }
            }
            Err(error) => {
                return Outcome::Failed(Failure::Stream(format!(
                    "merging the final segment: {error}"
                )))
            }
        }
        self.sink.completed_output().await;
        Outcome::Ran {
            produced_through: self.produced_through,
        }
    }

    /// The landing matcher's quantity for one fragment: summed video sample
    /// bytes, no container overhead — `None` for a fragment carrying no
    /// video, which says nothing about where in the film we are.
    fn video_bytes_of(&self, fragment: &Fragment) -> Option<u32> {
        let track = fragment.track(self.video_id?)?;
        Some(u32::try_from(track.byte_len()).unwrap_or(u32::MAX))
    }

    fn warn_memory(&mut self, reader_held: usize) {
        if self.warned_memory {
            return;
        }
        let pending = self
            .segmenter
            .as_ref()
            .map(Segmenter::pending_bytes)
            .unwrap_or(0);
        let held = pending + self.landing_bytes + reader_held;
        if held > MEMORY_WARN_BYTES {
            self.warned_memory = true;
            tracing::warn!(
                session = %session_log_id(self.session_log),
                held_bytes = held,
                "vod generation is holding more than a segment's worth of \
                 bytes; the plan's ceiling should have cut before here"
            );
        }
    }
}

fn landing_failed(why: String) -> Outcome {
    Outcome::Failed(Failure::Landing(why))
}
