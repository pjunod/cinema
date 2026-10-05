//! The tone-map census: every GPU graph this node could pair with its encoder,
//! measured against the CPU chain on authored PQ signals, with a still of
//! each, so the house operator can be chosen with numbers and pictures in
//! front of the person choosing (STARTUP-AND-TONE-MAP-BUILD-HANDOFF §6.2,
//! ruling R2).
//!
//! It measures; it decides nothing. Selection, the boot gate and every
//! production filter string are untouched — each candidate runs through its
//! production builder ([`Pipeline::filters`]) unchanged, and the baseline is
//! the production CPU chain ([`zscale_tone_map_filter`]) in production order,
//! scale then tone map, at each signal's own peak. What it reports is
//! *distance from the CPU chain*, which is a baseline, not an analytical
//! reference (`VIDEO-QUALITY-PROGRAM.md` H2).
//!
//! It is GPU work on a production node, so it follows the standing rule for
//! real hardware: it runs only when an administrator asks (Settings → System,
//! the "HDR tone-map" row); it takes a **background** permit through
//! [`crate::admission`] and so waits behind, and yields to, any viewer; it
//! polls `background_must_yield` every [`POLL`] while a child runs and kills
//! that child the moment a live start wants the hardware; it can be stopped;
//! its children are attributed on the Activity page as "tone-map census";
//! and the row says which step and which candidate it is on.
//!
//! Every ffmpeg it starts goes through the probe's own spawn seam
//! ([`super::Tools`] / [`super::Spawn`]), so the job ownership, priority class
//! and runtime cache are the boot probe's, and cancelling a step is dropping
//! its future: the seam's child is kill-on-drop and job-owned.
//!
//! Results are node-local, under the data directory, keyed by what makes a
//! measurement comparable — the ffmpeg build, the encoder family and the GPU
//! device — and served inside `GET /api/v1/system` as `tone_map.census`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use plurx_core::transcode::{zscale_tone_map_filter, Encoder, Pipeline, PIPELINE_CANDIDATES};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::signal::{self, Distance, SignalSpec, SIGNALS};
use super::{check_exit, Stdout, Tools};

/// The census frame. 1080p: large enough for 120-pixel grid blocks, small
/// enough that every hardware decoder takes it and a run takes seconds.
pub(crate) const WIDTH: usize = 1920;
pub(crate) const HEIGHT: usize = 1080;
/// Frames encoded into each signal clip, and frames decoded from it. The
/// measured frame is the last decoded one, the same index for every graph.
const FRAMES_ENCODED: usize = 6;
const FRAMES_DECODED: usize = 3;
/// How often a running census asks whether playback wants the hardware, and
/// how often a queued one retries admission. Never more than 500 ms.
pub(crate) const POLL: Duration = Duration::from_millis(500);
/// CPU threads reserved beside the hardware slot: the baseline is the CPU
/// float chain, and the software decodes are real cores too.
const CPU_THREADS: usize = 4;

/// What makes two census results comparable. A result is only ever shown for
/// the key the node has now; a new ffmpeg, a different encoder family or a
/// different GPU starts from no result rather than showing a stale one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CensusKey {
    /// First line of `ffmpeg -version`.
    pub ffmpeg_version: String,
    /// [`Encoder::family_name`] of the encoder the census paired graphs with.
    pub encoder_family: String,
    /// The hardware device the graphs ran on, with its PCI ids when readable.
    pub gpu_device: String,
}

impl CensusKey {
    pub fn for_node(ffmpeg_version: &str, encoder: Encoder) -> CensusKey {
        CensusKey {
            ffmpeg_version: ffmpeg_version.to_owned(),
            encoder_family: encoder.family_name().to_owned(),
            gpu_device: gpu_device(encoder),
        }
    }

    /// The directory name results for this key live in: a digest, because
    /// the version line is free text and must not become a path.
    pub fn slug(&self) -> String {
        use sha2::Digest;
        let canonical = serde_json::to_vec(self).expect("a key serialises");
        hex::encode(&sha2::Sha256::digest(canonical)[..8])
    }
}

/// The device the graphs for `encoder` run on. VA-API and QSV open the DRM
/// render node; its PCI vendor and device ids, when sysfs offers them, are
/// what tells one GPU from another behind the same path.
fn gpu_device(encoder: Encoder) -> String {
    match encoder {
        Encoder::Vaapi | Encoder::Qsv => {
            let node = plurx_core::transcode::vaapi_device();
            let ids = Path::new(&node)
                .file_name()
                .map(|name| Path::new("/sys/class/drm").join(name).join("device"))
                .and_then(|dir| {
                    let read = |f: &str| std::fs::read_to_string(dir.join(f)).ok();
                    Some(format!(
                        "{}:{}",
                        read("vendor")?.trim().trim_start_matches("0x"),
                        read("device")?.trim().trim_start_matches("0x")
                    ))
                });
            match ids {
                Some(ids) => format!("{node} ({ids})"),
                None => node,
            }
        }
        Encoder::Nvenc => "cuda".to_owned(),
        Encoder::VideoToolbox => "videotoolbox".to_owned(),
        Encoder::Software => "none".to_owned(),
    }
}

/// One signal version as encoded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalInfo {
    pub name: String,
    pub max_cll: u32,
    pub max_fall: u32,
}

/// The CPU chain on one signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineResult {
    pub signal: String,
    /// 8-bit luma of the brightest grid block.
    pub brightest_block_luma: f64,
    /// The CPU chain against itself across two runs: the noise floor every
    /// candidate's distance should be read against. Zero means deterministic.
    pub noise_floor: f64,
    pub still: Option<String>,
}

/// One candidate graph on one signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateSignal {
    pub signal: String,
    /// Whether the graph produced a measurable frame at all.
    pub started: bool,
    /// Why not, when it did not (ffmpeg's own diagnostics, bounded).
    pub reason: Option<String>,
    /// Distance from the CPU chain on the same signal, per channel (Y, U, V).
    pub distance: Option<Distance>,
    pub brightest_block_luma: Option<f64>,
    pub still: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateResult {
    pub pipeline: String,
    pub label: String,
    /// Whether the boot probe selected this graph on this node.
    pub selected: bool,
    pub signals: Vec<CandidateSignal>,
}

/// A finished census, as stored and as served.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CensusResult {
    pub key: CensusKey,
    /// Unix milliseconds.
    pub finished_at: i64,
    /// The graph the boot probe selected, for context.
    pub selected: String,
    pub width: usize,
    pub height: usize,
    /// `[columns, rows]` of the measurement grid.
    pub grid: [usize; 2],
    pub signals: Vec<SignalInfo>,
    pub baseline: Vec<BaselineResult>,
    /// True when the CPU chain reproduced itself exactly on every signal. The
    /// bar for "visibly different" is then a margin chosen from the stills,
    /// not a derived number.
    pub noise_floor_zero: bool,
    pub candidates: Vec<CandidateResult>,
}

