//! The VOD generation runner: film-addressed segments a plan already promised.
//!
//! Sibling of [`crate::copyseg`], built from the same [`FragmentReader`] and
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
//! places them in the [`FragmentIndex`] ([`match_landing`]), the discard to
//! the target entry's boundary is counted ([`discards_to`]), and only then is
//! the segmenter built and fed. `-noaccurate_seek -ss` lands at the RAP
//! at-or-before the boundary by design, so the discard only ever runs
//! forward.

use plurx_core::fmp4::{CutPolicy, Fragment, FragmentReader, Init, Published, Segmenter, Unit};
use plurx_core::segplan::{
    discards_to, match_landing, FragmentIndex, PlanEntryKind, SegmentPlan, LANDING_WINDOW,
};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::copyseg::sanitize_stale_dolby_brand;
use crate::renditiondir::{InitIdentity, InitRefused};

/// How much pipe is read at a time — [`crate::copyseg::READ_CHUNK`]'s
/// reasoning, unchanged: big enough that a fast copy is not a syscall storm,
/// small enough that a SIGSTOPped producer leaves the reader parked in one
/// `read` rather than holding a large buffer.
const READ_CHUNK: usize = 256 * 1024;

/// Mirror of [`crate::copyseg::MEMORY_WARN_BYTES`], with the landing buffer
/// counted in: before the landing resolves nothing can be published, so the
/// buffered fragments are held memory exactly as pending segments are. The
/// landing itself is bounded — [`LANDING_WINDOW`] fragments decide it — so a
/// generation that trips this is one whose plan has stopped cutting, not one
/// that is still finding its feet.
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
    /// The pipe broke or was unparseable mid-generation.
    Stream(String),
    /// The sink refused a write.
    Sink(std::io::Error),
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

/// Everything a generation needs to know, owned so the task is `'static`.
pub struct Generation {
    pub plan: SegmentPlan,
    pub index: FragmentIndex,
    pub identity: InitIdentity,
    /// Plan entry the spawner positioned this generation at (its `-ss` landed
    /// at or before this entry's start).
    pub start_entry: u32,
    pub policy: CutPolicy,
}

