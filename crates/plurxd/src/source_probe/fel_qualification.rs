//! Per-title qualification of the reconstructed Profile 8.1, run inside the
//! title's Dolby Vision scan job (`title_work`), before its aggregate is
//! published. The job is already the once-per-title GPU/CPU analysis of every
//! Dolby Vision title, is listed in Activity with its node and a Cancel, and
//! is rerun by rotating its demand; the qualification adds no job type,
//! thread or schedule of its own (`docs/streaming/FEL-P81-RECONSTRUCTION.md`).
//!
//! Each sample window is rendered by the deployed helper three ways: the FEL
//! master (the reference) and the EL-dropped picture, both as 16-bit R'G'B',
//! and the composed 10-bit picture through the route's own VA-API encode,
//! decoded back. Every child is a background job-owned process (listed in
//! Activity Processes, stopped with the job).

use crate::dv_segment::RendererDecode;
use plurx_core::process::ChildWork;
use plurx_core::transcode::dv_fel_qualification::{self as q, Lattice, WindowScore};
use std::io::{Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

const RGB48_EXPORT: &str = "pq-bilinear-rgb48-v1";
const YUV420P10_EXPORT: &str = "pq-bilinear-yuv420p10-v1";
/// One render, encode or decode of one 2 s window. A 4K FEL window renders in
/// well under 30 s on the measured worker; a stuck child must not hold the
/// node's reconstruction slot for the job's whole lease.
const STAGE_DEADLINE: Duration = Duration::from_secs(240);
/// The helper's bounded seek preroll, as in the streaming route.
const MAX_PREROLL: u16 = 512;
/// CPU threads one window stage's FFmpeg child runs, debited with the
/// renderer's: the main/demux-mux thread, one decoder thread (`-threads 1`)
/// and one filter thread (`-filter_threads 1`), which every consumer argv
/// pins. Stages run one at a time, so this is the qualification's whole
/// FFmpeg share.
pub(crate) const CONSUMER_THREADS: usize = 3;
/// The helper's trace (decoder report and per-picture rows) is a few
/// kilobytes per window; read at most this much of it.
const TRACE_LIMIT: u64 = 1 << 20;

/// An exact source timestamp `numerator / denominator` seconds.
pub(crate) type Instant = (i64, u32);

fn gcd(a: i128, b: i128) -> i128 {
    if b == 0 {
        a.abs()
    } else {
        gcd(b, a % b)
    }
}

fn reduced(numerator: i128, denominator: i128) -> Result<Instant, String> {
    let divisor = gcd(numerator, denominator).max(1);
    let (numerator, denominator) = (numerator / divisor, denominator / divisor);
    Ok((
        i64::try_from(numerator).map_err(|_| "window timestamp overflow")?,
        u32::try_from(denominator)
            .ok()
            .filter(|d| *d > 0 && *d <= i32::MAX as u32)
            .ok_or("window timestamp denominator exceeds the helper protocol")?,
    ))
}

fn parse_ratio(value: Option<&serde_json::Value>) -> Option<(u32, u32)> {
    let (numerator, denominator) = value?.as_str()?.split_once('/')?;
    let ratio = (numerator.parse().ok()?, denominator.parse().ok()?);
    (ratio.0 > 0 && ratio.1 > 0).then_some(ratio)
}

/// The selected video stream's raster, cadence and exact demux origin, from
/// the title's held probe document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Clock {
    pub(crate) video_index: u8,
    pub(crate) raster: (u32, u32),
    pub(crate) rate: (u32, u32),
    pub(crate) origin: Instant,
}

impl Clock {
    pub(crate) fn from_document(document: &serde_json::Value) -> Result<Self, String> {
        let video_index = super::dv_title::selected_video_index(document)
            .ok_or("unknown selected video stream")?;
        let stream = document
            .get("streams")
            .and_then(serde_json::Value::as_array)
            .and_then(|streams| {
                streams.iter().find(|stream| {
                    stream.get("index").and_then(serde_json::Value::as_u64)
                        == Some(u64::from(video_index))
                })
            })
            .ok_or("selected video stream is missing")?;
        let dimension = |key: &str| {
            stream
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .filter(|v| *v >= 64 && v % 2 == 0)
        };
        let raster = dimension("width")
            .zip(dimension("height"))
            .ok_or("selected video raster is unknown")?;
        let rate =
            parse_ratio(stream.get("r_frame_rate")).ok_or("selected video cadence is unknown")?;
        let tick =
            parse_ratio(stream.get("time_base")).ok_or("selected video time base is unknown")?;
        let start = stream
            .get("start_pts")
            .and_then(serde_json::Value::as_i64)
            .filter(|ticks| *ticks >= 0)
            .ok_or("selected video source origin is unobserved")?;
        let origin = reduced(i128::from(start) * i128::from(tick.0), i128::from(tick.1))?;
        Ok(Self {
            video_index,
            raster,
            rate,
            origin,
        })
    }

    /// The window starting at `at_ms` into the title, snapped to the picture
    /// grid: its first and end source timestamps.
    pub(crate) fn window_at(&self, at_ms: i64) -> Result<(Instant, Instant), String> {
        if at_ms < 0 {
            return Err("window position is negative".into());
        }
        let (num, den) = (i128::from(self.rate.0), i128::from(self.rate.1));
        let first = i128::from(at_ms) * num / (den * 1000);
        let point = |frame: i128| {
            let (on, od) = (i128::from(self.origin.0), i128::from(self.origin.1));
            reduced(on * num + frame * den * od, od * num)
        };
        Ok((point(first)?, point(first + i128::from(q::WINDOW_FRAMES))?))
    }
}

