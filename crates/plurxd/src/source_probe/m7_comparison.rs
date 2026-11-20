//! An M7 comparison of already-authored evidence under a maintenance title claim.
//! Callers supply independent censuses and actual publication readbacks. A
//! renderer capture can corroborate evidence but cannot supply its own census.

mod census;
pub(super) use census::{observe as observe_census, CensusRequest};

use crate::{
    admission::TranscodeResourceEstimate,
    background_jobs::JobFence,
    dv_tools::ToolSet,
    fragment_index_cluster::SourceFence,
    producer_spawn::{ChildStdio, ChildStdios, Descriptors},
    source_input::SourceInput,
    state::AppState,
    transcode::FragmentAdmission,
};
use plurx_core::{
    domain::MediaFile,
    process::{bounded, source_lifetime, ChildWork},
    store::{
        background_jobs::{JobPayload, JobState},
        dv_title_jobs::TitleDemand,
    },
    transcode::{
        dv_m7_receipt::{
            CalibrationWindow, M7Binding, M7ReceiptError, M7SourceProbe, WindowBinding,
            MODEL_VERSION,
        },
        dv_m7_scoring::{
            aggregation::{
                AggregationError, Aggregator, ComparisonReport, FrozenMargin, WindowShape,
                AGGREGATION_REVISION,
            },
            capture::{
                score_paired_frame, AuthoringFrameBinding, CaptureError, CaptureLayout,
                CapturePlan, CaptureReader, CaptureRole, FplusFrameEvidence, FrameEvidence,
            },
            SCORING_REVISION,
        },
        dv_refit::FIT_VERSION,
        dv_refit_serve::ServedCurveTable,
        Rational,
    },
};
use sha2::{Digest as _, Sha256};
use std::{
    fs::File,
    io::{self, BufReader, Read},
    os::unix::fs::FileExt,
    path::Path,
    process::ExitStatus,
    sync::Arc,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

const MAX_CAPTURE_SOURCE_BYTES: usize = 128 * 1024 * 1024;
const MAX_NAL_BYTES: usize = 4097;
const CAPTURE_HEADER_AND_TRAILER: usize = 88 + 24;
const FRAME_PREFIX_BYTES: usize = 8 + 32 + 3 * 64;
const CAPTURE_OUTPUT: &str = "lattice.bin";
const DIAGNOSTIC_BYTES: usize = 4096;

type Sha256Digest = [u8; 32];

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("M7 evidence unavailable: {0}")]
    Unsupported(&'static str),
    #[error("M7 deferred: {0}")]
    Busy(String),
    #[error("M7 comparison cancelled")]
    Cancelled,
    #[error("M7 authority changed: {0}")]
    Authority(&'static str),
    #[error("M7 sample helper refused the content: {0}")]
    Content(String),
    #[error("M7 execution failed: {0}")]
    Execution(String),
    #[error("M7 capture: {0}")]
    Capture(#[from] CaptureError),
    #[error("M7 aggregation: {0}")]
    Aggregation(#[from] AggregationError),
    #[error("M7 receipt binding: {0}")]
    Receipt(#[from] M7ReceiptError),
}

pub(super) struct TitleContext<'a> {
    pub state: &'a AppState,
    pub file: &'a MediaFile,
    pub demand: &'a TitleDemand,
    pub tools: &'a ToolSet,
    pub admission: &'a FragmentAdmission,
    pub fence: &'a JobFence,
    pub cancel: &'a CancellationToken,
    pub shutdown: &'a CancellationToken,
}

/// Verified source content and its independently observed file generation.
/// The strong digest comes from the source authority that made the census.
pub(super) struct HeldTitle {
    pub source: SourceFence,
    pub content_sha256: Sha256Digest,
    pub probe_sha256: Sha256Digest,
}

/// Frozen before candidate measurements. The candidate preparation receipt
/// must name this exact protocol and recipe.
pub(super) struct FrozenProtocol {
    pub source_sha256: Sha256Digest,
    pub probe_sha256: Sha256Digest,
    pub protocol_sha256: Sha256Digest,
    pub recipe_sha256: Sha256Digest,
    pub fit_table_sha256: Sha256Digest,
    pub calibration: Vec<CalibrationWindow>,
    pub margin: FrozenMargin,
    pub encoder_rate: Rational,
}

/// The preceding authoring stage binds a daemon-held clip, its own complete
/// packet census and the VCL hashes observed before authoring.
pub(super) struct AuthoredClip {
    pub source: SourceFence,
    pub layout: CaptureLayout,
    pub frames: Vec<FrameEvidence>,
    pub bindings: Vec<AuthoringFrameBinding>,
    pub encoded_vcl_sha256: Vec<Sha256Digest>,
    pub protocol_sha256: Sha256Digest,
    pub recipe_sha256: Sha256Digest,
    pub encoder_sha256: Sha256Digest,
    pub encoded_sha256: Sha256Digest,
    pub authored_sha256: Sha256Digest,
    pub authoring_receipt_sha256: Sha256Digest,
}

/// Produced by readback of an actual whole or independent seek publication.
/// These NALs must come from readback, not a local synthesis from the table.
pub(super) struct PublicationReadback {
    pub publication_sha256: Sha256Digest,
    pub readback_sha256: Sha256Digest,
    pub fit_table_sha256: Sha256Digest,
    pub frames: Vec<PublicationFrame>,
}

pub(super) struct PublicationFrame {
    pub source: FrameEvidence,
    pub bl_vcl_sha256: Sha256Digest,
    pub rpu_sha256: Sha256Digest,
    pub rpu_nal: Vec<u8>,
}

impl PublicationFrame {
    fn evidence(&self) -> FplusFrameEvidence<'_> {
        FplusFrameEvidence {
            source: self.source,
            bl_vcl_sha256: self.bl_vcl_sha256,
            rpu_sha256: self.rpu_sha256,
            rpu_nal: &self.rpu_nal,
        }
    }
}

pub(super) struct EvaluationWindow {
    pub layout: CaptureLayout,
    pub source_frames: Vec<FrameEvidence>,
    pub authored: AuthoredClip,
    pub whole: PublicationReadback,
    pub seek: PublicationReadback,
}

pub(super) struct ComparisonInput {
    pub title: HeldTitle,
    pub frozen: FrozenProtocol,
    pub windows: Vec<EvaluationWindow>,
    /// Previously admitted job-local scratch for the exact spill and captures.
    pub scratch_bytes: u64,
}

pub(super) struct Measurement {
    pub custody: M7Binding,
    pub report: ComparisonReport,
    pub measured_at_ms: i64,
}

/// The maintenance caller keeps the title attempt open and awaits this scope
/// through cooperative cancellation. No capture can start without evidence.
pub(super) async fn execute(
    context: TitleContext<'_>,
    input: Option<ComparisonInput>,
) -> Result<Measurement, Error> {
    let input = input.ok_or(Error::Unsupported("independent title comparison evidence"))?;
    if context.demand.playback_requested || context.admission.is_probe_only() {
        return Err(Error::Unsupported("maintenance title admission"));
    }
    let cancel = context.cancel.child_token();
    let expected_source = input.title.source.object_version().to_owned();
    let result = super::run_source_admitted_probe(
        &context.state.transcode,
        context.admission,
        &cancel,
        context.shutdown,
        None,
        context.fence,
        || async {
            context.tools.is_current().await
                && crate::fragment_index_cluster::inspect_source(context.file)
                    .await
                    .is_ok_and(|source| source == expected_source)
        },
        execute_owned(&context, input, &cancel),
    )
    .await;
    check_cancel(&cancel)?;
    result
}

struct AdmissionExtension<'a>(&'a FragmentAdmission);

impl Drop for AdmissionExtension<'_> {
    fn drop(&mut self) {
        self.0.release_extension();
    }
}