impl CensusResult {
    /// Every still this result names, so the still route can refuse any other
    /// file name without touching the filesystem.
    pub fn stills(&self) -> impl Iterator<Item = &str> {
        self.baseline
            .iter()
            .filter_map(|b| b.still.as_deref())
            .chain(
                self.candidates
                    .iter()
                    .flat_map(|c| c.signals.iter().filter_map(|s| s.still.as_deref())),
            )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CensusState {
    #[default]
    Idle,
    /// Asked for, waiting for a background permit: a viewer holds or wants
    /// the hardware, or the slots are full.
    Queued,
    Running,
    Finished,
    /// A live start wanted the hardware; the census killed its child and
    /// gave the slot back. Not a failure.
    Yielded,
    Stopped,
    Failed,
}

impl CensusState {
    fn active(self) -> bool {
        matches!(self, CensusState::Queued | CensusState::Running)
    }
}

/// What the census is doing now.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct CensusStatus {
    pub state: CensusState,
    /// The step in progress, in words ("vpp_qsv · maxcll-4000").
    pub step: Option<String>,
    /// The candidate graph in progress, when the step is one.
    pub candidate: Option<String>,
    /// Why it failed or stopped.
    pub reason: Option<String>,
    /// Unix milliseconds of the last state change.
    pub since: i64,
}

/// `tone_map.census` in `GET /api/v1/system`.
#[derive(Debug, Clone, Serialize)]
pub struct CensusView {
    #[serde(flatten)]
    pub status: CensusStatus,
    /// The key a result would be stored under on this node now.
    pub key: CensusKey,
    /// The stored result for that key, if a census has finished for it.
    pub result: Option<CensusResult>,
    /// The node that answered. The System row shows one node; behind a
    /// balanced ingress a poll or a Stop can reach another, and the row must
    /// not paint that node's census as this one's.
    pub node_id: String,
}

#[derive(Default)]
struct Inner {
    status: CensusStatus,
    generation: u64,
    stop: Option<CancellationToken>,
    /// The last result read or written, by key slug.
    cached: Option<(String, Option<CensusResult>)>,
}

/// The node's census: at most one run at a time, its live status and its
/// last result. One per process, held on `AppState`.
#[derive(Default)]
pub struct ToneMapCensus {
    inner: Mutex<Inner>,
}

impl ToneMapCensus {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Claim the census for a new run. `None` while one is queued or running.
    pub fn begin(&self) -> Option<(u64, CancellationToken)> {
        let mut inner = self.lock();
        if inner.status.state.active() {
            return None;
        }
        inner.generation += 1;
        let stop = CancellationToken::new();
        inner.stop = Some(stop.clone());
        inner.status = CensusStatus {
            state: CensusState::Queued,
            step: Some("waiting for a free hardware slot".to_owned()),
            candidate: None,
            reason: None,
            since: crate::state::clock_ms(),
        };
        Some((inner.generation, stop))
    }

    /// Ask a queued or running census to stop. False when none is active.
    pub fn stop(&self) -> bool {
        let inner = self.lock();
        match (&inner.stop, inner.status.state.active()) {
            (Some(stop), true) => {
                stop.cancel();
                true
            }
            _ => false,
        }
    }

    pub fn current(&self) -> CensusStatus {
        self.lock().status.clone()
    }

    /// Change the status of run `generation`, if it is still the current run.
    fn set(
        &self,
        generation: u64,
        state: CensusState,
        step: Option<String>,
        candidate: Option<&str>,
        reason: Option<String>,
    ) {
        let mut inner = self.lock();
        if inner.generation != generation {
            return;
        }
        inner.status = CensusStatus {
            state,
            step,
            candidate: candidate.map(str::to_owned),
            reason,
            since: crate::state::clock_ms(),
        };
        if !state.active() {
            inner.stop = None;
        }
    }

    fn running(&self, generation: u64, step: String, candidate: Option<&str>) {
        self.set(
            generation,
            CensusState::Running,
            Some(step),
            candidate,
            None,
        );
    }

    /// The status, the current key and the stored result for it. Reads the
    /// result file at most once per key.
    pub async fn view(&self, root: &Path, key: CensusKey) -> CensusView {
        let slug = key.slug();
        let cached = {
            let inner = self.lock();
            inner
                .cached
                .as_ref()
                .filter(|(s, _)| *s == slug)
                .map(|(_, r)| r.clone())
        };
        let result = match cached {
            Some(result) => result,
            None => {
                let loaded = load(root, &key).await;
                self.lock().cached = Some((slug, loaded.clone()));
                loaded
            }
        };
        CensusView {
            status: self.current(),
            key,
            result,
            node_id: String::new(),
        }
    }

    /// The stored result for `key`, when the still `name` is one it names.
    pub async fn still_path(&self, root: &Path, key: CensusKey, name: &str) -> Option<PathBuf> {
        let view = self.view(root, key).await;
        let result = view.result?;
        let named = result.stills().any(|still| still == name);
        named.then(|| root.join(result.key.slug()).join(name))
    }

    fn invalidate(&self) {
        self.lock().cached = None;
    }

    fn remember(&self, result: CensusResult) {
        self.lock().cached = Some((result.key.slug(), Some(result)));
    }
}

async fn load(root: &Path, key: &CensusKey) -> Option<CensusResult> {
    let bytes = tokio::fs::read(root.join(key.slug()).join("census.json"))
        .await
        .ok()?;
    serde_json::from_slice::<CensusResult>(&bytes)
        .ok()
        .filter(|result| result.key == *key)
}

/// Replace the stored result for `result.key` with a finished run: retire
/// the old `census.json` first (a reader then sees no result rather than one
/// naming stills that are being replaced), move the staged stills in, then
/// store the new result.
async fn publish(root: &Path, staged: &Path, result: &CensusResult) -> Result<(), String> {
    let dir = root.join(result.key.slug());
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    match tokio::fs::remove_file(dir.join("census.json")).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("could not retire the previous census result: {e}")),
    }
    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.path().extension().is_some_and(|ext| ext == "png") {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }
    for name in result.stills() {
        tokio::fs::rename(staged.join(name), dir.join(name))
            .await
            .map_err(|e| format!("could not store the still {name}: {e}"))?;
    }
    save(root, result).await
}