/// The helper's argv for one bounded window, in the streaming route's
/// protocol: source fd 3, evidence directory fd 4, NUT output fd 5.
pub(crate) fn helper_args(
    clock: &Clock,
    window: (Instant, Instant),
    mode: &str,
    export: &str,
) -> Vec<String> {
    let fd = |n: u8| format!("/proc/self/fd/{n}");
    let ((start, sd), (end, ed)) = window;
    let (origin, od) = clock.origin;
    vec![
        fd(3),
        fd(4),
        clock.video_index.to_string(),
        q::WINDOW_FRAMES.to_string(),
        clock.raster.0.to_string(),
        clock.raster.1.to_string(),
        "0".into(),
        "0".into(),
        "0".into(),
        "bt2020-pq-master-clip".into(),
        fd(5),
        format!("{start}/{sd}"),
        format!("{end}/{ed}"),
        MAX_PREROLL.to_string(),
        mode.into(),
        clock.raster.0.to_string(),
        clock.raster.1.to_string(),
        export.into(),
        "--held-source-origin".into(),
        format!("{origin}/{od}"),
    ]
}

/// The route's encode of a composed window: VA-API upload of the helper's
/// 10-bit PQ picture on the title's picture grid (as the route's own pipe
/// recipe reads it: `-r` on the input), and the reconstructed 8.1's encoder
/// arguments. Without `-r` the NUT's millisecond timestamps reach the encoder
/// unevenly and it drops and repeats pictures: Delta E 10.7 instead of 3.1 on
/// the same window.
pub(crate) fn encode_args(
    clock: &Clock,
    video_kbps: u32,
    output: &Path,
) -> Result<Vec<String>, String> {
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-v", "error"]
        .map(str::to_owned)
        .into();
    args.extend(plurx_core::transcode::Encoder::Vaapi.init_args());
    // `CONSUMER_THREADS`: one decoder thread, one filter thread.
    args.extend(["-filter_threads".to_owned(), "1".to_owned()]);
    args.extend([
        "-r".to_owned(),
        format!("{}/{}", clock.rate.0, clock.rate.1),
    ]);
    args.extend(
        [
            "-threads",
            "1",
            "-f",
            "nut",
            "-i",
            "pipe:0",
            "-map",
            "0:v:0",
            "-vf",
            "setparams=range=tv:color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc,\
             format=p010le,hwupload=extra_hw_frames=4",
        ]
        .map(str::to_owned),
    );
    args.extend(
        plurx_core::transcode::reconstructed_p81_window_encode_args(
            plurx_core::transcode::VaapiHdrPolicy::MesaRadeonsiVbaq,
            video_kbps,
            clock.raster.0,
            clock.raster.1,
            clock.rate,
            q::WINDOW_FRAMES,
        )
        .ok_or("no reconstructed 8.1 encode for this raster and cadence")?,
    );
    args.extend(["-f".to_owned(), "mp4".to_owned(), "-y".to_owned()]);
    args.push(output.to_string_lossy().into_owned());
    Ok(args)
}

/// Everything one title's measurement needs; nothing here is request data.
pub(crate) struct Sample<'a> {
    pub(crate) source: &'a std::fs::File,
    pub(crate) clock: Clock,
    pub(crate) duration_ms: i64,
    pub(crate) video_kbps: u32,
    pub(crate) renderer: &'a Path,
    pub(crate) renderer_env: Vec<(String, std::ffi::OsString)>,
    pub(crate) ffmpeg: String,
    pub(crate) runtime_cache: &'a Path,
    /// Where the renderer was admitted to decode; a window whose helper
    /// decoded in software under a GPU admission is refused (it ran threads
    /// nobody was debited), as in the streaming route.
    pub(crate) decode: RendererDecode,
    /// Foreground work, a job cancel or shutdown: every stage kills and reaps
    /// its children and returns.
    pub(crate) cancel: tokio_util::sync::CancellationToken,
}

/// The pooled active-picture deltas of every window, and each window's score.
pub(crate) struct Measurement {
    pub(crate) windows: Vec<WindowScore>,
    pub(crate) served: Vec<f64>,
    pub(crate) dropped: Vec<f64>,
}

/// Measure up to `count` lit windows: each slot's primary position, or its
/// alternate when the primary's reference is dark (a fade, a title card).
/// Fewer than `count` windows come back when both are dark.
pub(crate) async fn measure(sample: &Sample<'_>, count: usize) -> Result<Measurement, String> {
    let mut measurement = Measurement {
        windows: Vec::with_capacity(count),
        served: Vec::new(),
        dropped: Vec::new(),
    };
    for positions in q::sample_positions(sample.duration_ms, count) {
        for at_ms in positions {
            if let Some((score, served, dropped)) = measure_window(sample, at_ms).await? {
                measurement.served.extend_from_slice(&served);
                measurement.dropped.extend_from_slice(&dropped);
                measurement.windows.push(score);
                break;
            }
        }
    }
    Ok(measurement)
}