async fn execute_owned(
    context: &TitleContext<'_>,
    input: ComparisonInput,
    cancel: &CancellationToken,
) -> Result<Measurement, Error> {
    let token = verify_authority(context, &input.title, cancel).await?;
    let table = current_table(context, input.frozen.fit_table_sha256).await?;
    let shapes = validate_plan(&input, &table)?;
    let spill = Aggregator::required_spill_bytes(&shapes)?;
    let captures = shapes
        .iter()
        .map(|shape| capture_bytes(*shape, 16) + capture_bytes(*shape, 10))
        .max()
        .ok_or(Error::Unsupported("evaluation windows"))?;
    if spill.saturating_add(captures as u64) > input.scratch_bytes {
        return Err(Error::Busy("admitted M7 scratch is too small".into()));
    }

    // One title-worker permit already covers the caller or scorer. A FEL
    // capture can additionally run one frame decoder per software layer.
    let resources = TranscodeResourceEstimate {
        hardware_slot: false,
        cpu_threads: 2,
        decoder_threads: Some(1),
    };
    context
        .state
        .transcode
        .extend_fragment_admission(context.admission, &resources, Some(1))
        .await
        .map_err(Error::Busy)?;
    let _extension = AdmissionExtension(context.admission);
    let scorer_sha256 = scorer_identity(cancel).await?;
    let custody = bind_custody(context, &input, &shapes, scorer_sha256)?;
    custody.key()?;
    let scratch = tempfile::Builder::new()
        .prefix("dv-m7-")
        .tempdir_in(&context.state.runtime_cache_dir)
        .map_err(execution)?;
    let mut aggregator = Aggregator::new(scratch.path(), &shapes, input.frozen.margin, spill)?;
    let mut candidates = Vec::with_capacity(input.windows.len());
    for (window, shape) in input.windows.into_iter().zip(shapes) {
        check_cancel(cancel)?;
        let captures = capture_window(context, &input.title, &window, shape, &scratch).await?;
        let (next, window) = off_runtime({
            let table = Arc::clone(&table);
            let cancel = cancel.clone();
            move || score_window(aggregator, window, captures, &table, &cancel)
        })
        .await?;
        aggregator = next;
        candidates.push(window.authored.source);
    }
    let report = off_runtime({
        let cancel = cancel.clone();
        move || {
            aggregator
                .finish(|| cancel.is_cancelled())
                .map_err(Error::from)
        }
    })
    .await?;

    if candidates.iter().any(|held| !held.unchanged()) {
        return Err(Error::Authority("authored clip"));
    }
    let current = verify_authority(context, &input.title, cancel).await?;
    if (current.job_id, current.claim_id, current.fence)
        != (token.job_id, token.claim_id, token.fence)
        || *current_table(context, custody.fit_table_sha256).await? != *table
        || scorer_identity(cancel).await? != scorer_sha256
    {
        return Err(Error::Authority("title claim, table or scorer"));
    }
    check_cancel(cancel)?;
    Ok(Measurement {
        custody,
        report,
        measured_at_ms: super::now_ms(),
    })
}