/// Store `result` under its key, atomically: a reader sees the old file or
/// the new one, never half of one.
async fn save(root: &Path, result: &CensusResult) -> Result<(), String> {
    let dir = root.join(result.key.slug());
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let body = serde_json::to_vec_pretty(result).map_err(|e| e.to_string())?;
    let partial = dir.join("census.json.partial");
    tokio::fs::write(&partial, body)
        .await
        .map_err(|e| format!("could not write the census result: {e}"))?;
    tokio::fs::rename(&partial, dir.join("census.json"))
        .await
        .map_err(|e| format!("could not store the census result: {e}"))
}

/// The admission side of a run: how it gets its permit and when it must give
/// it back. A trait so the queueing and yielding rules are tested against the
/// real [`crate::admission::Admissions`] without a GPU.
trait CensusSlot {
    type Permit;
    fn try_admit(&self) -> Option<Self::Permit>;
    fn must_yield(&self) -> bool;
    /// Why no permit can ever be granted on this node as configured, so a
    /// run fails at once instead of waiting forever. `None` when waiting can
    /// end.
    fn unadmittable(&self) -> Option<String> {
        None
    }
}

/// The production slot: the node's one heavy-background gate (shared with
/// pretranscodes, fragment indexing and the transcoder benchmark, so the
/// census never runs beside any of them), one background hardware slot (for
/// a hardware encoder) and a small CPU share, taken together or not at all.
pub(crate) struct AdmissionSlot {
    pub admissions: crate::admission::Admissions,
    pub heavy: std::sync::Arc<tokio::sync::Semaphore>,
    pub hardware_max: usize,
    pub software_budget: usize,
    pub hardware: bool,
}

impl CensusSlot for AdmissionSlot {
    type Permit = (
        tokio::sync::OwnedSemaphorePermit,
        crate::admission::TranscodePermit,
    );

    fn try_admit(&self) -> Option<Self::Permit> {
        let heavy = std::sync::Arc::clone(&self.heavy)
            .try_acquire_owned()
            .ok()?;
        let estimate = crate::admission::TranscodeResourceEstimate {
            hardware_slot: self.hardware,
            cpu_threads: CPU_THREADS.min(self.software_budget).max(1),
            decoder_threads: None,
        };
        let permit = self.admissions.try_admit_bundle(
            self.hardware_max,
            self.software_budget,
            &estimate,
            crate::admission::Priority::Background,
        )?;
        Some((heavy, permit))
    }

    fn unadmittable(&self) -> Option<String> {
        (self.hardware && self.hardware_max == 0).then(|| {
            "this node allows no hardware transcodes (maximum hardware sessions is 0)".to_owned()
        })
    }

    fn must_yield(&self) -> bool {
        self.admissions.background_must_yield()
    }
}

/// What one run measures and where it writes.
pub(crate) struct CensusPlan {
    pub encoder: Encoder,
    /// The boot probe's selection, recorded beside the numbers.
    pub selected: Pipeline,
    pub key: CensusKey,
    /// The results root (`<data dir>/tone-map-census`).
    pub root: PathBuf,
}

/// Why a run ended without a result.
#[derive(Debug, PartialEq)]
enum Halt {
    Stopped,
    Yielded,
    Failed(String),
}

/// Purpose shown for the census's children on the Activity page.
const PURPOSE: &str = "tone-map census";

/// Run one census through the production spawn seam: the boot probe's own
/// [`super::Spawn`], with the playback runtime cache, attributed as
/// "tone-map census".
pub(crate) async fn run_on_node(
    census: &ToneMapCensus,
    generation: u64,
    stop: CancellationToken,
    slot: AdmissionSlot,
    runtime_cache: &Path,
    plan: CensusPlan,
) {
    let tools = super::Spawn {
        runtime_cache: Some(runtime_cache),
        purpose: Some(PURPOSE),
    };
    drive(census, generation, stop, &slot, &tools, plan).await;
}

/// Run one census from the queue to a stored result, reporting every step on
/// `census`. Returns when it has finished, failed, stopped or yielded; in each
/// case no child it started is still running and its permit is released.
async fn drive<T: Tools, S: CensusSlot>(
    census: &ToneMapCensus,
    generation: u64,
    stop: CancellationToken,
    slot: &S,
    tools: &T,
    plan: CensusPlan,
) {
    if let Some(reason) = slot.unadmittable() {
        census.set(generation, CensusState::Failed, None, None, Some(reason));
        return;
    }
    let permit = loop {
        if stop.is_cancelled() {
            census.set(
                generation,
                CensusState::Stopped,
                None,
                None,
                Some("stopped while queued".to_owned()),
            );
            return;
        }
        if let Some(permit) = slot.try_admit() {
            break permit;
        }
        tokio::select! {
            _ = stop.cancelled() => {}
            _ = tokio::time::sleep(POLL) => {}
        }
    };
    census.running(generation, "preparing the signals".to_owned(), None);
    let work = plan.root.join("work");
    let _ = tokio::fs::remove_dir_all(&work).await;
    let outcome = run(census, generation, &stop, slot, tools, &plan, &work).await;
    drop(permit);
    let outcome = match outcome {
        Ok(result) => match publish(&plan.root, &work.join("stills"), &result).await {
            Ok(()) => Ok(result),
            Err(reason) => {
                // The previous result may already be retired: read it again
                // from disk rather than serve a cached copy of it.
                census.invalidate();
                Err(Halt::Failed(reason))
            }
        },
        Err(halt) => Err(halt),
    };
    let _ = tokio::fs::remove_dir_all(&work).await;
    match outcome {
        Ok(result) => {
            census.remember(result);
            census.set(generation, CensusState::Finished, None, None, None);
            tracing::info!(key = %plan.key.slug(), "tone-map census finished");
        }
        Err(Halt::Stopped) => {
            census.set(
                generation,
                CensusState::Stopped,
                None,
                None,
                Some("stopped by an administrator".to_owned()),
            );
        }
        Err(Halt::Yielded) => {
            tracing::info!("tone-map census yielded the hardware to playback");
            census.set(
                generation,
                CensusState::Yielded,
                None,
                None,
                Some("yielded to playback — run it again when the node is idle".to_owned()),
            );
        }
        Err(Halt::Failed(reason)) => {
            tracing::warn!(%reason, "tone-map census failed");
            census.set(generation, CensusState::Failed, None, None, Some(reason));
        }
    }
}