/// One window's score and active-picture deltas; `None` when its reference
/// has no lit picture to measure.
async fn measure_window(
    sample: &Sample<'_>,
    at_ms: i64,
) -> Result<Option<(WindowScore, Vec<f64>, Vec<f64>)>, String> {
    let window = sample.clock.window_at(at_ms)?;
    let scratch = tempfile::Builder::new()
        .prefix("dv-fel-qualify-")
        .tempdir_in(sample.runtime_cache)
        .map_err(|e| e.to_string())?;
    let reference = render(sample, window, "fel", RGB48_EXPORT, scratch.path(), None).await?;
    // The letterbox of the window's brightest picture: a fade-in's first
    // frame says nothing about it.
    let Some(region) = q::brightest(&reference).and_then(q::active_region) else {
        return Ok(None);
    };
    let dropped = render(
        sample,
        window,
        "base-rpu",
        RGB48_EXPORT,
        scratch.path(),
        None,
    )
    .await?;
    let encoded = scratch.path().join("served.mp4");
    render(
        sample,
        window,
        "fel",
        YUV420P10_EXPORT,
        scratch.path(),
        Some(&encoded),
    )
    .await?;
    let served = decode(sample, &encoded).await?;
    drop(scratch);
    let (mut window_served, mut window_dropped) = (Vec::new(), Vec::new());
    for ((reference, served), dropped) in reference.iter().zip(&served).zip(&dropped) {
        if !q::active_deltas(reference, served, region, &mut window_served)
            || !q::active_deltas(reference, dropped, region, &mut window_dropped)
        {
            return Err("window pictures differ in shape".into());
        }
    }
    let (served, dropped) = (window_served.clone(), window_dropped.clone());
    let samples = window_served.len() as u64;
    let (served_mean, served_p99) = q::mean_p99(&mut window_served).ok_or("no served samples")?;
    let (dropped_mean, dropped_p99) =
        q::mean_p99(&mut window_dropped).ok_or("no EL-dropped samples")?;
    let round = |v: f64| (v * 10_000.0).round() / 10_000.0;
    Ok(Some((
        WindowScore {
            start_ms: window.0 .0 * 1000 / i64::from(window.0 .1),
            served_mean: round(served_mean),
            served_p99: round(served_p99),
            dropped_mean: round(dropped_mean),
            dropped_p99: round(dropped_p99),
            samples,
        },
        served,
        dropped,
    )))
}

fn read_end_file(pipe: std::io::PipeReader) -> std::fs::File {
    std::os::fd::OwnedFd::from(pipe).into()
}

/// Render one window. With `encode_to`, the picture goes through the route's
/// encode into that file and nothing is returned; otherwise its R'G'B'
/// lattices come back.
async fn render(
    sample: &Sample<'_>,
    window: (Instant, Instant),
    mode: &str,
    export: &str,
    scratch: &Path,
    encode_to: Option<&Path>,
) -> Result<Vec<Lattice>, String> {
    let evidence = scratch.join(format!("render-{mode}-{export}"));
    std::fs::create_dir(&evidence).map_err(|e| e.to_string())?;
    let directory = plurx_core::fs_secure::open_directory_nofollow_blocking(&evidence)
        .map_err(|e| e.to_string())?;
    let (read, write) = std::io::pipe().map_err(|e| e.to_string())?;
    let write: std::fs::File = std::os::fd::OwnedFd::from(write).into();
    let mut source = sample.source;
    source.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let diagnostics = std::fs::File::create(scratch.join(format!("{mode}-{export}.log")))
        .map_err(|e| e.to_string())?;
    let mut helper = tokio::process::Command::new(sample.renderer);
    helper
        .args(helper_args(&sample.clock, window, mode, export))
        .envs(
            sample
                .renderer_env
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_os_str())),
        )
        .env("MESA_SHADER_CACHE_DIR", scratch)
        .env("MESA_SHADER_CACHE_MAX_SIZE", "64M")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(diagnostics))
        .kill_on_drop(true);
    crate::ffmpeg::inherit_raw_file_descriptors_with_cwd(
        &mut helper,
        &[
            (sample.source.as_raw_fd(), 3),
            (directory.as_raw_fd(), 4),
            (write.as_raw_fd(), 5),
        ],
        None,
    );
    let (mut helper, _helper_job) = plurx_core::process::spawn_job_owned(
        &mut helper,
        ChildWork::background("Dolby Vision FEL qualification render"),
    )
    .map_err(|e| e.to_string())?;
    drop(write);
    let mut consumer = tokio::process::Command::new(&sample.ffmpeg);
    match encode_to {
        Some(output) => {
            consumer.args(encode_args(&sample.clock, sample.video_kbps, output)?);
            consumer.stdout(Stdio::null());
        }
        None => {
            consumer.args(rgb48_args("pipe:0", true));
            consumer.stdout(Stdio::piped());
        }
    }
    consumer
        .stdin(Stdio::from(read_end_file(read)))
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let work = if encode_to.is_some() {
        "Dolby Vision FEL qualification encode"
    } else {
        "Dolby Vision FEL qualification sample"
    };
    let (mut consumer, _consumer_job) =
        plurx_core::process::spawn_job_owned(&mut consumer, ChildWork::background(work))
            .map_err(|e| e.to_string())?;
    let trace = helper.stdout.take().ok_or("the renderer has no trace")?;
    let outcome = supervised(&sample.cancel, async {
        let consume = async {
            match consumer.stdout.take() {
                Some(stdout) => lattices(stdout, sample.clock.raster).await,
                None => Ok(Vec::new()),
            }
        };
        let (lattices, trace) = tokio::join!(consume, read_trace(trace));
        let lattices = lattices?;
        let consumed = finish(&mut consumer, "the window's consumer").await;
        let rendered = helper.wait().await.map_err(|e| e.to_string())?;
        if !rendered.success() {
            let log = std::fs::read_to_string(scratch.join(format!("{mode}-{export}.log")))
                .unwrap_or_default();
            let tail = log
                .char_indices()
                .rev()
                .nth(400)
                .map_or(log.as_str(), |(at, _)| &log[at..]);
            return Err(format!(
                "the renderer refused the window ({mode}): {}",
                tail.trim()
            ));
        }
        consumed?;
        if sample.decode == RendererDecode::Gpu && decoded_in_software(&trace?)? {
            return Err(
                "the renderer decoded in software, but the qualification was admitted for \
                 GPU decode and holds no CPU for software decoders"
                    .into(),
            );
        }
        Ok(lattices)
    })
    .await;
    if outcome.is_err() {
        reap(&mut helper).await;
        reap(&mut consumer).await;
    }
    outcome
}