async fn verify_authority(
    context: &TitleContext<'_>,
    title: &HeldTitle,
    cancel: &CancellationToken,
) -> Result<plurx_core::store::background_jobs::JobToken, Error> {
    check_cancel(cancel)?;
    let token = context.fence.snapshot().await.ok_or(Error::Cancelled)?;
    let job = context
        .state
        .store
        .background_job(&token.job_id)
        .await
        .map_err(execution)?
        .ok_or(Error::Authority("maintenance job"))?;
    if job.state != JobState::Running
        || job.token.as_ref().is_none_or(|current| {
            current.job_id != token.job_id
                || current.claim_id != token.claim_id
                || current.fence != token.fence
        })
    {
        return Err(Error::Authority("maintenance job claim"));
    }
    let probe = context
        .state
        .store
        .get_file_probe_json(context.file.id)
        .await
        .map_err(execution)?
        .ok_or(Error::Unsupported("current source probe"))?;
    let scanner = context.tools.title_scanner_digest();
    let JobPayload::DvTitleScan {
        file_id,
        source_size,
        source_mtime,
        probe_revision,
        scanner_digest,
    } = job.supported_payload().map_err(execution)?
    else {
        return Err(Error::Authority("maintenance job kind"));
    };
    let demand = context.demand;
    if (file_id, source_size, source_mtime, probe_revision)
        != (
            demand.file_id,
            demand.source_size,
            demand.source_mtime,
            demand.probe_revision,
        )
        || (file_id, source_size, source_mtime)
            != (context.file.id, context.file.size, context.file.mtime)
        || scanner_digest != scanner
        || M7SourceProbe::from_probe_json(&demand.probe_json)?.sha256() != title.probe_sha256
        || M7SourceProbe::from_probe_json(&probe)?.sha256() != title.probe_sha256
        || !title.source.unchanged()
        || title.source.reopen(context.file).await.is_err()
        || !context.tools.is_current().await
    {
        return Err(Error::Authority("held source, probe or helper bundle"));
    }
    Ok(token)
}