/// Await `step` unless the census is stopped or playback wants the hardware
/// first. Either way the step's future is dropped, which kills its child.
async fn guarded<F: std::future::Future, S: CensusSlot>(
    step: F,
    stop: &CancellationToken,
    slot: &S,
) -> Result<F::Output, Halt> {
    let mut step = std::pin::pin!(step);
    let mut poll = tokio::time::interval(POLL);
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => return Err(Halt::Stopped),
            _ = poll.tick() => {
                if slot.must_yield() {
                    return Err(Halt::Yielded);
                }
            }
            out = &mut step => return Ok(out),
        }
    }
}

async fn run<T: Tools, S: CensusSlot>(
    census: &ToneMapCensus,
    generation: u64,
    stop: &CancellationToken,
    slot: &S,
    tools: &T,
    plan: &CensusPlan,
    work: &Path,
) -> Result<CensusResult, Halt> {
    // Stills are staged under `work` and moved beside the result only when
    // the whole run finishes, so a stopped, yielded or failed run never
    // replaces the stills a stored result names.
    let out_dir = work.join("stills");
    for dir in [work, &out_dir] {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| Halt::Failed(format!("could not create {}: {e}", dir.display())))?;
    }

    // The signals, authored here and encoded once per run.
    let mut sources = Vec::new();
    for spec in SIGNALS {
        census.running(
            generation,
            format!("encoding the {} signal", spec.name()),
            None,
        );
        let raw = work.join(format!("{}.yuv", spec.name()));
        tokio::fs::write(&raw, spec.author_frame(WIDTH, HEIGHT))
            .await
            .map_err(|e| {
                Halt::Failed(format!("could not write the {} signal: {e}", spec.name()))
            })?;
        let clip = work.join(format!("{}.mkv", spec.name()));
        let output = guarded(
            tools.ffmpeg(encode_args(spec, &raw, &clip), Stdout::Discard),
            stop,
            slot,
        )
        .await?
        .and_then(|output| check_exit(&output));
        if let Err(e) = output {
            return Err(Halt::Failed(format!(
                "could not encode the {} signal: {e}",
                spec.name()
            )));
        }
        let _ = tokio::fs::remove_file(&raw).await;
        sources.push((spec, clip));
    }

    // The baseline, twice per signal for the noise floor.
    let mut baseline = Vec::new();
    let mut reference = Vec::new();
    for (spec, clip) in &sources {
        census.running(
            generation,
            format!("CPU baseline · {}", spec.name()),
            Some(Pipeline::Cpu.name()),
        );
        let args = graph_args(Pipeline::Cpu, plan.encoder, clip, spec.peak_nits);
        let first = capture(tools, stop, slot, args.clone()).await?;
        let second = capture(tools, stop, slot, args).await?;
        let measured = first
            .and_then(|a| Ok((signal::block_means(&a, WIDTH, HEIGHT)?, a)))
            .and_then(|(ma, a)| Ok((ma, a, signal::block_means(&second?, WIDTH, HEIGHT)?)));
        let (first_means, first_frame, second_means) = measured.map_err(|e| {
            Halt::Failed(format!(
                "the CPU baseline did not run on {}: {e}",
                spec.name()
            ))
        })?;
        let still = save_still(
            tools,
            stop,
            slot,
            work,
            &out_dir,
            Pipeline::Cpu,
            *spec,
            &first_frame,
        )
        .await?;
        baseline.push(BaselineResult {
            signal: spec.name(),
            brightest_block_luma: signal::brightest_block_luma(&first_means),
            noise_floor: signal::distance(&first_means, &second_means).overall_max(),
            still,
        });
        reference.push(first_means);
    }

    // Every graph the encoder pairs with, not only the one selected.
    let mut candidates = Vec::new();
    for candidate in PIPELINE_CANDIDATES
        .iter()
        .copied()
        .filter(|c| *c != Pipeline::Cpu && c.pairs_with(plan.encoder))
    {
        let mut signals = Vec::new();
        for ((spec, clip), reference) in sources.iter().zip(&reference) {
            census.running(
                generation,
                format!("{} · {}", candidate.name(), spec.name()),
                Some(candidate.name()),
            );
            let args = graph_args(candidate, plan.encoder, clip, spec.peak_nits);
            let frame = capture(tools, stop, slot, args).await?;
            let row = match frame {
                Err(reason) => CandidateSignal {
                    signal: spec.name(),
                    started: false,
                    reason: Some(reason),
                    distance: None,
                    brightest_block_luma: None,
                    still: None,
                },
                Ok(frame) => match signal::block_means(&frame, WIDTH, HEIGHT) {
                    Err(reason) => CandidateSignal {
                        signal: spec.name(),
                        started: true,
                        reason: Some(reason),
                        distance: None,
                        brightest_block_luma: None,
                        still: None,
                    },
                    Ok(means) => CandidateSignal {
                        signal: spec.name(),
                        started: true,
                        reason: None,
                        distance: Some(signal::distance(&means, reference)),
                        brightest_block_luma: Some(signal::brightest_block_luma(&means)),
                        still: save_still(
                            tools, stop, slot, work, &out_dir, candidate, *spec, &frame,
                        )
                        .await?,
                    },
                },
            };
            signals.push(row);
        }
        candidates.push(CandidateResult {
            pipeline: candidate.name().to_owned(),
            label: candidate.label().to_owned(),
            selected: candidate == plan.selected,
            signals,
        });
    }

    Ok(CensusResult {
        key: plan.key.clone(),
        finished_at: crate::state::clock_ms(),
        selected: plan.selected.name().to_owned(),
        width: WIDTH,
        height: HEIGHT,
        grid: [signal::GRID_COLS, signal::GRID_ROWS],
        signals: SIGNALS
            .iter()
            .map(|spec| SignalInfo {
                name: spec.name(),
                max_cll: spec.peak_nits,
                max_fall: spec.max_fall_nits(),
            })
            .collect(),
        noise_floor_zero: baseline.iter().all(|b| b.noise_floor == 0.0),
        baseline,
        candidates,
    })
}

/// Run a graph and return its last complete decoded frame, or why there is
/// none. `Err(Halt)` only for a stop or a yield.
async fn capture<T: Tools, S: CensusSlot>(
    tools: &T,
    stop: &CancellationToken,
    slot: &S,
    args: Vec<String>,
) -> Result<Result<Vec<u8>, String>, Halt> {
    let output = guarded(tools.ffmpeg(args, Stdout::Capture), stop, slot).await?;
    Ok(output.and_then(|output| {
        check_exit(&output)?;
        last_frame(&output.stdout, WIDTH, HEIGHT)
    }))
}