async fn read_trace(stdout: tokio::process::ChildStdout) -> Result<String, String> {
    let mut bytes = Vec::new();
    stdout
        .take(TRACE_LIMIT)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Whether the helper's decoder report says it opened its decoders without a
/// VA-API device (four software frame workers per layer).
fn decoded_in_software(trace: &str) -> Result<bool, String> {
    let report = trace
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|row| row.get("kind").and_then(serde_json::Value::as_str) == Some("decoder"))
        .ok_or("the renderer reported no decoder")?;
    Ok(crate::vodencode::dv_runtime::helper_decoder(&report)?.1)
}

/// Run one stage under the stage deadline and the qualification's cancel.
async fn supervised<T>(
    cancel: &tokio_util::sync::CancellationToken,
    work: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    tokio::select! {
        result = tokio::time::timeout(STAGE_DEADLINE, work) => {
            result.map_err(|_| "a FEL qualification stage exceeded its deadline".to_owned())?
        }
        () = cancel.cancelled() => Err("FEL qualification was preempted or cancelled".into()),
    }
}

/// Kill and reap a stage child that did not finish on its own.
async fn reap(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

async fn decode(sample: &Sample<'_>, encoded: &Path) -> Result<Vec<Lattice>, String> {
    let mut command = tokio::process::Command::new(&sample.ffmpeg);
    command
        .args(rgb48_args(&encoded.to_string_lossy(), false))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let (mut child, _job) = plurx_core::process::spawn_job_owned(
        &mut command,
        ChildWork::background("Dolby Vision FEL qualification sample"),
    )
    .map_err(|e| e.to_string())?;
    let outcome = supervised(&sample.cancel, async {
        let stdout = child.stdout.take().ok_or("decoder has no output")?;
        let lattices = lattices(stdout, sample.clock.raster).await?;
        finish(&mut child, "the served window's decoder").await?;
        Ok(lattices)
    })
    .await;
    if outcome.is_err() {
        reap(&mut child).await;
    }
    outcome
}

/// Raw 16-bit R'G'B' pictures of the input. The helper's own R'G'B' export is
/// already full-range rgb48; the encoded window is limited-range BT.2020
/// YCbCr and is expanded with the same matrix as the measurement tools.
fn rgb48_args(input: &str, nut: bool) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-v", "error"]
        .map(str::to_owned)
        .into();
    // `CONSUMER_THREADS`: one decoder thread, one filter thread.
    args.extend(["-filter_threads".to_owned(), "1".to_owned()]);
    args.extend(["-threads".to_owned(), "1".to_owned()]);
    if nut {
        args.extend(["-f".to_owned(), "nut".to_owned()]);
    }
    let filter = if nut {
        "format=rgb48le"
    } else {
        "scale=in_color_matrix=bt2020:in_range=tv:out_range=pc,format=rgb48le"
    };
    args.extend(
        [
            "-i",
            input,
            "-map",
            "0:v:0",
            "-frames:v",
            &q::WINDOW_FRAMES.to_string(),
            "-fps_mode",
            "passthrough",
            "-vf",
            filter,
            "-f",
            "rawvideo",
            "pipe:1",
        ]
        .map(str::to_owned),
    );
    args
}

async fn lattices(
    mut stdout: tokio::process::ChildStdout,
    raster: (u32, u32),
) -> Result<Vec<Lattice>, String> {
    let (width, height) = (raster.0 as usize, raster.1 as usize);
    let mut frame = vec![0_u8; width * height * 6];
    let mut out = Vec::with_capacity(q::WINDOW_FRAMES as usize);
    for _ in 0..q::WINDOW_FRAMES {
        stdout
            .read_exact(&mut frame)
            .await
            .map_err(|_| "the window ended early")?;
        out.push(Lattice::from_rgb48(&frame, width, height).ok_or("invalid window picture")?);
    }
    Ok(out)
}

async fn finish(child: &mut tokio::process::Child, what: &str) -> Result<(), String> {
    let mut diagnostic = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let mut bytes = Vec::new();
        let _ = (&mut stderr).take(4096).read_to_end(&mut bytes).await;
        diagnostic = String::from_utf8_lossy(&bytes).trim().to_owned();
    }
    let status = child.wait().await.map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed: {diagnostic}"))
    }
}