async fn current_table(
    context: &TitleContext<'_>,
    expected: Sha256Digest,
) -> Result<Arc<ServedCurveTable>, Error> {
    let table = crate::dv_refit_serving::current_table(context.state.store.as_ref(), context.file)
        .await
        .map_err(Error::Execution)?
        .ok_or(Error::Unsupported("current-method fitted table"))?;
    if table.digest() != hex::encode(expected) {
        return Err(Error::Authority("current-method table digest"));
    }
    Ok(table)
}

fn validate_plan(
    input: &ComparisonInput,
    table: &ServedCurveTable,
) -> Result<Vec<WindowShape>, Error> {
    use plurx_core::transcode::dv_m7_scoring::aggregation::MAX_EVALUATION_WINDOWS;
    if input.frozen.source_sha256 != input.title.content_sha256
        || input.frozen.probe_sha256 != input.title.probe_sha256
    {
        return Err(Error::Authority("frozen protocol source and probe"));
    }
    let cal = &input.frozen.calibration;
    if cal.is_empty()
        || cal.len() > MAX_EVALUATION_WINDOWS
        || input.windows.is_empty()
        || input.windows.len() > MAX_EVALUATION_WINDOWS
    {
        return Err(Error::Unsupported(
            "bounded calibration and evaluation windows",
        ));
    }
    let (num, den) = table.time_base();
    let clock = Rational::new(num, den).ok_or(Error::Authority("table clock"))?;
    let mut intervals = Vec::with_capacity(cal.len() + input.windows.len());
    for window in cal {
        if window.time_base != clock {
            return Err(Error::Authority("calibration clock"));
        }
        intervals.push((
            window.first_ordinal,
            window.last_ordinal,
            window.first_pts,
            window.end_pts,
        ));
    }
    let mut shapes = Vec::with_capacity(input.windows.len());
    for window in &input.windows {
        let (first, last, end) = endpoints(&window.source_frames, window.layout)?;
        let authored = &window.authored;
        let (clip_first, _, _) = endpoints(&authored.frames, authored.layout)?;
        let frames = window.source_frames.len();
        if window.layout.role != CaptureRole::SourceFel
            || window.layout.time_base != clock
            || authored.layout.role != CaptureRole::AuthoredCandidate
            || authored.layout.roi != window.layout.roi
            || authored.layout.raster != window.layout.raster
            || clip_first.ordinal != 0
            || clip_first.pts != 0
            || authored.frames.len() != frames
            || authored.encoded_vcl_sha256.len() != frames
            || authored.bindings.len() != frames
            || authored.protocol_sha256 != input.frozen.protocol_sha256
            || authored.recipe_sha256 != input.frozen.recipe_sha256
            || window.whole.fit_table_sha256 != input.frozen.fit_table_sha256
            || window.seek.fit_table_sha256 != input.frozen.fit_table_sha256
            || window.whole.frames.len() != frames
            || window.seek.frames.len() != frames
        {
            return Err(Error::Authority(
                "window plan, recipe or production publication",
            ));
        }
        for (index, (source, candidate)) in window
            .source_frames
            .iter()
            .zip(&authored.frames)
            .enumerate()
        {
            let binding = &authored.bindings[index];
            let whole = &window.whole.frames[index];
            let seek = &window.seek.frames[index];
            if binding.source != *source
                || binding.candidate != *candidate
                || binding.crop_first_source_ordinal != first.ordinal
                || binding.crop_frames != frames as u32
                || !on_encoder_grid(candidate, input.frozen.encoder_rate)
                || candidate.bl_vcl_sha256 != authored.encoded_vcl_sha256[index]
                || whole.source != *source
                || seek.source != *source
                || whole.bl_vcl_sha256 != source.bl_vcl_sha256
                || seek.bl_vcl_sha256 != source.bl_vcl_sha256
                || [whole, seek].into_iter().any(|readback| {
                    !(3..=MAX_NAL_BYTES).contains(&readback.rpu_nal.len())
                        || digest(&readback.rpu_nal) != readback.rpu_sha256
                })
            {
                return Err(Error::Authority(
                    "per-frame authoring or publication readback",
                ));
            }
        }
        intervals.push((first.ordinal, last.ordinal, first.pts, end));
        shapes.push(WindowShape {
            frames,
            sites_per_frame: window.layout.sites_per_frame()?,
        });
    }
    intervals.sort_unstable();
    if intervals
        .iter()
        .any(|(first, last, pts, end)| first > last || pts >= end)
        || intervals
            .windows(2)
            .any(|pair| pair[0].1 >= pair[1].0 || pair[0].3 > pair[1].2)
    {
        return Err(Error::Authority(
            "disjoint calibration and evaluation windows",
        ));
    }
    Ok(shapes)
}