/// The last whole `yuv420p` frame in `bytes`. A byte count that is not a
/// whole number of frames means the graph produced a different size, which
/// is reported rather than measured.
fn last_frame(bytes: &[u8], width: usize, height: usize) -> Result<Vec<u8>, String> {
    let frame = signal::yuv420p_frame_bytes(width, height);
    if bytes.is_empty() {
        return Err("the graph produced no frames".to_owned());
    }
    if !bytes.len().is_multiple_of(frame) {
        return Err(format!(
            "the graph produced {} bytes, not a whole number of {width}x{height} frames",
            bytes.len()
        ));
    }
    Ok(bytes[bytes.len() - frame..].to_vec())
}

/// Write `frame` as a PNG named for the graph and the signal. A still that
/// cannot be written is recorded as absent; the numbers stand without it.
#[allow(clippy::too_many_arguments)]
async fn save_still<T: Tools, S: CensusSlot>(
    tools: &T,
    stop: &CancellationToken,
    slot: &S,
    work: &Path,
    out_dir: &Path,
    graph: Pipeline,
    spec: SignalSpec,
    frame: &[u8],
) -> Result<Option<String>, Halt> {
    let name = still_name(graph, spec);
    let raw = work.join("still.yuv");
    if tokio::fs::write(&raw, frame).await.is_err() {
        return Ok(None);
    }
    let output = guarded(
        tools.ffmpeg(still_args(&raw, &out_dir.join(&name)), Stdout::Discard),
        stop,
        slot,
    )
    .await?;
    let _ = tokio::fs::remove_file(&raw).await;
    Ok(output
        .and_then(|output| check_exit(&output))
        .ok()
        .map(|()| name))
}

/// `<graph>-<signal>.png`: only lowercase letters, digits, `_` and `-`.
pub(crate) fn still_name(graph: Pipeline, spec: SignalSpec) -> String {
    format!("{}-{}.png", graph.name(), spec.name())
}

/// Encode one authored frame to a short HEVC Main10 clip carrying the
/// signal's real HDR10 metadata: PQ, BT.2020, mastering display and content
/// light level. A normal CRF rather than lossless, so every hardware decoder
/// takes it the way it takes a title.
fn encode_args(spec: SignalSpec, raw: &Path, clip: &Path) -> Vec<String> {
    let peak = spec.peak_nits;
    let x265 = format!(
        "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:range=limited:\
         master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L({},1):\
         max-cll={peak},{}:keyint={FRAMES_ENCODED}",
        u64::from(peak) * 10_000,
        spec.max_fall_nits(),
    );
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-y", "-stream_loop"]
        .map(str::to_owned)
        .to_vec();
    args.push((FRAMES_ENCODED - 1).to_string());
    args.extend(["-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-video_size"].map(str::to_owned));
    args.push(format!("{WIDTH}x{HEIGHT}"));
    args.extend(["-framerate", "24", "-i"].map(str::to_owned));
    args.push(raw.to_string_lossy().into_owned());
    args.extend(["-frames:v".to_owned(), FRAMES_ENCODED.to_string()]);
    args.extend(
        [
            "-an",
            "-c:v",
            "libx265",
            "-preset",
            "fast",
            "-crf",
            "12",
            "-pix_fmt",
            "yuv420p10le",
            "-color_primaries",
            "bt2020",
            "-color_trc",
            "smpte2084",
            "-colorspace",
            "bt2020nc",
            "-color_range",
            "tv",
            "-x265-params",
        ]
        .map(str::to_owned),
    );
    args.push(x265);
    args.push(clip.to_string_lossy().into_owned());
    args
}