/// What the title job did about the FEL qualification.
#[derive(Debug)]
pub(crate) enum Qualification {
    /// Not a Profile 7 FEL title, the route is off, this node cannot measure
    /// it, or its verdict is current.
    NotDue,
    /// The node's reconstruction slot or the GPU is busy; the verdict stays
    /// missing and the next play asks again.
    Busy,
    Stored(q::FelVerdict),
}

/// The deployed renderer a qualification runs, and whether it is still the
/// one it started with (`None` skips that check, for fixtures).
pub(crate) struct Renderer<'a> {
    pub(crate) scanner: String,
    pub(crate) path: &'a Path,
    pub(crate) tools: Option<&'a crate::vodencode::dv_runtime::ToolSet>,
    pub(crate) env: Vec<(String, std::ffi::OsString)>,
    /// Where this node's renderer decodes (`processed_renderer_decode`).
    pub(crate) decode: RendererDecode,
}

/// The measurement a qualification runs over its sample: [`measure_title`] in
/// production.
pub(crate) type Measurer = for<'a> fn(
    &'a Sample<'a>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Measurement, String>> + Send + 'a>,
>;

pub(crate) fn measure_title<'a>(
    sample: &'a Sample<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Measurement, String>> + Send + 'a>> {
    Box::pin(measure(sample, q::WINDOWS))
}

/// Measure and store the title's verdict when it is due on this node.
pub(super) async fn qualify_if_due(
    state: &crate::state::AppState,
    file: &plurx_core::domain::MediaFile,
    document: &str,
    tools: &crate::vodencode::dv_runtime::ToolSet,
    admission: &crate::transcode::FragmentAdmission,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Qualification, String> {
    let renderer = Renderer {
        scanner: tools.backend_digest(),
        path: &tools.renderer,
        tools: Some(tools),
        env: tools.renderer_env.clone(),
        decode: crate::transcode::TranscodeManager::processed_renderer_decode(
            Path::new(&plurx_core::transcode::vaapi_device()).exists(),
            crate::ffmpeg::has_hevc_main10_vaapi_decode().await,
        ),
    };
    qualify_with(
        state,
        file,
        document,
        &renderer,
        admission,
        cancel,
        measure_title,
    )
    .await
}

/// Releases the title job's admission extension when the qualification ends.
struct Extension<'a>(&'a crate::transcode::FragmentAdmission);

impl Drop for Extension<'_> {
    fn drop(&mut self) {
        self.0.release_extension();
    }
}