fn on_encoder_grid(frame: &FrameEvidence, encoder_rate: Rational) -> bool {
    let scale = i128::from(frame.time_base.numerator()) * i128::from(encoder_rate.numerator());
    let step = i128::from(frame.time_base.denominator()) * i128::from(encoder_rate.denominator());
    i128::from(frame.pts) * scale == i128::from(frame.ordinal) * step
        && i128::from(frame.duration) * scale == step
}

fn endpoints(
    frames: &[FrameEvidence],
    layout: CaptureLayout,
) -> Result<(FrameEvidence, FrameEvidence, i64), Error> {
    use plurx_core::transcode::dv_m7_scoring::aggregation::MAX_FRAMES_PER_WINDOW;
    let (&first, &last) = frames
        .first()
        .zip(frames.last())
        .ok_or(Error::Unsupported("independent frame census"))?;
    let end = last
        .pts
        .checked_add(last.duration)
        .ok_or(CaptureError::Census)?;
    if frames.len() > MAX_FRAMES_PER_WINDOW
        || layout.video_index > 255
        || last.ordinal >= 1_000_000
        || first.pts == i64::MIN
        || end == i64::MAX
        || frames.windows(2).any(|pair| pair[0].pts >= pair[1].pts)
        || frames.iter().enumerate().any(|(i, frame)| {
            first.ordinal.checked_add(i as u64) != Some(frame.ordinal)
                || frame.duration <= 0
                || frame.time_base != layout.time_base
                || frame
                    .pts
                    .checked_add(frame.duration)
                    .is_none_or(|frame_end| frame_end > end)
        })
    {
        return Err(CaptureError::Census.into());
    }
    Ok((first, last, end))
}

struct CaptureFiles {
    source_status: ExitStatus,
    candidate_status: ExitStatus,
    source_dir: TempDir,
    candidate_dir: TempDir,
}

async fn capture_window(
    context: &TitleContext<'_>,
    title: &HeldTitle,
    window: &EvaluationWindow,
    shape: WindowShape,
    scratch: &TempDir,
) -> Result<CaptureFiles, Error> {
    let source_dir = tempfile::tempdir_in(scratch.path()).map_err(execution)?;
    let candidate_dir = tempfile::tempdir_in(scratch.path()).map_err(execution)?;
    let candidate = &window.authored;
    let held = SourceFence::clone_held(&candidate.source.handle, candidate.source.object_version())
        .map_err(Error::Execution)?;
    let expected = candidate.authored_sha256;
    let cancel = bounded::cancellation().ok_or(Error::Cancelled)?;
    off_runtime(move || verify_candidate(&held, expected, &cancel)).await?;
    for (source, plan) in [
        (
            &title.source,
            CapturePlan {
                layout: window.layout,
                frames: &window.source_frames,
            },
        ),
        (
            &candidate.source,
            CapturePlan {
                layout: candidate.layout,
                frames: &candidate.frames,
            },
        ),
    ] {
        let observed = observe_census(context, source, request(plan)?, scratch).await?;
        if observed.as_slice() != plan.frames {
            return Err(Error::Authority(
                "independent coded census differs from frozen evidence",
            ));
        }
    }
    let source_status = capture(
        context,
        &title.source,
        CapturePlan {
            layout: window.layout,
            frames: &window.source_frames,
        },
        &source_dir,
        capture_bytes(shape, 16),
    )
    .await?;
    let candidate_status = capture(
        context,
        &candidate.source,
        CapturePlan {
            layout: candidate.layout,
            frames: &candidate.frames,
        },
        &candidate_dir,
        capture_bytes(shape, 10),
    )
    .await?;
    Ok(CaptureFiles {
        source_status,
        candidate_status,
        source_dir,
        candidate_dir,
    })
}