/// The command one graph is measured with: its production device and decode
/// flags and its production filter string, unchanged, decoded to raw 8-bit
/// `yuv420p` on stdout instead of encoded.
///
/// The vendor graphs end in GPU surfaces; they get the same
/// `hwdownload,format=nv12` the production builder appends before a subtitle
/// composite. libplacebo and OpenCL already end with their own download. The
/// CPU baseline is the production chain in production order — scale, then
/// [`zscale_tone_map_filter`] at the signal's own peak — with no device flags,
/// since nothing is encoded.
pub(crate) fn graph_args(
    candidate: Pipeline,
    encoder: Encoder,
    source: &Path,
    peak_nits: u32,
) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error"]
        .map(str::to_owned)
        .to_vec();
    if candidate != Pipeline::Cpu {
        args.extend(candidate.device_args(encoder));
        args.extend(candidate.decode_args());
    }
    args.push("-i".to_owned());
    args.push(source.to_string_lossy().into_owned());
    let vf = match candidate.filters(None, HEIGHT as i64, Some("hdr10")) {
        Some(graph) if candidate.keeps_frames_off_the_cpu() => {
            format!("{graph},hwdownload,format=nv12")
        }
        Some(graph) => graph,
        None => format!(
            "scale=-2:'min({HEIGHT},ih)',{}",
            zscale_tone_map_filter("smpte2084", peak_nits)
        ),
    };
    args.extend(["-vf".to_owned(), vf]);
    args.extend(["-frames:v".to_owned(), FRAMES_DECODED.to_string()]);
    args.extend(["-an", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"].map(str::to_owned));
    args
}

/// A captured BT.709 limited-range frame to an sRGB PNG.
fn still_args(raw: &Path, png: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-video_size",
    ]
    .map(str::to_owned)
    .to_vec();
    args.push(format!("{WIDTH}x{HEIGHT}"));
    args.push("-i".to_owned());
    args.push(raw.to_string_lossy().into_owned());
    args.extend(
        [
            "-frames:v",
            "1",
            "-vf",
            "scale=in_color_matrix=bt709:in_range=tv,format=rgb24",
        ]
        .map(str::to_owned),
    );
    args.push(png.to_string_lossy().into_owned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::{Admissions, Priority};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    fn success() -> std::process::ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(0)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(0)
        }
    }

    /// A stand-in for the spawn seam. Encodes and stills succeed; a graph
    /// run returns a flat frame whose luma is `luma(args)`; or, with `hang`,
    /// every call parks forever and records when its future is dropped —
    /// which is exactly what kills a real child (kill-on-drop, job-owned).
    struct FakeTools {
        calls: AtomicUsize,
        hang: bool,
        /// Hang from this call on (0-based), after the earlier calls ran.
        hang_from: Option<usize>,
        dropped: Arc<AtomicBool>,
        luma: fn(&[String]) -> u8,
    }

    impl FakeTools {
        fn new(luma: fn(&[String]) -> u8) -> FakeTools {
            FakeTools {
                calls: AtomicUsize::new(0),
                hang: false,
                hang_from: None,
                dropped: Arc::new(AtomicBool::new(false)),
                luma,
            }
        }
    }

    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl Tools for FakeTools {
        async fn ffmpeg(
            &self,
            args: Vec<String>,
            stdout: Stdout,
        ) -> Result<std::process::Output, String> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if self.hang || self.hang_from.is_some_and(|from| call >= from) {
                let _flag = DropFlag(Arc::clone(&self.dropped));
                std::future::pending::<()>().await;
            }
            let mut out = Vec::new();
            if stdout == Stdout::Capture {
                let y = (self.luma)(&args);
                let mut frame = vec![y; WIDTH * HEIGHT];
                frame.extend(std::iter::repeat_n(128, WIDTH * HEIGHT / 2));
                for _ in 0..FRAMES_DECODED {
                    out.extend_from_slice(&frame);
                }
            }
            // A still: create the named file so the result can name it.
            if let Some(png) = args.last().filter(|a| a.ends_with(".png")) {
                std::fs::write(png, b"png").map_err(|e| e.to_string())?;
            }
            Ok(std::process::Output {
                status: success(),
                stdout: out,
                stderr: Vec::new(),
            })
        }

        async fn ffprobe(&self, _args: Vec<String>) -> Result<std::process::Output, String> {
            Err("the census never runs ffprobe".to_owned())
        }
    }

    fn slot(admissions: &Admissions) -> AdmissionSlot {
        AdmissionSlot {
            admissions: admissions.clone(),
            heavy: Arc::new(tokio::sync::Semaphore::new(1)),
            hardware_max: 2,
            software_budget: 8,
            hardware: true,
        }
    }

    fn plan(root: &Path, encoder: Encoder) -> CensusPlan {
        CensusPlan {
            encoder,
            selected: Pipeline::TonemapVaapi,
            key: CensusKey {
                ffmpeg_version: "ffmpeg version test".to_owned(),
                encoder_family: encoder.family_name().to_owned(),
                gpu_device: "/dev/dri/renderD128".to_owned(),
            },
            root: root.to_path_buf(),
        }
    }

    async fn settle(census: &ToneMapCensus, want: CensusState) {
        for _ in 0..200 {
            if census.current().state == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("census never reached {want:?}: {:?}", census.current());
    }

    /// A real census on this host, through the production spawn seam, for a
    /// manual smoke run: `PLURX_CENSUS_SMOKE=vaapi|qsv|software cargo test -p
    /// plurxd --bin plurxd census_smoke -- --ignored --nocapture`. Prints the
    /// stored result. Never part of CI: it needs a GPU and real ffmpeg.
    #[tokio::test]
    #[ignore = "needs real ffmpeg and, for a hardware encoder, a GPU"]
    async fn census_smoke_on_this_host() {
        let encoder = match std::env::var("PLURX_CENSUS_SMOKE").as_deref() {
            Ok("vaapi") => Encoder::Vaapi,
            Ok("qsv") => Encoder::Qsv,
            _ => Encoder::Software,
        };
        let root = crate::test_tempdir().expect("test fixture");
        let cache = root.path().join("runtime");
        std::fs::create_dir_all(&cache).expect("test fixture");
        let census = ToneMapCensus::default();
        let admissions = Admissions::new();
        let (generation, stop) = census.begin().expect("test fixture");
        let mut plan = plan(root.path(), encoder);
        plan.key = CensusKey::for_node("smoke", encoder);
        run_on_node(&census, generation, stop, slot(&admissions), &cache, plan).await;
        let status = census.current();
        println!("{status:#?}");
        let key = CensusKey::for_node("smoke", encoder);
        let view = census.view(root.path(), key.clone()).await;
        println!(
            "{}",
            serde_json::to_string_pretty(&view).expect("test fixture")
        );
        if let Ok(keep) = std::env::var("PLURX_CENSUS_SMOKE_KEEP") {
            let from = root.path().join(key.slug());
            std::fs::create_dir_all(&keep).expect("test fixture");
            for entry in std::fs::read_dir(&from).expect("test fixture") {
                let entry = entry.expect("test fixture");
                std::fs::copy(entry.path(), Path::new(&keep).join(entry.file_name()))
                    .expect("test fixture");
            }
        }
        assert_eq!(status.state, CensusState::Finished, "{status:?}");
    }

    #[test]
    fn graph_args_use_each_production_builder_unchanged() {
        let src = Path::new("/w/maxcll-4000.mkv");
        let cpu = graph_args(Pipeline::Cpu, Encoder::Vaapi, src, 4000);
        let vf = &cpu[cpu.iter().position(|a| a == "-vf").expect("test fixture") + 1];
        assert_eq!(
            *vf,
            format!(
                "scale=-2:'min(1080,ih)',{}",
                zscale_tone_map_filter("smpte2084", 4000)
            ),
            "the baseline is the production chain, scale first, at the signal's peak"
        );
        assert!(vf.contains("peak=40"));
        assert!(
            !cpu.contains(&"-vaapi_device".to_owned()),
            "no device for the baseline"
        );
        for candidate in [
            Pipeline::TonemapVaapi,
            Pipeline::LibplaceboVaapi,
            Pipeline::Libplacebo,
            Pipeline::TonemapOpencl,
        ] {
            let args = graph_args(candidate, Encoder::Vaapi, src, 4000);
            let vf = &args[args.iter().position(|a| a == "-vf").expect("test fixture") + 1];
            let production = candidate
                .filters(None, 1080, Some("hdr10"))
                .expect("test fixture");
            assert!(vf.starts_with(&production), "{candidate:?}: {vf}");
            assert_eq!(
                vf.ends_with(",hwdownload,format=nv12") && vf != &production,
                candidate.keeps_frames_off_the_cpu()
            );
            let device = candidate.device_args(Encoder::Vaapi);
            assert_eq!(&args[3..3 + device.len()], device.as_slice());
            assert!(
                args.ends_with(&["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"].map(String::from))
            );
        }
    }

    #[test]
    fn the_signal_clip_carries_its_own_hdr10_metadata() {
        for spec in SIGNALS {
            let args = encode_args(spec, Path::new("/w/s.yuv"), Path::new("/w/s.mkv"));
            let x265 = &args[args
                .iter()
                .position(|a| a == "-x265-params")
                .expect("test fixture")
                + 1];
            assert!(
                x265.contains(&format!("L({},1)", u64::from(spec.peak_nits) * 10_000)),
                "{x265}"
            );
            assert!(
                x265.contains(&format!(
                    "max-cll={},{}",
                    spec.peak_nits,
                    spec.max_fall_nits()
                )),
                "{x265}"
            );
            assert!(x265.contains("transfer=smpte2084") && x265.contains("colorprim=bt2020"));
            assert!(args.windows(2).any(|w| w == ["-c:v", "libx265"]));
            assert!(args.windows(2).any(|w| w[0] == "-crf"), "not lossless");
            assert!(args.windows(2).any(|w| w == ["-pix_fmt", "yuv420p10le"]));
        }
    }

    #[test]
    fn a_frame_of_the_wrong_size_is_reported_not_measured() {
        let one = signal::yuv420p_frame_bytes(WIDTH, HEIGHT);
        assert!(last_frame(&[], WIDTH, HEIGHT).is_err());
        assert!(last_frame(&vec![0; one + 7], WIDTH, HEIGHT).is_err());
        let mut two = vec![1; one];
        two.extend(vec![2; one]);
        assert_eq!(
            last_frame(&two, WIDTH, HEIGHT).expect("test fixture"),
            vec![2; one]
        );
    }

    #[test]
    fn the_storage_key_round_trips_and_separates_nodes() {
        let key = CensusKey::for_node("ffmpeg version 8.1.3-Jellyfin", Encoder::Software);
        assert_eq!(key.gpu_device, "none");
        let json = serde_json::to_string(&key).expect("test fixture");
        assert_eq!(
            serde_json::from_str::<CensusKey>(&json).expect("test fixture"),
            key
        );
        assert_eq!(key.slug(), key.clone().slug());
        assert_eq!(key.slug().len(), 16);
        assert!(key.slug().bytes().all(|b| b.is_ascii_hexdigit()));
        for other in [
            CensusKey {
                ffmpeg_version: "ffmpeg version 9.0.1".to_owned(),
                ..key.clone()
            },
            CensusKey {
                encoder_family: "qsv".to_owned(),
                ..key.clone()
            },
            CensusKey {
                gpu_device: "/dev/dri/renderD129".to_owned(),
                ..key.clone()
            },
        ] {
            assert_ne!(other.slug(), key.slug(), "{other:?}");
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn a_finished_census_is_stored_under_its_key_and_read_back() {
        let root = crate::test_tempdir().expect("test fixture");
        let census = ToneMapCensus::default();
        let admissions = Admissions::new();
        // vpp_qsv is not paired with VA-API, so the candidates are the four
        // VA-API-compatible graphs. tonemap_vaapi lands 6 levels darker than
        // the baseline; the others match it.
        let tools = FakeTools::new(|args| {
            let vf = &args[args.iter().position(|a| a == "-vf").expect("test fixture") + 1];
            if vf.contains("tonemap_vaapi") {
                94
            } else {
                100
            }
        });
        let (generation, stop) = census.begin().expect("test fixture");
        drive(
            &census,
            generation,
            stop,
            &slot(&admissions),
            &tools,
            plan(root.path(), Encoder::Vaapi),
        )
        .await;
        assert_eq!(
            census.current().state,
            CensusState::Finished,
            "{:?}",
            census.current()
        );
        assert_eq!(admissions.in_use(), 0, "the permit is released");
        assert!(!root.path().join("work").exists(), "scratch is cleaned up");

        let key = plan(root.path(), Encoder::Vaapi).key;
        let fresh = ToneMapCensus::default();
        let result = fresh
            .view(root.path(), key.clone())
            .await
            .result
            .expect("stored result");
        assert_eq!(result.key, key);
        assert_eq!(result.signals.len(), 2);
        assert!(result.noise_floor_zero);
        let names: Vec<_> = result
            .candidates
            .iter()
            .map(|c| c.pipeline.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "tonemap_vaapi",
                "libplacebo_vaapi",
                "libplacebo",
                "tonemap_opencl"
            ]
        );
        let vaapi = &result.candidates[0];
        assert!(vaapi.selected);
        let d = vaapi.signals[0].distance.expect("test fixture");
        assert_eq!(d.max, [6.0, 0.0, 0.0]);
        assert_eq!(d.p95, [6.0, 0.0, 0.0]);
        assert_eq!(vaapi.signals[0].brightest_block_luma, Some(94.0));
        assert_eq!(
            result.candidates[1].signals[1]
                .distance
                .expect("test fixture")
                .overall_max(),
            0.0
        );
        // Every still is named, on disk, and servable; nothing else is.
        assert_eq!(result.stills().count(), 2 + 4 * 2);
        let still = still_name(Pipeline::TonemapVaapi, SIGNALS[1]);
        let path = fresh
            .still_path(root.path(), key.clone(), &still)
            .await
            .expect("named still");
        assert!(path.exists());
        assert!(fresh
            .still_path(root.path(), key.clone(), "../census.json")
            .await
            .is_none());
        // A different key sees no result.
        let other = CensusKey {
            encoder_family: "qsv".to_owned(),
            ..key
        };
        assert!(fresh.view(root.path(), other).await.result.is_none());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn the_census_waits_while_a_viewer_holds_the_hardware() {
        let root = crate::test_tempdir().expect("test fixture");
        let census = Arc::new(ToneMapCensus::default());
        let admissions = Admissions::new();
        let viewer = admissions
            .try_acquire(2, Priority::Live)
            .expect("a viewer plays");
        let tools = Arc::new(FakeTools::new(|_| 100));
        let (generation, stop) = census.begin().expect("test fixture");
        assert!(census.begin().is_none(), "one census at a time");
        let task = {
            let (census, tools, slot, plan) = (
                Arc::clone(&census),
                Arc::clone(&tools),
                slot(&admissions),
                plan(root.path(), Encoder::Vaapi),
            );
            tokio::spawn(
                async move { drive(&census, generation, stop, &slot, &*tools, plan).await },
            )
        };
        // Several admission retries later it is still queued and has started
        // nothing at all.
        tokio::time::sleep(POLL * 3).await;
        assert_eq!(census.current().state, CensusState::Queued);
        assert_eq!(
            tools.calls.load(Ordering::SeqCst),
            0,
            "no child while a viewer holds the slot"
        );
        // The viewer leaves; the census takes the slot and runs to the end.
        drop(viewer);
        task.await.expect("test fixture");
        assert_eq!(census.current().state, CensusState::Finished);
        assert!(tools.calls.load(Ordering::SeqCst) > 0);
    }

    /// Pretranscodes, fragment indexing and the transcoder benchmark share
    /// one heavy-background gate. While another background worker holds it
    /// the census stays queued and starts nothing; it runs once it is free.
    #[tokio::test]
    #[cfg(unix)]
    async fn the_census_waits_while_another_background_worker_holds_the_gate() {
        let root = crate::test_tempdir().expect("test fixture");
        let census = Arc::new(ToneMapCensus::default());
        let admissions = Admissions::new();
        let census_slot = slot(&admissions);
        let worker = Arc::clone(&census_slot.heavy)
            .try_acquire_owned()
            .expect("a pretranscode holds the gate");
        let tools = Arc::new(FakeTools::new(|_| 100));
        let (generation, stop) = census.begin().expect("test fixture");
        let task = {
            let (census, tools, plan) = (
                Arc::clone(&census),
                Arc::clone(&tools),
                plan(root.path(), Encoder::Vaapi),
            );
            tokio::spawn(async move {
                drive(&census, generation, stop, &census_slot, &*tools, plan).await
            })
        };
        tokio::time::sleep(POLL * 3).await;
        assert_eq!(census.current().state, CensusState::Queued);
        assert_eq!(tools.calls.load(Ordering::SeqCst), 0);
        drop(worker);
        task.await.expect("test fixture");
        assert_eq!(census.current().state, CensusState::Finished);
    }

    /// A hardware census on a node configured for no hardware sessions can
    /// never be admitted: it fails at once with the reason, not queued
    /// forever.
    #[tokio::test]
    async fn a_node_with_no_hardware_sessions_fails_the_census_at_once() {
        let root = crate::test_tempdir().expect("test fixture");
        let census = ToneMapCensus::default();
        let admissions = Admissions::new();
        let mut census_slot = slot(&admissions);
        census_slot.hardware_max = 0;
        let tools = FakeTools::new(|_| 100);
        let (generation, stop) = census.begin().expect("test fixture");
        tokio::time::timeout(
            POLL,
            drive(
                &census,
                generation,
                stop,
                &census_slot,
                &tools,
                plan(root.path(), Encoder::Vaapi),
            ),
        )
        .await
        .expect("fails without waiting");
        let current = census.current();
        assert_eq!(current.state, CensusState::Failed);
        assert_eq!(tools.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn stop_kills_the_running_child_and_releases_the_slot() {
        let root = crate::test_tempdir().expect("test fixture");
        let census = Arc::new(ToneMapCensus::default());
        let admissions = Admissions::new();
        let mut fake = FakeTools::new(|_| 100);
        fake.hang = true;
        let tools = Arc::new(fake);
        let (generation, stop) = census.begin().expect("test fixture");
        let task = {
            let (census, tools, slot, plan) = (
                Arc::clone(&census),
                Arc::clone(&tools),
                slot(&admissions),
                plan(root.path(), Encoder::Vaapi),
            );
            tokio::spawn(
                async move { drive(&census, generation, stop, &slot, &*tools, plan).await },
            )
        };
        settle(&census, CensusState::Running).await;
        while tools.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            admissions.background_is_active(),
            "the census holds a background permit"
        );
        assert!(census.stop());
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("stops promptly")
            .expect("test fixture");
        assert_eq!(census.current().state, CensusState::Stopped);
        assert!(
            tools.dropped.load(Ordering::SeqCst),
            "the child's future was dropped, killing it"
        );
        assert!(!admissions.background_is_active(), "the permit is released");
        assert!(!census.stop(), "nothing left to stop");
    }

    /// A run that does not finish never touches the stored result: its
    /// stills are staged and only moved in when the whole run completes.
    #[tokio::test]
    #[cfg(unix)]
    async fn a_stopped_run_leaves_the_stored_result_and_its_stills_alone() {
        let root = crate::test_tempdir().expect("test fixture");
        let census = Arc::new(ToneMapCensus::default());
        let admissions = Admissions::new();
        let first = FakeTools::new(|_| 100);
        let (generation, stop) = census.begin().expect("test fixture");
        drive(
            &census,
            generation,
            stop,
            &slot(&admissions),
            &first,
            plan(root.path(), Encoder::Vaapi),
        )
        .await;
        assert_eq!(census.current().state, CensusState::Finished);
        let full_run = first.calls.load(Ordering::SeqCst);
        let stored = census
            .view(root.path(), plan(root.path(), Encoder::Vaapi).key)
            .await
            .result
            .expect("a stored result");
        let dir = root.path().join(stored.key.slug());
        let stills: Vec<String> = stored.stills().map(str::to_owned).collect();
        for name in &stills {
            std::fs::write(dir.join(name), b"stored").expect("test fixture");
        }
        // The second run writes every still but hangs on its last child, and
        // is stopped there.
        let mut second = FakeTools::new(|_| 50);
        second.hang_from = Some(full_run - 1);
        let second = Arc::new(second);
        let (generation, stop) = census.begin().expect("test fixture");
        let task = {
            let (census, tools, slot, plan) = (
                Arc::clone(&census),
                Arc::clone(&second),
                slot(&admissions),
                plan(root.path(), Encoder::Vaapi),
            );
            tokio::spawn(
                async move { drive(&census, generation, stop, &slot, &*tools, plan).await },
            )
        };
        while second.calls.load(Ordering::SeqCst) < full_run {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(census.stop());
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("stops promptly")
            .expect("test fixture");
        assert_eq!(census.current().state, CensusState::Stopped);
        assert!(
            dir.join("census.json").exists(),
            "the stored result survives"
        );
        for name in &stills {
            assert_eq!(
                std::fs::read(dir.join(name)).expect("test fixture"),
                b"stored",
                "{name} was replaced by an unfinished run"
            );
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn a_live_start_makes_the_running_census_yield() {
        let root = crate::test_tempdir().expect("test fixture");
        let census = Arc::new(ToneMapCensus::default());
        let admissions = Admissions::new();
        let mut fake = FakeTools::new(|_| 100);
        fake.hang = true;
        let tools = Arc::new(fake);
        let (generation, stop) = census.begin().expect("test fixture");
        let task = {
            let (census, tools, slot, plan) = (
                Arc::clone(&census),
                Arc::clone(&tools),
                slot(&admissions),
                plan(root.path(), Encoder::Vaapi),
            );
            tokio::spawn(
                async move { drive(&census, generation, stop, &slot, &*tools, plan).await },
            )
        };
        while tools.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // A viewer presses play: background_must_yield turns true. Within one
        // poll the census kills its child and gives the hardware back.
        let wait = admissions.wait_for_slot();
        assert!(admissions.background_must_yield());
        tokio::time::timeout(POLL * 4, task)
            .await
            .expect("yields within a poll")
            .expect("test fixture");
        assert_eq!(census.current().state, CensusState::Yielded);
        assert!(census
            .current()
            .reason
            .expect("test fixture")
            .contains("yielded to playback"));
        assert!(tools.dropped.load(Ordering::SeqCst));
        assert!(!admissions.background_is_active());
        drop(wait);
        assert!(
            admissions.try_acquire(2, Priority::Live).is_some(),
            "the viewer gets the slot"
        );
    }
}
