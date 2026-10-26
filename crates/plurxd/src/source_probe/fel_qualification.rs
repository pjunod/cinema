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

    /// The `index`th of `count` windows, at (index+1)/(count+1) of the title,
    /// snapped to the picture grid: its first and end source timestamps.
    pub(crate) fn window(
        &self,
        duration_ms: i64,
        index: usize,
        count: usize,
    ) -> Result<(Instant, Instant), String> {
        if duration_ms <= 0 || index >= count {
            return Err("title duration is unknown".into());
        }
        let (num, den) = (i128::from(self.rate.0), i128::from(self.rate.1));
        let at_ms = i128::from(duration_ms) * (index as i128 + 1) / (count as i128 + 1);
        let first = at_ms * num / (den * 1000);
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
    args.extend([
        "-r".to_owned(),
        format!("{}/{}", clock.rate.0, clock.rate.1),
    ]);
    args.extend(
        [
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
    /// Foreground work, a job cancel or shutdown: every stage kills and reaps
    /// its children and returns.
    pub(crate) cancel: tokio_util::sync::CancellationToken,
}

impl Sample<'_> {
    pub(crate) fn environment(
        tools: &crate::vodencode::dv_runtime::ToolSet,
        decode: RendererDecode,
    ) -> Vec<(String, std::ffi::OsString)> {
        let mut environment = tools.renderer_env.clone();
        if decode == RendererDecode::Gpu {
            crate::vodencode::dv_runtime::gpu_decode_environment(
                &mut environment,
                &plurx_core::transcode::vaapi_device(),
                &crate::ffmpeg::ffmpeg_bin(),
            );
        }
        environment
    }
}

/// The pooled active-picture deltas of every window, and each window's score.
pub(crate) struct Measurement {
    pub(crate) windows: Vec<WindowScore>,
    pub(crate) served: Vec<f64>,
    pub(crate) dropped: Vec<f64>,
}

pub(crate) async fn measure(sample: &Sample<'_>, count: usize) -> Result<Measurement, String> {
    let mut measurement = Measurement {
        windows: Vec::with_capacity(count),
        served: Vec::new(),
        dropped: Vec::new(),
    };
    for index in 0..count {
        let window = sample.clock.window(sample.duration_ms, index, count)?;
        let scratch = tempfile::Builder::new()
            .prefix("dv-fel-qualify-")
            .tempdir_in(sample.runtime_cache)
            .map_err(|e| e.to_string())?;
        let reference = render(sample, window, "fel", RGB48_EXPORT, scratch.path(), None).await?;
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
        let region = q::active_region(&reference[0]).ok_or("the window's reference is all dark")?;
        let (mut window_served, mut window_dropped) = (Vec::new(), Vec::new());
        for ((reference, served), dropped) in reference.iter().zip(&served).zip(&dropped) {
            if !q::active_deltas(reference, served, region, &mut window_served)
                || !q::active_deltas(reference, dropped, region, &mut window_dropped)
            {
                return Err("window pictures differ in shape".into());
            }
        }
        measurement.served.extend_from_slice(&window_served);
        measurement.dropped.extend_from_slice(&window_dropped);
        let samples = window_served.len() as u64;
        let (served_mean, served_p99) =
            q::mean_p99(&mut window_served).ok_or("no served samples")?;
        let (dropped_mean, dropped_p99) =
            q::mean_p99(&mut window_dropped).ok_or("no EL-dropped samples")?;
        let round = |v: f64| (v * 10_000.0).round() / 10_000.0;
        measurement.windows.push(WindowScore {
            start_ms: window.0 .0 * 1000 / i64::from(window.0 .1),
            served_mean: round(served_mean),
            served_p99: round(served_p99),
            dropped_mean: round(dropped_mean),
            dropped_p99: round(dropped_p99),
            samples,
        });
    }
    Ok(measurement)
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
        .stdout(Stdio::null())
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
    let outcome = supervised(&sample.cancel, async {
        let lattices = match consumer.stdout.take() {
            Some(stdout) => lattices(stdout, sample.clock.raster).await?,
            None => Vec::new(),
        };
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
        Ok(lattices)
    })
    .await;
    if outcome.is_err() {
        reap(&mut helper).await;
        reap(&mut consumer).await;
    }
    outcome
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
pub(super) enum Qualification {
    /// Not a Profile 7 FEL title, the route is off, this node cannot measure
    /// it, or its verdict is current.
    NotDue,
    /// The node's reconstruction slot or the GPU is busy; the verdict stays
    /// missing and the next play asks again.
    Busy,
    Stored(q::FelVerdict),
}

/// Measure and store the title's verdict when it is due on this node.
pub(super) async fn qualify_if_due(
    state: &crate::state::AppState,
    file: &plurx_core::domain::MediaFile,
    document: &str,
    tools: &crate::vodencode::dv_runtime::ToolSet,
    cancel: &tokio_util::sync::CancellationToken,
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
    let identity = q::identity(
        &tools.scanner_digest(),
        plurx_core::transcode::RECONSTRUCTED_P81_RECIPE,
    );
    if matches!(
        q::verdict_state(Some(document), file.size, file.mtime, &identity),
        q::VerdictState::Current(_)
    ) {
        return Ok(Qualification::NotDue);
    }
    let Ok((_slot, _permit, decode)) = state.transcode.admit_fel_qualification().await else {
        return Ok(Qualification::Busy);
    };
    let document: serde_json::Value = serde_json::from_str(document).map_err(|e| e.to_string())?;
    let source = crate::fragment_index_cluster::open_source_playback_fence(file, None).await?;
    let sample = Sample {
        source: &source.handle,
        clock: Clock::from_document(&document)?,
        duration_ms: file.duration_ms.ok_or("title duration is unknown")?,
        video_kbps: plurx_core::transcode::reconstructed_p81_video_kbps(file),
        renderer: &tools.renderer,
        renderer_env: Sample::environment(tools, decode),
        ffmpeg: crate::ffmpeg::ffmpeg_bin(),
        runtime_cache: &state.runtime_cache_dir,
        cancel: cancel.clone(),
    };
    let started = std::time::Instant::now();
    let mut measured = measure(&sample, q::WINDOWS).await?;
    if !tools.is_current().await || !source.unchanged() || source.reopen(file).await.is_err() {
        return Err("the renderer or source changed during FEL qualification".into());
    }
    let verdict = q::FelVerdict::from_windows(
        identity,
        file.size,
        file.mtime,
        super::now_ms(),
        measured.windows,
        &mut measured.served,
        &mut measured.dropped,
    )
    .ok_or("FEL qualification produced no samples")?;
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
        let (start, end) = clock.window(duration_ms, 0, 8).expect("window");
        // 800 s in: picture 19180 of a 23.976 fps title.
        let exact = |(n, d): Instant, frame: i64| {
            i128::from(n) * 24000 == i128::from(frame * 1001) * i128::from(d)
        };
        assert!(exact(start, 19180), "{start:?}");
        assert!(exact(end, 19180 + 48), "{end:?}");
        let last = clock.window(duration_ms, 7, 8).expect("last");
        assert!(last.1 .0 as f64 / f64::from(last.1 .1) < duration_ms as f64 / 1000.0);
    }

    #[test]
    fn window_timestamps_carry_the_held_source_origin() {
        let mut document = document();
        document["streams"][0]["time_base"] = "1/90000".into();
        document["streams"][0]["start_pts"] = 4500.into();
        let clock = Clock::from_document(&document).expect("clock");
        assert_eq!(clock.origin, (1, 20));
        let (start, _) = clock.window(48_000, 0, 1).expect("window");
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

    /// The production measurement on one real title, without a store:
    /// `PLURX_FEL_QUALIFY_SOURCE=<P7 FEL file>` (optionally
    /// `PLURX_FEL_QUALIFY_WINDOWS`, `PLURX_FEL_QUALIFY_SCRATCH`). Prints the
    /// verdict the title job would store.
    #[tokio::test]
    #[ignore = "physical: the deployed processing helper, a radeonsi VA-API device and a P7 FEL title"]
    async fn physical_fel_qualification_of_one_title() {
        let path = std::env::var("PLURX_FEL_QUALIFY_SOURCE").expect("PLURX_FEL_QUALIFY_SOURCE");
        let tools = crate::vodencode::dv_runtime::ToolSet::capture()
            .await
            .expect("helper identity")
            .expect("deployed helper");
        let probe = plurx_core::process::output_job_owned(
            tokio::process::Command::new(crate::ffmpeg::ffprobe_bin()).args([
                "-v",
                "error",
                "-show_streams",
                "-show_format",
                "-of",
                "json",
                &path,
            ]),
            ChildWork::background("FEL qualification control probe"),
        )
        .await
        .expect("ffprobe");
        let document: serde_json::Value =
            serde_json::from_slice(&probe.stdout).expect("probe JSON");
        let format = &document["format"];
        let number = |key: &str| format[key].as_str().and_then(|v| v.parse::<f64>().ok());
        let source = std::fs::File::open(&path).expect("source");
        let metadata = source.metadata().expect("metadata");
        let video_kbps = number("bit_rate").map_or(
            plurx_core::transcode::RECONSTRUCTED_P81_MAX_VIDEO_KBPS,
            |bps| {
                ((bps / 1000.0) as u32).clamp(
                    1_000,
                    plurx_core::transcode::RECONSTRUCTED_P81_MAX_VIDEO_KBPS,
                )
            },
        );
        let decode = if crate::ffmpeg::has_hevc_main10_vaapi_decode().await {
            RendererDecode::Gpu
        } else {
            RendererDecode::Software
        };
        let scratch = std::env::var("PLURX_FEL_QUALIFY_SCRATCH")
            .map_or_else(|_| std::env::temp_dir(), std::path::PathBuf::from);
        let sample = Sample {
            source: &source,
            clock: Clock::from_document(&document).expect("clock"),
            duration_ms: (number("duration").expect("duration") * 1000.0) as i64,
            video_kbps,
            renderer: &tools.renderer,
            renderer_env: Sample::environment(&tools, decode),
            ffmpeg: crate::ffmpeg::ffmpeg_bin(),
            runtime_cache: &scratch,
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let windows = std::env::var("PLURX_FEL_QUALIFY_WINDOWS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(q::WINDOWS);
        let started = std::time::Instant::now();
        let mut measured = measure(&sample, windows).await.expect("measurement");
        let identity = q::identity(
            &tools.scanner_digest(),
            plurx_core::transcode::RECONSTRUCTED_P81_RECIPE,
        );
        use std::os::unix::fs::MetadataExt;
        let verdict = q::FelVerdict::from_windows(
            identity,
            i64::try_from(metadata.len()).expect("size"),
            metadata.mtime(),
            0,
            measured.windows,
            &mut measured.served,
            &mut measured.dropped,
        )
        .expect("verdict");
        println!(
            "FEL qualification: {} decode={decode:?} video_kbps={video_kbps} seconds={} verdict={}",
            path,
            started.elapsed().as_secs(),
            serde_json::to_string(&verdict).expect("JSON")
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
}