/// [`qualify_if_due`] with its renderer and measurement given.
pub(crate) async fn qualify_with(
    state: &crate::state::AppState,
    file: &plurx_core::domain::MediaFile,
    document: &str,
    renderer: &Renderer<'_>,
    admission: &crate::transcode::FragmentAdmission,
    cancel: &tokio_util::sync::CancellationToken,
    measurer: Measurer,
) -> Result<Qualification, String> {
    let dv = file.dolby_vision;
    if dv.profile != Some(7)
        || dv.el_present != Some(true)
        || !state.transcode.fel_qualification_capable()
    {
        return Ok(Qualification::NotDue);
    }
    let settings = state
        .store
        .get_settings(&[
            plurx_core::store::keys::DV_HDR_PROCESSING,
            plurx_core::store::keys::DV_FEL_REENCODE,
        ])
        .await
        .map_err(|e| e.to_string())?;
    let on =
        |key: &str| plurx_core::store::stored_switch(settings.get(key).map(String::as_str), false);
    if !on(plurx_core::store::keys::DV_HDR_PROCESSING)
        || !on(plurx_core::store::keys::DV_FEL_REENCODE)
    {
        return Ok(Qualification::NotDue);
    }
    let identity = match state.transcode.fel_identity(&renderer.scanner).await {
        Ok(identity) => identity,
        Err(reason) => {
            tracing::info!(target: "plurxd::dv", file_id = file.id, %reason,
                "FEL qualification skipped: no encoder identity");
            return Ok(Qualification::NotDue);
        }
    };
    if matches!(
        q::verdict_state(Some(document), file.size, file.mtime, &identity),
        q::VerdictState::Current(_)
    ) {
        return Ok(Qualification::NotDue);
    }
    // The slot is preemptible by a live reconstructed 8.1 (it cancels
    // `cancel`); the GPU graph is a claim on this job's own admission, so the
    // job's idle check does not read its own step as foreground pressure.
    let (_slot, decode) = match state
        .transcode
        .admit_fel_qualification(admission, cancel.clone(), renderer.decode)
        .await
    {
        Ok(admitted) => admitted,
        Err(reason) => {
            tracing::debug!(target: "plurxd::dv", file_id = file.id, %reason,
                "FEL qualification deferred");
            return Ok(Qualification::Busy);
        }
    };
    let _extension = Extension(admission);
    let document: serde_json::Value = serde_json::from_str(document).map_err(|e| e.to_string())?;
    let source = crate::fragment_index_cluster::open_source_playback_fence(file, None).await?;
    let mut environment = renderer.env.clone();
    if decode == RendererDecode::Gpu {
        crate::vodencode::dv_runtime::gpu_decode_environment(
            &mut environment,
            &plurx_core::transcode::vaapi_device(),
            &crate::ffmpeg::ffmpeg_bin(),
        );
    }
    let sample = Sample {
        source: &source.handle,
        clock: Clock::from_document(&document)?,
        duration_ms: file.duration_ms.ok_or("title duration is unknown")?,
        video_kbps: plurx_core::transcode::reconstructed_p81_video_kbps(file),
        renderer: renderer.path,
        renderer_env: environment,
        ffmpeg: crate::ffmpeg::ffmpeg_bin(),
        runtime_cache: &state.runtime_cache_dir,
        decode,
        cancel: cancel.clone(),
    };
    let started = std::time::Instant::now();
    let mut measured = measurer(&sample).await?;
    let renderer_current = match renderer.tools {
        Some(tools) => tools.is_current().await,
        None => true,
    };
    if !renderer_current || !source.unchanged() || source.reopen(file).await.is_err() {
        return Err("the renderer or source changed during FEL qualification".into());
    }
    let now = super::now_ms();
    let verdict = if measured.windows.is_empty() {
        q::FelVerdict::unmeasurable(identity, file.size, file.mtime, now)
    } else {
        q::FelVerdict::from_windows(
            identity,
            file.size,
            file.mtime,
            now,
            measured.windows,
            &mut measured.served,
            &mut measured.dropped,
        )
        .ok_or("FEL qualification produced no samples")?
    };
    let stored = state
        .store
        .put_dv_fel_verdict(
            file.id,
            file.size,
            file.mtime,
            &serde_json::to_value(&verdict).map_err(|e| e.to_string())?,
        )
        .await
        .map_err(|e| e.to_string())?;
    if !stored {
        return Err("the title changed before its FEL verdict was stored".into());
    }
    tracing::info!(
        target: "plurxd::dv",
        file_id = file.id,
        admitted = verdict.admitted,
        windows = verdict.windows.len(),
        served_mean = verdict.served_mean,
        dropped_mean = verdict.dropped_mean,
        served_p99 = verdict.served_p99,
        dropped_p99 = verdict.dropped_p99,
        seconds = started.elapsed().as_secs(),
        "FEL qualification measured"
    );
    Ok(Qualification::Stored(verdict))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> serde_json::Value {
        serde_json::json!({"format":{"start_time":"0.000000"},"streams":[
            {"index":0,"codec_type":"video","width":3840,"height":2160,
             "r_frame_rate":"24000/1001","time_base":"1/1000","start_pts":0}]})
    }

    #[test]
    fn windows_are_spread_through_the_title_on_the_picture_grid() {
        let clock = Clock::from_document(&document()).expect("clock");
        assert_eq!(clock.rate, (24000, 1001));
        let duration_ms = 7_200_000;
        let positions = q::sample_positions(duration_ms, q::WINDOWS);
        assert_eq!(positions[0], [800_000, 1_200_000]);
        let (start, end) = clock.window_at(positions[0][0]).expect("window");
        // 800 s in: picture 19180 of a 23.976 fps title.
        let exact = |(n, d): Instant, frame: i64| {
            i128::from(n) * 24000 == i128::from(frame * 1001) * i128::from(d)
        };
        assert!(exact(start, 19180), "{start:?}");
        assert!(exact(end, 19180 + 48), "{end:?}");
        let last = clock.window_at(positions[7][1]).expect("last alternate");
        assert!(last.1 .0 as f64 / f64::from(last.1 .1) < duration_ms as f64 / 1000.0);
    }

    #[test]
    fn window_timestamps_carry_the_held_source_origin() {
        let mut document = document();
        document["streams"][0]["time_base"] = "1/90000".into();
        document["streams"][0]["start_pts"] = 4500.into();
        let clock = Clock::from_document(&document).expect("clock");
        assert_eq!(clock.origin, (1, 20));
        let (start, _) = clock.window_at(24_000).expect("window");
        // 0.05 s + 575 pictures of 1001/24000 s.
        let exact = 0.05 + 575.0 * 1001.0 / 24000.0;
        assert!((start.0 as f64 / f64::from(start.1) - exact).abs() < 1e-12);
    }

    #[test]
    fn an_unobserved_origin_or_cadence_is_refused() {
        let mut document = document();
        document["streams"][0]
            .as_object_mut()
            .expect("stream")
            .remove("start_pts");
        assert!(Clock::from_document(&document).is_err());
        let mut document = self::document();
        document["streams"][0]["r_frame_rate"] = "0/0".into();
        assert!(Clock::from_document(&document).is_err());
    }

    #[test]
    fn helper_argv_is_the_streaming_protocol_with_the_requested_export() {
        let clock = Clock::from_document(&document()).expect("clock");
        let args = helper_args(
            &clock,
            ((1001, 24000), (49049, 24000)),
            "base-rpu",
            RGB48_EXPORT,
        );
        assert_eq!(
            args,
            [
                "/proc/self/fd/3",
                "/proc/self/fd/4",
                "0",
                "48",
                "3840",
                "2160",
                "0",
                "0",
                "0",
                "bt2020-pq-master-clip",
                "/proc/self/fd/5",
                "1001/24000",
                "49049/24000",
                "512",
                "base-rpu",
                "3840",
                "2160",
                "pq-bilinear-rgb48-v1",
                "--held-source-origin",
                "0/1",
            ]
        );
    }

    #[test]
    fn the_window_encode_is_the_routes_recipe() {
        let clock = Clock::from_document(&document()).expect("clock");
        let args = encode_args(&clock, 60_000, Path::new("/x/served.mp4")).expect("args");
        let value = |flag: &str| {
            args.iter()
                .position(|arg| arg == flag)
                .map(|at| args[at + 1].as_str())
        };
        let input = args.iter().position(|arg| arg == "-i").expect("input");
        assert_eq!(value("-r"), Some("24000/1001"));
        assert!(args.iter().position(|arg| arg == "-r").expect("grid") < input);
        assert_eq!(value("-c:v"), Some("hevc_vaapi"));
        assert_eq!(value("-b:v"), Some("60000k"));
        assert_eq!(value("-maxrate"), Some("90000k"));
        assert_eq!(value("-bufsize"), Some("120000k"));
        assert_eq!(value("-compression_level"), Some("0"));
        assert_eq!(value("-bf"), Some("0"));
        assert_eq!(value("-g"), Some("48"));
        assert_eq!(value("-qmax"), None);
        assert_eq!(args.last().map(String::as_str), Some("/x/served.mp4"));
    }

    /// Every stage child runs the threads its charge (`CONSUMER_THREADS`)
    /// pays for: one decoder thread before its input, one filter thread.
    #[test]
    fn every_stage_child_is_capped_to_the_threads_it_is_charged() {
        let clock = Clock::from_document(&document()).expect("clock");
        let encode = encode_args(&clock, 60_000, Path::new("/x/served.mp4")).expect("args");
        for args in [
            encode,
            rgb48_args("pipe:0", true),
            rgb48_args("/x/served.mp4", false),
        ] {
            let input = args.iter().position(|arg| arg == "-i").expect("input");
            let at = |flag: &str| {
                args.windows(2)
                    .position(|pair| pair[0] == flag && pair[1] == "1")
                    .unwrap_or_else(|| panic!("{flag} 1 missing from {args:?}"))
            };
            assert!(at("-threads") < input, "{args:?}");
            assert!(at("-filter_threads") < input, "{args:?}");
            assert_eq!(
                args.iter().filter(|arg| *arg == "-threads").count(),
                1,
                "{args:?}"
            );
        }
        assert_eq!(CONSUMER_THREADS, 3);
    }

    /// The helper's own decoder report decides: a software decode under a GPU
    /// admission is refused, a missing report is an error.
    #[test]
    fn the_helpers_decoder_report_decides_a_software_decode() {
        let trace = |mode: &str| {
            format!(
                "{{\"kind\":\"frame\"}}\n{{\"kind\":\"decoder\",\"mode\":\"{mode}\",\"reason\":\"\"}}\n"
            )
        };
        assert_eq!(decoded_in_software(&trace("vaapi")), Ok(false));
        assert_eq!(decoded_in_software(&trace("software")), Ok(true));
        assert!(decoded_in_software("{\"kind\":\"frame\"}").is_err());
    }

    /// A fixture measurement: four 250 ms beats, each checking the
    /// qualification's cancellation (as every real stage does), then eight
    /// windows.
    fn slow_measurement<'a>(
        sample: &'a Sample<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Measurement, String>> + Send + 'a>>
    {
        Box::pin(async move {
            for _ in 0..4 {
                tokio::time::sleep(Duration::from_millis(250)).await;
                if sample.cancel.is_cancelled() {
                    return Err("FEL qualification was preempted or cancelled".into());
                }
            }
            let window = WindowScore {
                start_ms: 0,
                served_mean: 3.0,
                served_p99: 9.7,
                dropped_mean: 2.9,
                dropped_p99: 6.6,
                samples: 2,
            };
            Ok(Measurement {
                windows: vec![window; q::WINDOWS],
                served: vec![3.0; 16],
                dropped: vec![2.9; 16],
            })
        })
    }

    async fn qualification_fixture(
        dir: &Path,
    ) -> (
        crate::state::AppState,
        plurx_core::domain::MediaFile,
        String,
    ) {
        use plurx_core::domain::{
            DolbyVisionFacts, ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult,
        };
        use plurx_core::store::{SqliteStore, Store};
        use std::sync::Arc;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        // One title worker (1) plus its simulated GPU graph (4 + 3).
        // This fixture tests custody, not the host's physical CPU capacity.
        store
            .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "8")
            .await
            .expect("synthetic title and GPU graph budget");
        let caps = plurx_core::transcode::EncoderCaps {
            vaapi: true,
            vaapi_hdr_policy: Some(plurx_core::transcode::VaapiHdrPolicy::MesaRadeonsiVbaq),
            vaapi_driver: Some("Mesa Gallium driver for AMD fixture (radeonsi)".into()),
            ..Default::default()
        };
        let mut state = crate::state::AppState::new(
            "test".into(),
            Arc::clone(&store),
            crate::state::Dirs {
                artwork: dir.join("artwork"),
                transcode: dir.join("transcode"),
                cache: dir.join("cache"),
                subs: dir.join("subs"),
                runtime_cache: dir.join("runtime"),
                renditions: dir.join("renditions"),
            },
            "qualification-node".into(),
            caps.clone(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(16)),
        );
        state.transcode = Arc::new(
            crate::transcode::TranscodeManager::new(
                Arc::clone(&store),
                dir.join("runtime"),
                caps,
                plurx_core::transcode::Pipeline::Cpu,
            )
            .with_hdr10_passthrough_vaapi(true),
        );
        assert_eq!(state.transcode.software_budget().await, 8);
        std::fs::create_dir_all(dir.join("runtime")).expect("runtime");
        for key in [
            plurx_core::store::keys::DV_HDR_PROCESSING,
            plurx_core::store::keys::DV_FEL_REENCODE,
        ] {
            store.put_setting(key, "1").await.expect("setting");
        }
        let library = store
            .create_library(&NewLibrary {
                name: "fel".into(),
                kind: LibraryKind::Movies,
                paths: vec![dir.into()],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "fel".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let path = dir.join("fel.mkv");
        std::fs::write(&path, b"fixture source").expect("source");
        let (size, mtime) = super::super::snapshot(&path).await.expect("snapshot");
        let document = document().to_string();
        let id = store
            .upsert_file(
                item,
                path.to_str().expect("path"),
                size,
                mtime,
                &ProbeResult {
                    video_codec: Some("hevc".into()),
                    duration_ms: Some(7_200_000),
                    dolby_vision: DolbyVisionFacts {
                        profile: Some(7),
                        el_present: Some(true),
                        rpu_present: Some(true),
                        ..Default::default()
                    },
                    raw_json: Some(document.clone()),
                    ..Default::default()
                },
            )
            .await
            .expect("file");
        let file = store.get_file(id).await.expect("file").expect("file");
        (state, file, document)
    }

    /// B1: the qualification's own GPU graph is a claim on the title job's
    /// admission, so `run_admitted_probe` (which cancels the job's work as
    /// soon as the node is not idle) does not read it as foreground pressure:
    /// the measurement runs past several idle checks and its verdict is
    /// stored, keyed to this renderer, recipe and encoder.
    #[tokio::test]
    async fn a_qualification_inside_its_title_job_is_not_preempted_by_itself_and_stores_its_verdict(
    ) {
        let dir = crate::test_tempdir().expect("scratch");
        let (state, file, document) = qualification_fixture(dir.path()).await;
        let admission = std::sync::Arc::new(
            state
                .transcode
                .admit_dv_title()
                .await
                .expect("title admission"),
        );
        let renderer_path = dir.path().join("renderer");
        let renderer = Renderer {
            scanner: "a".repeat(64),
            path: &renderer_path,
            tools: None,
            env: Vec::new(),
            decode: RendererDecode::Gpu,
        };
        let token = tokio_util::sync::CancellationToken::new();
        let outcome = super::super::run_admitted_probe(
            &state.transcode,
            &admission,
            &token,
            &tokio_util::sync::CancellationToken::new(),
            None,
            qualify_with(
                &state,
                &file,
                &document,
                &renderer,
                &admission,
                &token,
                slow_measurement,
            ),
        )
        .await
        .expect("qualified");
        let Qualification::Stored(verdict) = outcome else {
            panic!("no verdict: {outcome:?}");
        };
        assert!(!token.is_cancelled(), "the job's own step is not pressure");
        assert!(!verdict.admitted);
        let stored = state
            .store
            .get_file_probe_json(file.id)
            .await
            .expect("probe")
            .expect("probe");
        let identity = state
            .transcode
            .fel_identity(&renderer.scanner)
            .await
            .expect("identity");
        assert_eq!(
            q::verdict_state(Some(&stored), file.size, file.mtime, &identity),
            q::VerdictState::Current(verdict)
        );
        // The step's capacity and the slot are released with it.
        assert!(!admission.has_extension());
        assert!(state.transcode.fragment_worker_idle(&admission));
        assert!(
            !state
                .transcode
                .live_reconstruction_facts()
                .await
                .held_by_qualification
        );
        // With a verdict current, the next attempt does not measure again.
        let again = qualify_with(
            &state,
            &file,
            &stored,
            &renderer,
            &admission,
            &token,
            slow_measurement,
        )
        .await
        .expect("not due");
        assert!(matches!(again, Qualification::NotDue), "{again:?}");
    }

    /// S1: a live reconstructed 8.1 preempts a running qualification through
    /// the slot: the qualification stops (no verdict), the play gets the slot.
    #[tokio::test]
    async fn a_live_play_preempts_a_running_qualification_and_gets_the_slot() {
        use plurx_core::transcode::dv_processing::DvDestination;
        let dir = crate::test_tempdir().expect("scratch");
        let (state, file, document) = qualification_fixture(dir.path()).await;
        let admission = state
            .transcode
            .admit_dv_title()
            .await
            .expect("title admission");
        let renderer_path = dir.path().join("renderer");
        let renderer = Renderer {
            scanner: "a".repeat(64),
            path: &renderer_path,
            tools: None,
            env: Vec::new(),
            decode: RendererDecode::Gpu,
        };
        let token = tokio_util::sync::CancellationToken::new();
        let qualification = qualify_with(
            &state,
            &file,
            &document,
            &renderer,
            &admission,
            &token,
            slow_measurement,
        );
        let play = async {
            // Let the qualification take the slot first.
            tokio::time::timeout(Duration::from_secs(5), async {
                while !state
                    .transcode
                    .live_reconstruction_facts()
                    .await
                    .held_by_qualification
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the qualification took the slot");
            state
                .transcode
                .acquire_reconstruction_slot(DvDestination::Profile81)
                .await
        };
        let (qualified, played) = tokio::join!(qualification, play);
        assert!(qualified.is_err(), "{qualified:?}");
        assert!(played.expect("the play takes the slot").is_some());
        assert!(token.is_cancelled());
        assert!(!admission.has_extension());
        let stored = state
            .store
            .get_file_probe_json(file.id)
            .await
            .expect("probe")
            .expect("probe");
        assert!(
            !stored.contains(q::PROBE_KEY),
            "a preempted qualification stores nothing"
        );
    }
}