async fn capture(
    context: &TitleContext<'_>,
    source: &SourceFence,
    plan: CapturePlan<'_>,
    dir: &TempDir,
    max_bytes: usize,
) -> Result<ExitStatus, Error> {
    let request = request(plan)?;
    let mode = match request.layout.role {
        CaptureRole::SourceFel => "--m7-sample-fel-v1",
        CaptureRole::AuthoredCandidate => "--m7-sample-candidate-v1",
    };
    run_renderer(context, source, request, mode, dir, max_bytes).await
}

fn request(plan: CapturePlan<'_>) -> Result<CensusRequest, Error> {
    let (first, last, end_pts) = endpoints(plan.frames, plan.layout)?;
    Ok(CensusRequest {
        layout: plan.layout,
        first: first.ordinal,
        last: last.ordinal,
        first_pts: first.pts,
        end_pts,
    })
}

async fn run_renderer(
    context: &TitleContext<'_>,
    source: &SourceFence,
    request: CensusRequest,
    mode: &'static str,
    dir: &TempDir,
    max_bytes: usize,
) -> Result<ExitStatus, Error> {
    bounded::check_cancellation().map_err(|_| Error::Cancelled)?;
    let dir_handle =
        plurx_core::fs_secure::open_directory_nofollow_blocking(dir.path()).map_err(execution)?;
    let _dir_guard =
        source_lifetime::register_daemon_file(&dir_handle, dir.path()).map_err(execution)?;
    let stdout_path = dir.path().join("stdout");
    let stdout = File::create_new(&stdout_path).map_err(execution)?;
    let _stdout_guard =
        source_lifetime::register_daemon_file(&stdout, &stdout_path).map_err(execution)?;
    let source = SourceFence::clone_held(&source.handle, source.object_version())
        .map_err(Error::Execution)?;
    let input = SourceInput::open_background_held(
        source,
        "application/octet-stream",
        context.fence.source_budget(),
    )
    .await
    .map_err(Error::Execution)?;

    if let Err(error) = input.seal_read_ceiling(MAX_CAPTURE_SOURCE_BYTES as u64) {
        input.close_and_wait().await.map_err(execution)?;
        return Err(Error::Execution(error));
    }
    let mut command = tokio::process::Command::new(&context.tools.renderer);
    command
        .current_dir(dir.path())
        .arg(mode)
        .arg(input.url())
        .args([
            request.layout.video_index.to_string(),
            request.first.to_string(),
            request.last.to_string(),
            request.first_pts.to_string(),
            request.end_pts.to_string(),
        ])
        .args(request.layout.roi.map(|n| n.to_string()))
        .arg(CAPTURE_OUTPUT);
    let output = async {
        let manifest = Descriptors::default().prepare(
            &mut command,
            ChildStdios {
                stdin: ChildStdio::Null,
                stdout: ChildStdio::File(&stdout),
                stderr: ChildStdio::Piped,
            },
        )?;
        source_lifetime::mediated(bounded::output_command_bundle_env_to_file(
            &mut command,
            super::FEL_WINDOW_STAGE_DEADLINE,
            DIAGNOSTIC_BYTES,
            max_bytes,
            ChildWork::background("Dolby Vision M7 renderer observation"),
            &context.tools.renderer_env,
            stdout,
            Some(manifest),
        ))
        .await
    }
    .await;
    // The bounded owner reaps the child before this physical source join,
    // including launch, preemption, timeout and file-size-limit failures.
    let source_read_failed = input.source_read_failed();
    input.close_and_wait().await.map_err(execution)?;
    let output = output.map_err(|error| {
        if error.kind() == io::ErrorKind::Interrupted {
            Error::Cancelled
        } else {
            execution(error)
        }
    })?;
    if source_read_failed {
        return Err(Error::Execution("held capture input read failed".into()));
    }
    if !output.status.success() {
        let diagnostic = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return if output.status.code() == Some(3) {
            Err(Error::Content(diagnostic))
        } else {
            Err(Error::Execution(format!("{} {diagnostic}", output.status)))
        };
    }
    Ok(output.status)
}

fn capture_bytes(shape: WindowShape, record_bytes: usize) -> usize {
    CAPTURE_HEADER_AND_TRAILER
        + shape.frames * (FRAME_PREFIX_BYTES + MAX_NAL_BYTES + shape.sites_per_frame * record_bytes)
}