/// Where film-indexed segments go. Implemented by the rendition layer
/// (`RenditionDir::materialize` under its manifest lock); kept as a trait so
/// this module tests against memory.
pub trait Sink: Send + Sync {
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
/// byte slice, exactly as [`crate::copyseg::run`] is.
pub async fn run<R, S>(mut src: R, generation: Generation, sink: &S, session_log: &str) -> Outcome
where
    R: AsyncRead + Unpin,
    S: Sink,
{
    let mut reader = FragmentReader::new();
    let mut state = GenerationRun {
        generation,
        sink,
        session_log,
        served: None,
        video_id: None,
        landing: Vec::new(),
        landing_bytes: 0,
        observed: Vec::new(),
        segmenter: None,
        produced_through: None,
        warned_memory: false,
    };
    let mut buf = vec![0u8; READ_CHUNK];

    loop {
        let n = match src.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) => {
                // A killed producer closes the pipe under us. That is a
                // generation ending; finish with what was produced.
                tracing::debug!(
                    session = %crate::transcode::session_log_id(session_log),
                    "vod generation pipe read: {error}"
                );
                break;
            }
        };
        reader.push(&buf[..n]);

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
    sink: &'a S,
    session_log: &'a str,
    /// The served init — muxer init with the stored promotion applied — held
    /// until the landing resolves and the segmenter is built from it.
    served: Option<Init>,
    video_id: Option<u32>,
    /// Fragments buffered while the landing is undecided.
    landing: Vec<Fragment>,
    landing_bytes: usize,
    /// Video-sample byte counts of the buffered fragments, in order — the
    /// quantity [`match_landing`] compares (its tests say so: index
    /// `video_bytes`, never the wire length).
    observed: Vec<u32>,
    segmenter: Option<Segmenter>,
    produced_through: Option<u32>,
    warned_memory: bool,
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
        sanitize_stale_dolby_brand(&mut muxer);
        match self.generation.identity.served_init_for(&muxer) {
            Ok(served) => {
                self.video_id = served.video().map(|video| video.id);
                self.served = Some(served);
                Ok(())
            }
            Err(refused) => {
                if matches!(refused, InitRefused::PromotionDrift { .. }) {
                    // The impossible one: promotion stopped being a pure
                    // function of stored facts. Loud, per the handoff — a
                    // quiet refusal here would look like ordinary drift.
                    tracing::error!(
                        session = %crate::transcode::session_log_id(self.session_log),
                        "promotion is no longer a pure function of stored \
                         inputs: {refused}"
                    );
                }
                Err(Outcome::Failed(Failure::InitDrift(refused.to_string())))
            }
        }
    }

    async fn on_fragment(&mut self, fragment: Fragment, reader_held: usize) -> Result<(), Outcome> {
        if self.segmenter.is_some() {
            self.push_to_segmenter(fragment).await?;
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
            // A full window is a final answer either way: `match_landing`
            // reads only the first `LANDING_WINDOW` values, so more fragments
            // cannot change what it says. A shorter window is only consulted
            // at end of stream, where "the pipe ran out" is itself evidence.
            if self.observed.len() >= LANDING_WINDOW {
                self.engage().await?;
            }
        }
        self.warn_memory(reader_held);
        Ok(())
    }

    /// The landing: place the buffered fragments in the index, discard
    /// forward to the target entry's boundary, and start cutting on the
    /// plan's boundaries from there.
    async fn engage(&mut self) -> Result<(), Outcome> {
        let start_entry = self.generation.start_entry;
        let Some(entry) = self.generation.plan.entry(start_entry) else {
            return Err(landing_failed(format!(
                "start entry {start_entry} is not in the plan, which has {} \
                 entries",
                self.generation.plan.len()
            )));
        };
        let entry_start_ticks = entry.start_ticks;
        let row = match match_landing(&self.generation.index, &self.observed) {
            Ok(row) => row,
            Err(error) => return Err(landing_failed(error.to_string())),
        };
        // Always forward: `-noaccurate_seek -ss` lands at the RAP at-or-before
        // the boundary by design. A landing past the entry means the spawner
        // and the plan disagree about where this generation is.
        let Some(discards) = discards_to(&self.generation.index, row, entry) else {
            return Err(landing_failed(format!(
                "landed at index row {row}, past entry {start_entry} at \
                 {entry_start_ticks} ticks"
            )));
        };
        // The boundaries this generation will cut on: the plan's VIDEO
        // entries from the start entry onward. Audio-tail entries have no
        // video boundary — `Segmenter::finish` splits the tail at the
        // policy's ceiling exactly as the plan did, so the tail entries come
        // out of `finish` under the right indexes.
        let starts: Vec<u64> = self
            .generation
            .plan
            .entries
            .iter()
            .filter(|entry| entry.index >= start_entry && entry.kind == PlanEntryKind::Video)
            .map(|entry| entry.start_ticks)
            .collect();
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
        let segmenter = match Segmenter::following(
            init,
            self.generation.policy,
            u64::from(start_entry),
            starts,
        ) {
            Ok(segmenter) => segmenter,
            Err(error) => {
                return Err(Outcome::Failed(Failure::Stream(format!(
                    "placing the generation against its plan: {error}"
                ))))
            }
        };
        self.segmenter = Some(segmenter);

        // Drop the first `discards` video-carrying fragments — the index
        // counts fragments of the video-only pipe, so a fragment without
        // video (a leading audio-only flush) does not count against the
        // discard — and feed everything after them onward.
        let mut to_drop = discards;
        let buffered = std::mem::take(&mut self.landing);
        self.landing_bytes = 0;
        for fragment in buffered {
            if to_drop > 0 {
                if self.video_bytes_of(&fragment).is_some() {
                    to_drop -= 1;
                }
                continue;
            }
            self.push_to_segmenter(fragment).await?;
        }
        Ok(())
    }

    async fn push_to_segmenter(&mut self, fragment: Fragment) -> Result<(), Outcome> {
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
                    session = %crate::transcode::session_log_id(self.session_log),
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
            if let Err(outcome) = self.engage().await {
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
                session = %crate::transcode::session_log_id(self.session_log),
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
                            session = %crate::transcode::session_log_id(self.session_log),
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
                session = %crate::transcode::session_log_id(self.session_log),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use plurx_core::domain::MediaFile;
    use plurx_core::segplan::{SourceIdentity, TrackDurations};
    use plurx_core::testfixtures::{self, pipe};
    use plurx_core::transcode::{copy_pipe_args, Pacing};

    use crate::fragindex::{index_stream, IndexOutcome};

    /// A sink that remembers what it was handed and can start refusing.
    #[derive(Default)]
    struct MemSink {
        writes: Mutex<Vec<(u32, Vec<u8>)>>,
        /// Refuse with this error kind once this many writes have landed.
        refuse_after: Option<(usize, std::io::ErrorKind)>,
    }

    impl MemSink {
        fn refusing(after: usize, kind: std::io::ErrorKind) -> MemSink {
            MemSink {
                writes: Mutex::new(Vec::new()),
                refuse_after: Some((after, kind)),
            }
        }

        fn taken(&self) -> Vec<(u32, Vec<u8>)> {
            self.writes.lock().expect("sink lock").clone()
        }
    }

    impl Sink for MemSink {
        async fn materialize(&self, entry: u32, bytes: Vec<u8>) -> std::io::Result<()> {
            let mut writes = self.writes.lock().expect("sink lock");
            if let Some((after, kind)) = self.refuse_after {
                if writes.len() >= after {
                    return Err(std::io::Error::from(kind));
                }
            }
            writes.push((entry, bytes));
            Ok(())
        }
    }

    /// Everything one fixture film's generations share.
    struct Film {
        index: FragmentIndex,
        plan: SegmentPlan,
        identity: InitIdentity,
        policy: CutPolicy,
        video_id: u32,
        /// The production pipe, cached — what a full-film generation reads.
        feed: Vec<u8>,
    }

    /// The video-only index pipe over a fixture — the exact command
    /// `fragindex`'s own tests run (`copy_index_pipe_args`' shape).
    fn index_pipe_bytes(kind: &str) -> Vec<u8> {
        let source = testfixtures::source(kind);
        let mut command = std::process::Command::new(testfixtures::ffmpeg());
        command
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&source)
            .args(["-map_chapters", "-1", "-map", "0:v:0?", "-an", "-sn"])
            .args(["-c:v", "copy", "-tag:v", "hvc1"])
            .args(["-bsf:v", "filter_units=remove_types=32-34"])
            .args(["-avoid_negative_ts", "make_zero"])
            .args([
                "-movflags",
                "frag_keyframe+empty_moov+default_base_moof+delay_moov",
            ])
            .args(["-use_editlist", "0", "-f", "mp4", "pipe:1"]);
        testfixtures::run(&mut command)
    }

    /// The production pipe's muxer init — `Unit::Init`, the byte string the
    /// identity check compares.
    fn muxer_init(feed: &[u8]) -> Init {
        let mut reader = FragmentReader::new();
        reader.push(feed);
        loop {
            match reader.next_unit().expect("parsing the pipe") {
                Some(Unit::Init(init)) => return init,
                Some(_) => continue,
                None => panic!("the pipe carried no init"),
            }
        }
    }

    /// A real film: index from the video-only pipe, plan from the index,
    /// identity established from the production pipe's own first generation —
    /// the whole M3 attach sequence, in miniature.
    ///
    /// Brisk policy for `copyseg`'s reason: the 12 s fixture cannot reach a
    /// mid-film entry under the shipped 6 s floor, and the property under
    /// test is the plan being followed, not the constant.
    async fn film() -> Film {
        let feed = pipe("clean-cra");
        let index = match index_stream(
            std::io::Cursor::new(index_pipe_bytes("clean-cra")),
            SourceIdentity::new(1, 1, "fingerprint"),
            None,
            false,
        )
        .await
        {
            IndexOutcome::Built(index) => *index,
            other => panic!("the fixture must index: {other:?}"),
        };
        let policy = CutPolicy::new(3, 1, 48_000_000, 15, index.timescale);
        let plan = plurx_core::segplan::plan_copy(
            &index,
            &policy,
            &TrackDurations {
                video_ms: 12_000,
                audio_ms: 12_000,
                audio_bits_per_second: 256_000,
            },
        );
        let mut init = muxer_init(&feed);
        sanitize_stale_dolby_brand(&mut init);
        let video_id = init.video().expect("a video track").id;
        let identity =
            InitIdentity::establish(&init, index.promotion.clone()).expect("establishing identity");
        Film {
            index,
            plan,
            identity,
            policy,
            video_id,
            feed,
        }
    }

    fn generation(film: &Film, start_entry: u32) -> Generation {
        Generation {
            plan: film.plan.clone(),
            index: film.index.clone(),
            identity: film.identity.clone(),
            start_entry,
            policy: film.policy,
        }
    }

    /// Walk one level of MP4 boxes.
    fn box_walk<'a>(region: &'a [u8], mut visit: impl FnMut(&[u8; 4], &'a [u8])) {
        let mut at = 0usize;
        while at + 8 <= region.len() {
            let size =
                u32::from_be_bytes(region[at..at + 4].try_into().expect("a box size")) as usize;
            if size < 8 || at + size > region.len() {
                break;
            }
            let name: [u8; 4] = region[at + 4..at + 8].try_into().expect("a box name");
            visit(&name, &region[at + 8..at + size]);
            at += size;
        }
    }

    /// A materialized segment's `mdat` payload. The merger copies each
    /// track's samples contiguously, video first, so the film's video bytes
    /// are the payload's prefix.
    fn mdat_payload(segment: &[u8]) -> &[u8] {
        let mut found = None;
        box_walk(segment, |name, body| {
            if name == b"mdat" && found.is_none() {
                found = Some(body);
            }
        });
        found.expect("a materialized segment carries an mdat")
    }

    /// The video `tfdt` a materialized segment actually carries, read off the
    /// bytes rather than off what the segmenter believed.
    fn video_tfdt(segment: &[u8], video_id: u32) -> u64 {
        let mut moofs = Vec::new();
        box_walk(segment, |name, body| {
            if name == b"moof" {
                moofs.push(body);
            }
        });
        let moof = moofs.first().expect("a segment carries a moof");
        let mut trafs = Vec::new();
        box_walk(moof, |name, body| {
            if name == b"traf" {
                trafs.push(body);
            }
        });
        for traf in trafs {
            let mut id = None;
            let mut tfdt = None;
            box_walk(traf, |name, body| match name {
                b"tfhd" if body.len() >= 8 => {
                    id = Some(u32::from_be_bytes(
                        body[4..8].try_into().expect("a track id"),
                    ));
                }
                b"tfdt" if body.len() >= 12 => {
                    tfdt = Some(if body[0] == 1 {
                        u64::from_be_bytes(body[4..12].try_into().expect("a v1 tfdt"))
                    } else {
                        u64::from(u32::from_be_bytes(
                            body[4..8].try_into().expect("a v0 tfdt"),
                        ))
                    });
                }
                _ => {}
            });
            if id == Some(video_id) {
                return tfdt.expect("the video traf carries a tfdt");
            }
        }
        panic!("no video traf in the segment");
    }

    /// The byte offset of the end of the pipe's `fragments`-th fragment —
    /// a cut there is exactly what a SIGKILL between two flushes leaves:
    /// a drained pipe, on a boundary, with no trailer. Verbatim contiguity
    /// (init, then moof+mdat pairs, nothing between) is asserted as we walk,
    /// so the cut cannot silently land inside some box this walk skipped.
    fn fragment_boundary_cut(feed: &[u8], fragments: usize) -> usize {
        let mut reader = FragmentReader::new();
        reader.push(feed);
        let mut cut = 0usize;
        let mut seen = 0usize;
        while let Some(unit) = reader.next_unit().expect("parsing the pipe") {
            match unit {
                Unit::Init(init) => {
                    assert_eq!(&feed[..init.bytes.len()], &init.bytes[..]);
                    cut += init.bytes.len();
                }
                Unit::Fragment(fragment) => {
                    assert_eq!(&feed[cut..cut + fragment.len()], &fragment.bytes[..]);
                    cut += fragment.len();
                    seen += 1;
                    if seen == fragments {
                        return cut;
                    }
                }
                Unit::Trailer => {}
            }
        }
        panic!("the pipe carried only {seen} fragments");
    }

    /// The film's video payload of one plan entry, in bytes — summed
    /// `video_bytes` of the index rows the entry covers. This is the quantity
    /// that must come out identical from every generation, because video
    /// samples are copied.
    fn video_payload_len(film: &Film, entry: u32) -> usize {
        let mut row = 0usize;
        for earlier in &film.plan.entries[..entry as usize] {
            row += earlier.fragments as usize;
        }
        let count = film.plan.entries[entry as usize].fragments as usize;
        film.index.rows[row..row + count]
            .iter()
            .map(|row| row.video_bytes as usize)
            .sum()
    }

    /// The whole point, end to end: a generation started at entry 0
    /// materializes every entry the plan promised, in order, each exactly
    /// once, and says how far it got.
    #[tokio::test]
    async fn a_full_film_generation_materializes_every_plan_entry_in_order() {
        let film = film().await;
        assert!(
            film.plan.len() >= 4,
            "the fixture must plan several entries under the brisk policy, \
             got {}",
            film.plan.len()
        );
        let sink = MemSink::default();
        let outcome = run(&film.feed[..], generation(&film, 0), &sink, "test").await;
        let Outcome::Ran { produced_through } = outcome else {
            panic!("the generation did not run: {outcome:?}");
        };
        let writes = sink.taken();
        assert_eq!(writes.len(), film.plan.len(), "an entry per plan entry");
        for (offset, (entry, bytes)) in writes.iter().enumerate() {
            assert_eq!(*entry as usize, offset, "entries must arrive in order");
            assert!(!bytes.is_empty());
        }
        assert_eq!(produced_through, Some(film.plan.len() as u32 - 1));
    }

    /// The rendition serves whichever generation's bytes happen to be on
    /// disk, so a re-run of the same generation must produce the same bytes.
    #[tokio::test]
    async fn rerunning_a_generation_materializes_identical_bytes() {
        let film = film().await;
        let first = MemSink::default();
        let second = MemSink::default();
        let one = run(&film.feed[..], generation(&film, 0), &first, "test").await;
        let two = run(&film.feed[..], generation(&film, 0), &second, "test").await;
        assert!(matches!(one, Outcome::Ran { .. }), "{one:?}");
        assert!(matches!(two, Outcome::Ran { .. }), "{two:?}");
        let first = first.taken();
        let second = second.taken();
        assert_eq!(first.len(), second.len());
        assert_eq!(
            first[0], second[0],
            "entry 0's bytes must not depend on which run produced them"
        );
        assert_eq!(first, second, "every entry, not only the first");
    }

    /// Plan §2.2's identity check, at the runner: a pipe whose muxer init is
    /// not the rendition's is refused before a single sink write.
    #[tokio::test]
    async fn a_drifted_muxer_init_is_refused_before_a_single_sink_write() {
        let film = film().await;
        // A different fixture's pipe: a stand-in for a changed video
        // pipeline under a rendition whose playlist a client already holds.
        let drifted = pipe("closed-gop");
        let sink = MemSink::default();
        let outcome = run(&drifted[..], generation(&film, 0), &sink, "test").await;
        assert!(
            matches!(outcome, Outcome::Failed(Failure::InitDrift(_))),
            "a drifted init must fail typed, got {outcome:?}"
        );
        assert!(
            sink.taken().is_empty(),
            "a refused generation must not have written anything"
        );
    }

    /// The core noncontiguous-correctness assertion: a real repositioned
    /// producer — `copy_pipe_args` with a start offset, a live ffmpeg —
    /// lands, discards forward to its entry's boundary, and its first
    /// materialized index is exactly `start_entry`, carrying the same film
    /// video bytes the full-film run put under that index and starting at
    /// the film time the plan named.
    ///
    /// Whole-segment byte identity across generations is not a property this
    /// path has: the audio branch is re-encoded from the seek point, so its
    /// bytes differ by construction, and each generation's timeline is
    /// anchored at its own first boundary. What is invariant — and what a
    /// player needs — is the entry's video payload and its planned index and
    /// start.
    #[tokio::test]
    async fn a_mid_film_generation_lands_on_its_entry_and_matches_the_full_run() {
        let film = film().await;
        // The reference: the same entries from a full-film generation.
        let full = MemSink::default();
        let outcome = run(&film.feed[..], generation(&film, 0), &full, "full").await;
        assert!(matches!(outcome, Outcome::Ran { .. }), "{outcome:?}");
        let full = full.taken();

        let start_entry = 2u32;
        let entry = film.plan.entry(start_entry).expect("a mid-film entry");
        assert_eq!(entry.kind, PlanEntryKind::Video);
        let target_row = film
            .index
            .rows
            .iter()
            .position(|row| row.dts == entry.start_ticks)
            .expect("the entry begins at an index row");
        assert!(
            target_row >= 1,
            "the entry must have a fragment in front of it for the seek to \
             land on"
        );
        // Seek strictly between the previous fragment boundary and the
        // entry's: `-noaccurate_seek` then lands on the RAP *before* the
        // entry, so the landing has a real discard to count.
        let mid_ticks = (film.index.rows[target_row - 1].dts + entry.start_ticks) / 2;
        let start_seconds = mid_ticks as f64 / f64::from(film.index.timescale);

        let src = testfixtures::source("clean-cra");
        let file = MediaFile {
            id: 1,
            item_id: 1,
            path: src.clone(),
            size: 1,
            mtime: 1,
            duration_ms: Some(12_000),
            container: Some("mkv".into()),
            video_codec: Some("hevc".into()),
            video_profile: Some("Main".into()),
            width: Some(640),
            height: Some(360),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            bitrate: Some(1_000_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
        };
        let args = copy_pipe_args(&file, start_seconds, None, true, Pacing::unpaced(), false);
        let mut child = tokio::process::Command::new(testfixtures::ffmpeg())
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawning ffmpeg");
        let stdout = child.stdout.take().expect("stdout pipe");

        let sink = MemSink::default();
        let outcome = run(stdout, generation(&film, start_entry), &sink, "mid").await;
        let _ = child.wait().await;

        let Outcome::Ran { produced_through } = outcome else {
            panic!("the repositioned generation did not run: {outcome:?}");
        };
        let writes = sink.taken();
        assert!(!writes.is_empty(), "it must have materialized something");
        assert_eq!(
            writes[0].0, start_entry,
            "the discards must land the generation exactly on its entry"
        );
        for (offset, (entry, _)) in writes.iter().enumerate() {
            assert_eq!(*entry, start_entry + offset as u32);
        }
        assert_eq!(
            produced_through,
            Some(film.plan.len() as u32 - 1),
            "a mid-film generation runs to the end of the plan"
        );

        // The media-time contract: the segment begins at the film time the
        // plan named for it, whatever timestamps ffmpeg handed us.
        assert_eq!(
            video_tfdt(&writes[0].1, film.video_id),
            entry.start_ticks,
            "the entry must begin at the plan's own film time"
        );

        // And it carries the same film: the entry's video payload —
        // copied samples, the invariant quantity — is byte-identical to
        // what the full-film run put under the same index.
        let payload = video_payload_len(&film, start_entry);
        assert!(payload > 0);
        let of_full = mdat_payload(&full[start_entry as usize].1);
        let of_mid = mdat_payload(&writes[0].1);
        assert!(of_full.len() >= payload && of_mid.len() >= payload);
        assert_eq!(
            of_mid[..payload],
            of_full[..payload],
            "entry {start_entry}'s video bytes differ between generations"
        );
    }

    /// A killed producer: the pipe stops mid-fragment. What reached the sink
    /// is exactly the entries that completed — no tail, no claim of the rest
    /// of the film — and the generation still Ran.
    #[tokio::test]
    async fn a_truncated_pipe_materializes_what_completed_and_no_more() {
        let film = film().await;
        let cut = film.feed.len() * 2 / 3;
        let sink = MemSink::default();
        let outcome = run(&film.feed[..cut], generation(&film, 0), &sink, "test").await;
        let Outcome::Ran { produced_through } = outcome else {
            panic!("a killed pipe still Ran: {outcome:?}");
        };
        let writes = sink.taken();
        assert!(!writes.is_empty(), "two thirds of the film cut something");
        assert!(
            writes.len() < film.plan.len(),
            "a killed pipe must not claim the whole plan"
        );
        for (offset, (entry, _)) in writes.iter().enumerate() {
            assert_eq!(*entry as usize, offset);
        }
        assert_eq!(produced_through, Some(writes.len() as u32 - 1));
    }

    /// The kill that copyseg's rule would mistake for a clean end: ffmpeg
    /// flushes the pipe per fragment, so a SIGKILL routinely leaves the pipe
    /// drained exactly on a fragment boundary, mid-entry, with no trailer.
    /// The pending rump of the half-read entry must NOT reach the sink —
    /// published under its real index it would be a permanent cache hit
    /// carrying a fraction of its media — and the generation still Ran,
    /// ending at the last complete entry.
    #[tokio::test]
    async fn a_kill_on_a_fragment_boundary_publishes_no_partial_entry() {
        let film = film().await;
        // Four fragments in: entry 0 (fragment 0) and entry 1 (fragments
        // 1-2) are complete, and fragment 3 — half of entry 2 — is pending.
        let cut = fragment_boundary_cut(&film.feed, 4);
        assert!(cut < film.feed.len());
        let sink = MemSink::default();
        let outcome = run(&film.feed[..cut], generation(&film, 0), &sink, "test").await;
        let Outcome::Ran { produced_through } = outcome else {
            panic!("a killed pipe still Ran: {outcome:?}");
        };
        let indexes: Vec<u32> = sink.taken().iter().map(|(entry, _)| *entry).collect();
        assert_eq!(
            indexes,
            vec![0, 1],
            "only the entries whose media fully arrived may materialize; a \
             drained pipe with no trailer is a killed pipe, not a finished \
             film"
        );
        assert_eq!(produced_through, Some(1));
    }

    /// The production pipe with its audio outrunning the plan: the fixture's
    /// 12 s of film against 12.3 s of tone, under a plan whose probe said the
    /// tracks are equal — the shape of a tail the plan skipped. The plan
    /// names no tail entry, while `Segmenter::finish` splits at the exact
    /// video boundary and hands the trailing audio back as its own chunk —
    /// one index past the plan. (The tone runs 0.3 s over rather than the
    /// threshold's 50 ms because `-avoid_negative_ts make_zero` stretches
    /// the video timeline by the audio priming delay, and a sliver under one
    /// AAC frame past the stretched video end never leaves the last chunk.)
    fn audiotail_pipe() -> Vec<u8> {
        let source = testfixtures::source("clean-cra");
        let mut command = std::process::Command::new(testfixtures::ffmpeg());
        command
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&source)
            .args(["-f", "lavfi", "-t", "12.3"])
            .args(["-i", "sine=frequency=440:sample_rate=48000"])
            .args(["-map", "0:v:0", "-map", "1:a:0", "-sn"])
            .args(["-c:v", "copy", "-tag:v", "hvc1"])
            .args(["-bsf:v", "filter_units=remove_types=32-34"])
            .args(["-c:a", "aac", "-b:a", "256k"])
            .args(["-avoid_negative_ts", "make_zero"])
            .args([
                "-movflags",
                "frag_keyframe+empty_moov+default_base_moof+delay_moov",
            ])
            .args(["-use_editlist", "0", "-f", "mp4", "pipe:1"]);
        testfixtures::run(&mut command)
    }

    /// The end-of-film twin of the kill test: a source whose audio outruns
    /// the plan's last entry makes `finish` emit one chunk at an index the
    /// playlist never names. Nothing can ever fetch it, so it is dropped
    /// with a warning — not handed to a sink that would refuse it as
    /// out-of-plan and poison the rendition at the end of every complete
    /// watch of an affected title.
    #[tokio::test]
    async fn an_unplanned_rump_one_past_the_plan_is_dropped_not_a_failure() {
        let film = film().await;
        let feed = audiotail_pipe();
        // The sliver-bearing pipe is its own first generation: same video
        // branch (so the film's index and plan still describe it), its own
        // muxer init to establish against.
        let mut init = muxer_init(&feed);
        sanitize_stale_dolby_brand(&mut init);
        let identity = InitIdentity::establish(&init, film.index.promotion.clone())
            .expect("establishing identity");
        let generation = Generation {
            plan: film.plan.clone(),
            index: film.index.clone(),
            identity,
            start_entry: 0,
            policy: film.policy,
        };
        let planned = film.plan.len() as u32;
        let sink = MemSink::default();
        let outcome = run(&feed[..], generation, &sink, "test").await;
        let Outcome::Ran { produced_through } = outcome else {
            panic!("an unplanned rump must not fail the generation: {outcome:?}");
        };
        let writes = sink.taken();
        assert_eq!(
            writes.len(),
            planned as usize,
            "every planned entry and nothing else"
        );
        assert!(
            writes.iter().all(|(entry, _)| *entry < planned),
            "the sink must never see an index the plan does not name"
        );
        assert_eq!(produced_through, Some(planned - 1));
    }

    /// A sink whose directory went away is a session ending, not a fault:
    /// stop quietly, report what was produced.
    #[tokio::test]
    async fn a_sink_that_vanished_ends_the_generation_quietly() {
        let film = film().await;
        let sink = MemSink::refusing(1, std::io::ErrorKind::NotFound);
        let outcome = run(&film.feed[..], generation(&film, 0), &sink, "test").await;
        assert!(
            matches!(
                outcome,
                Outcome::Ran {
                    produced_through: Some(0)
                }
            ),
            "a vanished sink ends the session with what landed: {outcome:?}"
        );
        assert_eq!(sink.taken().len(), 1);
    }

    /// Any other sink refusal is a typed failure the serving layer reports.
    #[tokio::test]
    async fn a_sink_refusal_other_than_absence_fails_typed() {
        let film = film().await;
        let sink = MemSink::refusing(1, std::io::ErrorKind::PermissionDenied);
        let outcome = run(&film.feed[..], generation(&film, 0), &sink, "test").await;
        let Outcome::Failed(Failure::Sink(error)) = outcome else {
            panic!("a refused write must fail typed: {outcome:?}");
        };
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    /// The ffmpeg-upgrade case, at the runner: an index that no longer
    /// describes the pipe's fragments misses rather than misaligns, and the
    /// generation fails typed before anything is written.
    #[tokio::test]
    async fn an_index_that_no_longer_describes_the_pipe_fails_the_landing() {
        let mut film = film().await;
        for row in &mut film.index.rows {
            row.video_bytes = row.video_bytes.wrapping_add(1);
        }
        let sink = MemSink::default();
        let outcome = run(&film.feed[..], generation(&film, 0), &sink, "test").await;
        assert!(
            matches!(outcome, Outcome::Failed(Failure::Landing(_))),
            "a stale index must be a typed landing failure, got {outcome:?}"
        );
        assert!(sink.taken().is_empty());
    }
}