fn score_window(
    mut aggregator: Aggregator,
    window: EvaluationWindow,
    captures: CaptureFiles,
    table: &ServedCurveTable,
    cancel: &CancellationToken,
) -> Result<(Aggregator, EvaluationWindow), Error> {
    let open = |dir: &TempDir| {
        plurx_core::fs_secure::open_read_nofollow_blocking(&dir.path().join(CAPTURE_OUTPUT))
            .map(BufReader::new)
            .map_err(execution)
    };
    let mut source = CaptureReader::new(
        open(&captures.source_dir)?,
        CapturePlan {
            layout: window.layout,
            frames: &window.source_frames,
        },
        captures.source_status,
    )?;
    let mut candidate = CaptureReader::new(
        open(&captures.candidate_dir)?,
        CapturePlan {
            layout: window.authored.layout,
            frames: &window.authored.frames,
        },
        captures.candidate_status,
    )?;
    for ((authoring, whole), seek) in window
        .authored
        .bindings
        .iter()
        .zip(&window.whole.frames)
        .zip(&window.seek.frames)
    {
        check_cancel(cancel)?;
        let source = source.next_frame()?.ok_or(CaptureError::Completion)?;
        let candidate = candidate.next_frame()?.ok_or(CaptureError::Completion)?;
        let curves = table
            .curves_at(source.evidence().pts)
            .ok_or(Error::Unsupported("fitted curves on every evaluated frame"))?;
        let scores = score_paired_frame(
            &source,
            &candidate,
            authoring,
            &whole.evidence(),
            &seek.evidence(),
            curves,
            || cancel.is_cancelled(),
        )?;
        aggregator.push_frame(&scores, || cancel.is_cancelled())?;
    }
    source.finish()?;
    candidate.finish()?;
    Ok((aggregator, window))
}

fn bind_custody(
    context: &TitleContext<'_>,
    input: &ComparisonInput,
    shapes: &[WindowShape],
    scorer_sha256: Sha256Digest,
) -> Result<M7Binding, Error> {
    let windows = input
        .windows
        .iter()
        .zip(shapes)
        .map(|(window, shape)| {
            let authored = &window.authored;
            let (first, last, end_pts) = endpoints(&window.source_frames, window.layout)?;
            let source_census_sha256 = census_digest(window.layout, &window.source_frames);
            let candidate_census_sha256 = census_digest(authored.layout, &authored.frames);
            let mut crop = Sha256::new();
            crop.update(b"plurx/m7/authoring-crop/v1\0");
            crop.update(source_census_sha256);
            crop.update(candidate_census_sha256);
            for binding in &authored.bindings {
                crop.update(binding.crop_first_source_ordinal.to_le_bytes());
                crop.update(binding.crop_frames.to_le_bytes());
            }
            Ok(WindowBinding {
                first_ordinal: first.ordinal,
                last_ordinal: last.ordinal,
                first_pts: first.pts,
                end_pts,
                time_base: window.layout.time_base,
                frames: shape.frames,
                sites_per_frame: shape.sites_per_frame,
                source_census_sha256,
                candidate_census_sha256,
                authored_crop_sha256: crop.finalize().into(),
                encoder_sha256: authored.encoder_sha256,
                encoded_sha256: authored.encoded_sha256,
                authored_sha256: authored.authored_sha256,
                authoring_receipt_sha256: authored.authoring_receipt_sha256,
                whole_publication_sha256: window.whole.publication_sha256,
                whole_readback_sha256: window.whole.readback_sha256,
                seek_publication_sha256: window.seek.publication_sha256,
                seek_readback_sha256: window.seek.readback_sha256,
                source_vcl_sha256: ordered_digests(
                    b"plurx/m7/source-vcl/v1\0",
                    window.source_frames.iter().map(|frame| frame.bl_vcl_sha256),
                ),
                whole_rpu_sha256: ordered_digests(
                    b"plurx/m7/whole-rpu/v1\0",
                    window.whole.frames.iter().map(|frame| frame.rpu_sha256),
                ),
                seek_rpu_sha256: ordered_digests(
                    b"plurx/m7/seek-rpu/v1\0",
                    window.seek.frames.iter().map(|frame| frame.rpu_sha256),
                ),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(M7Binding {
        file_id: context.file.id,
        source_size: context.file.size,
        source_mtime: context.file.mtime,
        probe_revision: context.demand.probe_revision,
        source_sha256: input.title.content_sha256,
        held_source_version: input.title.source.object_version().to_owned(),
        probe_sha256: input.title.probe_sha256,
        scanner_digest: context.tools.title_scanner_digest(),
        renderer_digest: context.tools.backend_digest(),
        scorer_sha256,
        model_version: MODEL_VERSION.to_owned(),
        scoring_revision: SCORING_REVISION,
        aggregation_revision: AGGREGATION_REVISION,
        protocol_sha256: input.frozen.protocol_sha256,
        recipe_sha256: input.frozen.recipe_sha256,
        fit_method: FIT_VERSION.to_owned(),
        fit_table_sha256: input.frozen.fit_table_sha256,
        calibration: input.frozen.calibration.clone(),
        margin: input.frozen.margin.value(),
        windows,
    })
}

fn ordered_digests(domain: &[u8], digests: impl Iterator<Item = Sha256Digest>) -> Sha256Digest {
    let mut hash = Sha256::new();
    hash.update(domain);
    for digest in digests {
        hash.update(digest);
    }
    hash.finalize().into()
}

fn census_digest(layout: CaptureLayout, frames: &[FrameEvidence]) -> Sha256Digest {
    let mut hash = Sha256::new();
    hash.update(match layout.role {
        CaptureRole::SourceFel => b"plurx/m7/source-census/v1\0".as_slice(),
        CaptureRole::AuthoredCandidate => b"plurx/m7/candidate-census/v1\0".as_slice(),
    });
    hash.update(layout.video_index.to_le_bytes());
    hash.update(layout.time_base.numerator().to_le_bytes());
    hash.update(layout.time_base.denominator().to_le_bytes());
    for field in layout.raster.into_iter().chain(layout.roi) {
        hash.update(field.to_le_bytes());
    }
    for frame in frames {
        hash.update(frame.ordinal.to_le_bytes());
        hash.update(frame.pts.to_le_bytes());
        hash.update(frame.duration.to_le_bytes());
        hash.update(frame.access_unit_sha256);
        hash.update(frame.rpu_sha256);
        hash.update(frame.bl_vcl_sha256);
    }
    hash.finalize().into()
}

fn verify_candidate(
    held: &SourceFence,
    expected: Sha256Digest,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let metadata = held.handle.metadata().map_err(execution)?;
    if !metadata.is_file() || metadata.len() > MAX_CAPTURE_SOURCE_BYTES as u64 {
        return Err(Error::Unsupported("bounded authored clip"));
    }
    let mut hash = Sha256::new();
    let mut offset = 0;
    let mut buffer = [0; 64 * 1024];
    while offset < metadata.len() {
        check_cancel(cancel)?;
        let read = held
            .handle
            .read_at(&mut buffer, offset)
            .map_err(execution)?;
        if read == 0 {
            return Err(Error::Authority("authored clip length"));
        }
        hash.update(&buffer[..read]);
        offset += read as u64;
    }
    let actual: Sha256Digest = hash.finalize().into();
    if offset != metadata.len() || actual != expected || !held.unchanged() {
        return Err(Error::Authority("held authored clip bytes"));
    }
    Ok(())
}

async fn scorer_identity(cancel: &CancellationToken) -> Result<Sha256Digest, Error> {
    let cancel = cancel.clone();
    off_runtime(move || scorer_binary_sha256(&cancel)).await
}

fn scorer_binary_sha256(cancel: &CancellationToken) -> Result<Sha256Digest, Error> {
    #[cfg(target_os = "linux")]
    let executable = File::open("/proc/self/exe").map_err(execution)?;
    #[cfg(target_os = "macos")]
    let executable = File::open(std::env::current_exe().map_err(execution)?).map_err(execution)?;
    let mut hasher = Sha256::new();
    let mut input = BufReader::new(executable);
    let mut buffer = [0; 64 * 1024];
    loop {
        check_cancel(cancel)?;
        let count = input.read(&mut buffer).map_err(execution)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().into())
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    Sha256::digest(bytes).into()
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), Error> {
    if cancel.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn execution(error: impl std::fmt::Display) -> Error {
    Error::Execution(error.to_string())
}

async fn off_runtime<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(work).await.map_err(execution)?
}
