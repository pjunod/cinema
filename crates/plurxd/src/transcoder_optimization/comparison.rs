//! Fixed real-footage decode → scale/tonemap → encode comparisons.
//! Fixture preparation is offline; no request can supply a URL or input path.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use plurx_core::transcode::{Encoder, EncoderCaps, Pipeline, TranscodeOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const VERSION: u32 = 1;
const FRAME_COUNT: u32 = 240;
const REPEATS: usize = 3;
const OUTPUT_BITRATE: u32 = 4_000;
type ExecutableCache = std::collections::BTreeMap<(PathBuf, u64, u128), String>;
static EXECUTABLE_CACHE: std::sync::Mutex<ExecutableCache> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Case {
    pub id: String,
    pub label: String,
    pub source_codec: String,
    pub source_width: u32,
    pub source_height: u32,
    pub source_bit_depth: u32,
    pub source_hdr: Option<String>,
    pub target_height: u32,
    pub frame_start: u32,
    pub frame_count: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub output_codec: String,
    pub bitrate_kbps: u32,
    pub fixture_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    /// End-to-end process time, including startup and shutdown.
    pub seconds: f64,
    /// FFmpeg's own benchmark clock; absent when the process did not finish.
    pub pipeline_seconds: Option<f64>,
    /// End-to-end minus FFmpeg's clock: process launch/readout overhead.
    pub startup_seconds: Option<f64>,
    /// FFmpeg user + system process CPU; excludes driver/service CPU.
    pub cpu_seconds: Option<f64>,
    pub frames: u32,
    pub completed: bool,
    pub fallback: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResultRow {
    pub case_id: String,
    pub backend: String,
    pub device_id: Option<String>,
    pub pipeline: String,
    pub decoder: String,
    pub processing_selection: Option<String>,
    pub command: Vec<String>,
    pub median_seconds: Option<f64>,
    pub median_pipeline_seconds: Option<f64>,
    pub median_startup_seconds: Option<f64>,
    pub cpu_percent: Option<f64>,
    pub realtime_ratio: Option<f64>,
    pub relative_to_cpu: Option<f64>,
    pub completion: String,
    pub fallback: bool,
    pub warmup: Option<Run>,
    pub runs: Vec<Run>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FixtureSource {
    pub title: String,
    pub attribution: String,
    pub license: String,
    #[serde(rename(deserialize = "source_page"))]
    pub source_url: String,
    pub license_url: String,
}

pub(super) fn fixture_source() -> FixtureSource {
    static SOURCE: std::sync::OnceLock<FixtureSource> = std::sync::OnceLock::new();
    SOURCE
        .get_or_init(|| {
            serde_json::from_str(include_str!("fixtures/manifest.json"))
                .expect("committed fixture provenance manifest")
        })
        .clone()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicySnapshot {
    pub macos_video_processing_enabled: bool,
    pub macos_hevc_output_enabled: bool,
    pub hardware_decode_override: Option<String>,
    pub software_threads: [usize; 3],
    pub ffprobe_identity: Option<(PathBuf, u64, u128)>,
}
impl PolicySnapshot {
    fn flags(macos_video_processing_enabled: bool, macos_hevc_output_enabled: bool) -> Self {
        Self {
            macos_video_processing_enabled,
            macos_hevc_output_enabled,
            hardware_decode_override: std::env::var("PLURX_HWDECODE")
                .ok()
                .map(|value| value.trim().to_lowercase()),
            software_threads: super::benchmark_thread_budgets(),
            ffprobe_identity: executable_stamp(&crate::ffmpeg::ffprobe_bin()).ok(),
        }
    }
    pub(super) fn current(manager: &crate::transcode::TranscodeManager) -> Self {
        Self::flags(
            manager.macos_video_processing_enabled(),
            manager.macos_hevc_output_enabled(),
        )
    }
    pub(super) async fn saved(
        store: &std::sync::Arc<dyn plurx_core::store::Store>,
    ) -> anyhow::Result<Self> {
        let processing = store
            .get_setting(plurx_core::store::keys::MACOS_VIDEO_PROCESSING_ENABLED)
            .await?;
        let hevc = store
            .get_setting(plurx_core::store::keys::MACOS_HEVC_OUTPUT_ENABLED)
            .await?;
        Ok(Self::flags(
            plurx_core::store::stored_switch(processing.as_deref(), false),
            plurx_core::store::stored_switch(hevc.as_deref(), false),
        ))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Comparison {
    pub version: u32,
    pub fixture_source: FixtureSource,
    pub policy_snapshot: PolicySnapshot,
    pub status: String,
    pub error: Option<String>,
    pub executable_sha256: String,
    pub executable_path: PathBuf,
    pub executable_size: u64,
    pub executable_modified_ns: u128,
    pub hardware_identity: String,
    pub cases: Vec<Case>,
    pub results: Vec<ResultRow>,
}

impl Comparison {
    pub fn valid_for(&self, ffmpeg: &str, caps: &EncoderCaps, policy: &PolicySnapshot) -> bool {
        self.version == VERSION
            && &self.policy_snapshot == policy
            && self.status == "complete"
            && self.complete_baseline()
            && self.hardware_identity == hardware_identity(caps)
            && executable_stamp(ffmpeg).is_ok_and(|(path, size, modified)| {
                path == self.executable_path
                    && size == self.executable_size
                    && modified == self.executable_modified_ns
                    && EXECUTABLE_CACHE.lock().ok().is_some_and(|cache| {
                        cache
                            .get(&(path, size, modified))
                            .is_some_and(|digest| digest == &self.executable_sha256)
                    })
            })
            && self.cases == cases()
    }

    /// Startup/admin-run validation is the only path that reads executable
    /// bytes. Status requests only consult its cached digest and cheap stamp.
    pub(super) fn validate_executable(&self, ffmpeg: &str) -> bool {
        executable_stamp(ffmpeg).is_ok_and(|stamp| {
            executable_digest(&stamp).is_ok_and(|digest| digest == self.executable_sha256)
        })
    }

    fn complete_baseline(&self) -> bool {
        self.cases.len() == 3
            && self.cases.iter().all(|case| {
                let baseline: Vec<_> = self
                    .results
                    .iter()
                    .filter(|row| row.backend == "software" && row.case_id == case.id)
                    .collect();
                baseline.len() == 1 && trustworthy(baseline[0], case.frame_count)
            })
    }

    /// Hardware preference is ranked by the slowest representative workload.
    /// The CPU baseline and incomplete/fallback runs cannot win Auto.
    pub fn preferred_hardware(&self, caps: &EncoderCaps) -> Option<Encoder> {
        if self.status != "complete" || self.version != VERSION || !self.complete_baseline() {
            return None;
        }
        let mut best: Option<(Encoder, f64)> = None;
        for row in self.results.iter().filter(|row| row.backend != "software") {
            let Some(encoder) = super::ENCODERS
                .into_iter()
                .find(|e| e.family_name() == row.backend && caps.available(*e))
            else {
                continue;
            };
            let matching: Vec<_> = self
                .results
                .iter()
                .filter(|candidate| {
                    candidate.backend == row.backend && candidate.device_id == row.device_id
                })
                .collect();
            let score = self
                .cases
                .iter()
                .map(|case| {
                    let candidates: Vec<_> = matching
                        .iter()
                        .filter(|candidate| candidate.case_id == case.id)
                        .collect();
                    if candidates.len() != 1 {
                        return None;
                    }
                    let candidate = candidates[0];
                    trustworthy(candidate, case.frame_count)
                        .then_some(candidate.realtime_ratio)
                        .flatten()
                        .filter(|value| value.is_finite() && *value > 0.0)
                })
                .collect::<Option<Vec<_>>>()
                .and_then(|values| values.into_iter().reduce(f64::min));
            if let Some(score) = score {
                if best.is_none_or(|(_, previous)| score > previous) {
                    best = Some((encoder, score));
                }
            }
        }
        best.map(|(encoder, _)| encoder)
    }
}

fn trustworthy(row: &ResultRow, frames: u32) -> bool {
    let complete = |run: &Run| {
        run.completed
            && !run.fallback
            && run.frames == frames
            && run.seconds.is_finite()
            && run.seconds > 0.0
    };
    row.completion == "complete"
        && !row.fallback
        && row.runs.len() == REPEATS
        && row.warmup.as_ref().is_some_and(complete)
        && row.runs.iter().all(complete)
        && row
            .median_seconds
            .is_some_and(|seconds| seconds.is_finite() && seconds > 0.0)
}

fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn fixtures() -> [(&'static str, &'static str, &'static [u8]); 3] {
    [
        (
            "1080p-h264-sdr",
            "1080p H.264 SDR → 720p SDR",
            include_bytes!("fixtures/1080p-h264-sdr.mkv"),
        ),
        (
            "4k-hevc-sdr",
            "4K HEVC SDR → 1080p SDR",
            include_bytes!("fixtures/4k-hevc-sdr.mkv"),
        ),
        (
            "4k-hevc-hdr10",
            "4K HEVC HDR10 → 1080p SDR",
            include_bytes!("fixtures/4k-hevc-hdr10.mkv"),
        ),
    ]
}

fn cases() -> Vec<Case> {
    static CASES: std::sync::OnceLock<Vec<Case>> = std::sync::OnceLock::new();
    CASES
        .get_or_init(|| {
            fixtures()
                .into_iter()
                .enumerate()
                .map(|(index, (id, label, bytes))| Case {
                    id: id.into(),
                    label: label.into(),
                    source_codec: if index == 0 { "h264" } else { "hevc" }.into(),
                    source_width: if index == 0 { 1920 } else { 3840 },
                    source_height: if index == 0 { 1080 } else { 2160 },
                    source_bit_depth: if index == 0 { 8 } else { 10 },
                    source_hdr: (index == 2).then(|| "hdr10".into()),
                    target_height: if index == 0 { 720 } else { 1080 },
                    frame_start: 0,
                    frame_count: FRAME_COUNT,
                    fps_num: 24,
                    fps_den: 1,
                    output_codec: "h264".into(),
                    bitrate_kbps: OUTPUT_BITRATE,
                    fixture_sha256: hex_digest(bytes),
                })
                .collect()
        })
        .clone()
}
fn executable_digest(stamp: &(PathBuf, u64, u128)) -> anyhow::Result<String> {
    let mut cache = EXECUTABLE_CACHE
        .lock()
        .map_err(|_| anyhow!("FFmpeg identity cache unavailable"))?;
    if let Some(digest) = cache.get(stamp) {
        return Ok(digest.clone());
    }
    use std::io::Read;
    let mut file = std::fs::File::open(&stamp.0)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let digest = format!("{:x}", hasher.finalize());
    if cache.len() >= 16 {
        cache.clear();
    }
    cache.insert(stamp.clone(), digest.clone());
    Ok(digest)
}

fn executable_stamp(ffmpeg: &str) -> anyhow::Result<(PathBuf, u64, u128)> {
    let path = if Path::new(ffmpeg).is_absolute() || Path::new(ffmpeg).components().count() > 1 {
        PathBuf::from(ffmpeg)
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|directory| directory.join(ffmpeg))
            .find(|path| path.is_file())
            .ok_or_else(|| anyhow!("FFmpeg executable is unavailable"))?
    }
    .canonicalize()?;
    let meta = std::fs::metadata(&path)?;
    let modified = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    Ok((path, meta.len(), modified))
}

fn hardware_identity(caps: &EncoderCaps) -> String {
    // Inventory is not a driver receipt. Include the currently loaded kernel
    // and GPU module versions so a driver update invalidates old measurements.
    let mut bytes = serde_json::to_vec(&(
        caps.nvenc,
        caps.qsv,
        caps.vaapi,
        caps.videotoolbox,
        &caps.graphics_devices,
        &caps.device_bindings,
    ))
    .unwrap_or_default();
    for path in [
        "/proc/sys/kernel/osrelease",
        "/proc/driver/nvidia/version",
        "/sys/module/nvidia/version",
        "/sys/module/i915/version",
        "/sys/module/xe/version",
        "/System/Library/CoreServices/SystemVersion.plist",
    ] {
        if let Ok(value) = std::fs::read(path) {
            bytes.extend(value);
        }
    }
    for device in &caps.graphics_devices {
        let driver = PathBuf::from("/sys/bus/pci/devices")
            .join(&device.id)
            .join("driver");
        if let Ok(path) = driver.canonicalize() {
            bytes.extend(path.to_string_lossy().as_bytes());
        }
    }
    // User-space GPU drivers can change independently of the loaded kernel.
    // Metadata checks keep ordinary status reads free of subprocess launches.
    for directory in [
        "/usr/lib/x86_64-linux-gnu/dri",
        "/usr/lib/aarch64-linux-gnu/dri",
        "/usr/lib64/dri",
        "/usr/lib/dri",
    ] {
        for name in [
            "iHD_drv_video.so",
            "i965_drv_video.so",
            "radeonsi_drv_video.so",
            "nvidia_drv_video.so",
        ] {
            let path = Path::new(directory).join(name);
            if let Ok(meta) = std::fs::metadata(&path) {
                bytes.extend(path.to_string_lossy().as_bytes());
                bytes.extend(meta.len().to_le_bytes());
                if let Ok(modified) = meta.modified().and_then(|time| {
                    time.duration_since(std::time::UNIX_EPOCH)
                        .map_err(std::io::Error::other)
                }) {
                    bytes.extend(modified.as_nanos().to_le_bytes());
                }
            }
        }
    }
    hex_digest(&bytes)
}

#[derive(Clone)]
struct Candidate {
    encoder: Encoder,
    device_id: Option<String>,
    proven_pipeline: Pipeline,
}

fn candidates(caps: &EncoderCaps, proven: &[(Encoder, Pipeline)]) -> Vec<Candidate> {
    super::ENCODERS
        .into_iter()
        .filter(|encoder| caps.available(*encoder))
        .map(|encoder| {
            let binding = caps.device_bindings.iter().find(|binding| {
                binding.backend == encoder.family_name()
                    && binding.binding == "configured"
                    && binding.device_ids.len() == 1
            });
            Candidate {
                encoder,
                device_id: binding.map(|binding| binding.device_ids[0].clone()),
                proven_pipeline: proven
                    .iter()
                    .find(|(candidate, _)| *candidate == encoder)
                    .map_or(Pipeline::Cpu, |(_, pipe)| *pipe),
            }
        })
        .collect()
}

fn source_metadata(case: &Case, path: &Path) -> plurx_core::domain::MediaFile {
    plurx_core::domain::MediaFile {
        id: 0,
        item_id: 0,
        path: path.into(),
        size: 0,
        mtime: 0,
        duration_ms: Some(
            i64::from(case.frame_count) * 1000 * i64::from(case.fps_den) / i64::from(case.fps_num),
        ),
        container: Some("mkv".into()),
        video_codec: Some(case.source_codec.clone()),
        video_codec_tag: None,
        field_order: Some("progressive".into()),
        video_profile: None,
        width: Some(i64::from(case.source_width)),
        height: Some(i64::from(case.source_height)),
        bit_depth: Some(i64::from(case.source_bit_depth)),
        hdr: case.source_hdr.clone(),
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        dolby_vision: Default::default(),
        bitrate: None,
        audio_streams: Vec::new(),
        subtitle_streams: Vec::new(),
        downloaded_subtitles: Vec::new(),
        scanned_at: 0,
        audio_offset_ms: 0,
        probed: true,
    }
}

fn arguments(
    case: &Case,
    candidate: &Candidate,
    input: &Path,
    caps: &EncoderCaps,
    manager: &crate::transcode::TranscodeManager,
    probe: &str,
) -> Result<(Vec<String>, String, String, Option<String>), String> {
    let source = source_metadata(case, input);
    let options = TranscodeOptions {
        output_codec: Some(plurx_core::transcode::VideoCodec::H264),
        target_height: i64::from(case.target_height),
        video_bitrate_kbps: case.bitrate_kbps,
        pipeline: candidate.proven_pipeline,
        force_idr: caps.forced_idr.wanted_by(candidate.encoder),
        software_threads: Some(
            crate::admission::Workload {
                source_height: i64::from(case.source_height),
                codec: &case.source_codec,
                hdr: case.source_hdr.as_deref(),
                target_height: i64::from(case.target_height),
            }
            .software_threads() as u32,
        ),
        ..Default::default()
    };
    let plan =
        manager.representative_comparison_plan(&source, &options, candidate.encoder, probe)?;
    let execution = plurx_core::transcode::TranscodeExecution::from_options(
        &source,
        &options,
        plurx_core::transcode::Pacing::unpaced(),
        "representative-null-output",
    )
    .map_err(|error| error.to_string())?;
    let processing_selection = plan
        .macos_processing_selection()
        .map(|selection| selection.name().to_owned());
    let pipeline = plan.options().pipeline.name().to_owned();
    let decoder = plan.decode().backend().name().to_owned();
    let mut args = plurx_core::transcode::hls_args_for_plan(&plan, &execution);
    // Retain the exact production decoder/filter/encoder/keyframe arguments.
    // The benchmark writes encoded packets to null instead of publishing HLS.
    let muxer = args
        .iter()
        .position(|arg| arg == "-muxdelay")
        .ok_or("Production HLS muxer boundary missing")?;
    args.truncate(muxer);
    let log = args
        .iter()
        .position(|arg| arg == "-loglevel")
        .ok_or("Production logging boundary missing")?;
    args[log + 1] = "info".into();
    args.extend([
        "-nostdin".into(),
        "-nostats".into(),
        "-benchmark".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-frames:v".into(),
        case.frame_count.to_string(),
        "-an".into(),
        "-sn".into(),
        "-dn".into(),
        "-f".into(),
        "null".into(),
        "-".into(),
    ]);
    Ok((args, pipeline, decoder, processing_selection))
}

async fn fixture_probes(paths: &[PathBuf]) -> anyhow::Result<Vec<String>> {
    // Use the production held-file probe and captured reporter identity. The
    // complete metadata capture is bounded and outside all measured runs.
    let reporter = crate::ffmpeg::EncodedExecutable::capture_program(&crate::ffmpeg::ffprobe_bin())
        .await
        .map_err(anyhow::Error::msg)?;
    let mut probes = Vec::new();
    for path in paths {
        let source =
            plurx_core::fs_secure::open_read_nofollow_blocking(path).map_err(anyhow::Error::msg)?;
        probes.push(
            crate::ffmpeg::held_source_probe_json_bound(
                &source,
                crate::process_control::ChildWork::background("representative fixture metadata"),
                &reporter,
            )
            .await
            .map_err(anyhow::Error::msg)?,
        );
    }
    Ok(probes)
}

fn benchmark_value(stderr: &str, key: &str) -> Option<f64> {
    stderr
        .lines()
        .filter(|line| line.starts_with("bench:"))
        .flat_map(str::split_whitespace)
        .find_map(|term| {
            term.strip_prefix(key)?
                .strip_suffix('s')?
                .parse::<f64>()
                .ok()
        })
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn summarize_output(output: std::process::Output, seconds: f64, expected: u32) -> Run {
    let progress = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let frames = progress
        .lines()
        .filter_map(|line| line.strip_prefix("frame=")?.trim().parse().ok())
        .max()
        .unwrap_or(0);
    let fallback = fallback_reported(&stderr);
    let completed = output.status.success()
        && progress.lines().any(|line| line == "progress=end")
        && frames == expected
        && !fallback;
    let pipeline_seconds = benchmark_value(&stderr, "rtime=").filter(|value| *value > 0.0);
    let cpu_seconds = benchmark_value(&stderr, "utime=")
        .zip(benchmark_value(&stderr, "stime="))
        .map(|(user, system)| user + system);
    let error = (!completed).then(|| {
        stderr
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("FFmpeg did not complete the fixed frame range")
            .chars()
            .take(400)
            .collect()
    });
    Run {
        seconds,
        pipeline_seconds,
        startup_seconds: pipeline_seconds.map(|pipeline| (seconds - pipeline).max(0.0)),
        cpu_seconds,
        frames,
        completed,
        fallback,
        error,
    }
}

fn fallback_reported(value: &str) -> bool {
    let lower = value.to_lowercase();
    [
        "falling back",
        "fallback to software",
        "failed setup for format",
        "hardware acceleration failed",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

async fn read_tail(mut input: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    const LIMIT: usize = 64 * 1024;
    let mut tail = Vec::new();
    let mut fallback = false;
    let mut buffer = [0u8; 4096];
    loop {
        let count = input.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        tail.extend_from_slice(&buffer[..count]);
        fallback |= fallback_reported(&String::from_utf8_lossy(&tail));
        if tail.len() > LIMIT {
            tail.drain(..tail.len() - LIMIT);
        }
    }
    if fallback {
        tail.extend_from_slice(b"\nfallback to software detected earlier in output\n");
    }
    Ok(tail)
}

async fn run(ffmpeg: &str, args: &[String], expected: u32) -> Run {
    let start = Instant::now();
    let output = async {
        let mut command = tokio::process::Command::new(ffmpeg);
        command
            .args(args)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let (mut child, _child_job) = crate::process_control::spawn_job_owned(
            &mut command,
            crate::process_control::ChildWork::background("representative transcode comparison"),
        )?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (status, stdout, stderr) =
            tokio::try_join!(child.wait(), read_tail(stdout), read_tail(stderr))?;
        Ok::<_, std::io::Error>(std::process::Output {
            status,
            stdout,
            stderr,
        })
    };
    match tokio::time::timeout(Duration::from_secs(60), output).await {
        Ok(Ok(output)) => summarize_output(output, start.elapsed().as_secs_f64(), expected),
        result => Run {
            seconds: start.elapsed().as_secs_f64(),
            pipeline_seconds: None,
            startup_seconds: None,
            cpu_seconds: None,
            frames: 0,
            completed: false,
            fallback: false,
            error: Some(match result {
                Err(_) => "Workload exceeded its 60-second limit".into(),
                Ok(Err(error)) => error.to_string(),
                _ => unreachable!(),
            }),
        },
    }
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() || values.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[values.len() / 2])
}

fn summarize(row: &mut ResultRow, case: &Case) {
    row.fallback = row.runs.iter().any(|run| run.fallback)
        || row.warmup.as_ref().is_some_and(|run| run.fallback);
    let complete = row.runs.len() == REPEATS
        && row.runs.iter().all(|run| run.completed)
        && row.warmup.as_ref().is_some_and(|run| run.completed)
        && !row.fallback;
    row.completion = if complete { "complete" } else { "failed" }.into();
    if !complete {
        row.error = row
            .runs
            .iter()
            .find_map(|run| run.error.clone())
            .or_else(|| row.warmup.as_ref().and_then(|run| run.error.clone()));
        return;
    }
    row.median_seconds = median(row.runs.iter().map(|run| run.seconds).collect());
    row.median_pipeline_seconds = row
        .runs
        .iter()
        .map(|run| run.pipeline_seconds)
        .collect::<Option<Vec<_>>>()
        .and_then(median);
    row.median_startup_seconds = row
        .runs
        .iter()
        .map(|run| run.startup_seconds)
        .collect::<Option<Vec<_>>>()
        .and_then(median);
    row.cpu_percent = row
        .runs
        .iter()
        .map(|run| run.cpu_seconds.map(|cpu| 100.0 * cpu / run.seconds))
        .collect::<Option<Vec<_>>>()
        .and_then(median);
    row.realtime_ratio = row
        .median_seconds
        .filter(|seconds| *seconds > 0.0)
        .map(|seconds| {
            f64::from(case.frame_count) * f64::from(case.fps_den)
                / f64::from(case.fps_num)
                / seconds
        });
}

/// Materialize only compiled, hashed fixture bytes into a private versioned
/// cache. Atomic renames prevent a canceled write becoming a future input.
async fn materialize(cache: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let directory = cache.join("transcoder-comparison-v1");
    tokio::fs::create_dir_all(&directory).await?;
    let mut paths = Vec::new();
    let manifest: serde_json::Value = serde_json::from_str(include_str!("fixtures/manifest.json"))?;
    for (id, _, bytes) in fixtures() {
        let digest = hex_digest(bytes);
        let manifest_hash = manifest["files"]
            .as_array()
            .and_then(|files| {
                files
                    .iter()
                    .find(|file| file["path"].as_str() == Some(&format!("{id}.mkv")))
            })
            .and_then(|file| file["sha256"].as_str());
        anyhow::ensure!(
            manifest_hash == Some(digest.as_str()),
            "Compiled fixture {id} does not match the versioned manifest"
        );
        let path = directory.join(format!("{id}-{digest}.mkv"));
        let bounded_file = tokio::fs::symlink_metadata(&path)
            .await
            .is_ok_and(|meta| meta.file_type().is_file() && meta.len() == bytes.len() as u64);
        let valid = bounded_file
            && tokio::fs::read(&path)
                .await
                .is_ok_and(|cached| hex_digest(&cached) == digest);
        if !valid {
            let mut staging = tempfile::NamedTempFile::new_in(&directory)?;
            std::io::Write::write_all(&mut staging, bytes)?;
            staging.persist(&path).map_err(|error| error.error)?;
        }
        paths.push(path);
    }
    Ok(paths)
}

pub async fn measure(
    ffmpeg: &str,
    caps: &EncoderCaps,
    cache: &Path,
    proven: &[(Encoder, Pipeline)],
    manager: &crate::transcode::TranscodeManager,
) -> anyhow::Result<Comparison> {
    let (path, size, modified) = executable_stamp(ffmpeg)?;
    let executable_sha256 =
        executable_digest(&(path.clone(), size, modified)).context("read FFmpeg identity")?;
    let mut comparison = Comparison {
        version: VERSION,
        fixture_source: fixture_source(),
        policy_snapshot: PolicySnapshot::current(manager),
        status: "running".into(),
        error: None,
        executable_sha256,
        executable_path: path,
        executable_size: size,
        executable_modified_ns: modified,
        hardware_identity: hardware_identity(caps),
        cases: cases(),
        results: Vec::new(),
    };
    super::STATUS.lock().await.comparison = Some(comparison.clone());
    let paths = materialize(cache).await?;
    let probes = fixture_probes(&paths).await?;
    for candidate in candidates(caps, proven) {
        for ((case, input), probe) in comparison.cases.iter().zip(&paths).zip(&probes) {
            let mut row = ResultRow {
                case_id: case.id.clone(),
                backend: candidate.encoder.family_name().into(),
                device_id: candidate.device_id.clone(),
                pipeline: String::new(),
                decoder: String::new(),
                processing_selection: None,
                command: Vec::new(),
                median_seconds: None,
                median_pipeline_seconds: None,
                median_startup_seconds: None,
                cpu_percent: None,
                realtime_ratio: None,
                relative_to_cpu: None,
                completion: "running".into(),
                fallback: false,
                warmup: None,
                runs: Vec::new(),
                error: None,
            };
            match arguments(case, &candidate, input, caps, manager, probe) {
                Ok((args, pipeline, decoder, processing_selection)) => {
                    row.pipeline = pipeline;
                    row.decoder = decoder;
                    row.processing_selection = processing_selection;
                    row.command = args
                        .iter()
                        .map(|arg| {
                            if arg == &input.to_string_lossy() {
                                "<representative-fixture>".into()
                            } else {
                                arg.clone()
                            }
                        })
                        .collect();
                    row.warmup = Some(run(ffmpeg, &args, case.frame_count).await);
                    if row.warmup.as_ref().is_some_and(|run| run.completed) {
                        for _ in 0..REPEATS {
                            row.runs.push(run(ffmpeg, &args, case.frame_count).await);
                        }
                    }
                    summarize(&mut row, case);
                }
                Err(error) => {
                    row.completion = "unavailable".into();
                    row.error = Some(error);
                }
            }
            comparison.results.push(row);
            super::STATUS.lock().await.comparison = Some(comparison.clone());
        }
    }
    for index in 0..comparison.results.len() {
        let cpu = comparison
            .results
            .iter()
            .find(|row| {
                row.backend == "software" && row.case_id == comparison.results[index].case_id
            })
            .and_then(|row| row.median_seconds);
        comparison.results[index].relative_to_cpu = cpu
            .zip(comparison.results[index].median_seconds)
            .map(|(cpu, seconds)| cpu / seconds);
    }
    let cpu_complete = comparison
        .results
        .iter()
        .filter(|row| row.backend == "software")
        .count()
        == 3
        && comparison
            .results
            .iter()
            .filter(|row| row.backend == "software")
            .all(|row| row.completion == "complete");
    comparison.status = if cpu_complete { "complete" } else { "failed" }.into();
    if !cpu_complete {
        comparison.error = Some(
            "The complete CPU baseline is required; partial results do not change Auto".into(),
        );
    }
    // A binary/driver replacement during the run invalidates every row.
    if executable_stamp(ffmpeg)? != (comparison.executable_path.clone(), size, modified)
        || comparison.hardware_identity != hardware_identity(caps)
        || comparison.policy_snapshot != PolicySnapshot::current(manager)
    {
        comparison.status = "failed".into();
        comparison.error = Some(
            "FFmpeg, hardware identity, or production processing policy changed during comparison"
                .into(),
        );
    }
    super::STATUS.lock().await.comparison = Some(comparison.clone());
    Ok(comparison)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn complete_row(case: &Case, backend: &str, seconds: f64) -> ResultRow {
        let sample = Run {
            seconds,
            pipeline_seconds: Some(seconds * 0.9),
            startup_seconds: Some(seconds * 0.1),
            cpu_seconds: Some(seconds * 0.5),
            frames: FRAME_COUNT,
            completed: true,
            fallback: false,
            error: None,
        };
        let mut row = ResultRow {
            case_id: case.id.clone(),
            backend: backend.into(),
            device_id: None,
            pipeline: "cpu".into(),
            decoder: "software".into(),
            processing_selection: None,
            command: Vec::new(),
            median_seconds: None,
            median_pipeline_seconds: None,
            median_startup_seconds: None,
            cpu_percent: None,
            realtime_ratio: None,
            relative_to_cpu: None,
            completion: "running".into(),
            fallback: false,
            warmup: Some(sample.clone()),
            runs: vec![sample; REPEATS],
            error: None,
        };
        summarize(&mut row, case);
        row
    }
    fn complete_comparison(exe: &Path) -> Comparison {
        let (path, size, modified) =
            executable_stamp(exe.to_str().expect("test executable path is UTF-8"))
                .expect("capture test executable identity");
        let executable_sha256 =
            executable_digest(&(path.clone(), size, modified)).expect("hash test executable");
        let cases = cases();
        let results = cases
            .iter()
            .map(|case| complete_row(case, "software", 0.1))
            .collect();
        Comparison {
            version: VERSION,
            fixture_source: fixture_source(),
            policy_snapshot: PolicySnapshot::flags(false, false),
            status: "complete".into(),
            error: None,
            executable_sha256,
            executable_path: path,
            executable_size: size,
            executable_modified_ns: modified,
            hardware_identity: hardware_identity(&EncoderCaps::default()),
            cases,
            results,
        }
    }
    #[test]
    fn auto_ranks_completed_hardware_speed_and_cpu_only_when_hardware_rejected() {
        let directory = tempfile::tempdir().expect("create test directory");
        let exe = directory.path().join("ffmpeg");
        std::fs::write(&exe, b"first").expect("write test executable");
        let mut report = complete_comparison(&exe);
        let caps = EncoderCaps {
            qsv: true,
            nvenc: true,
            ..Default::default()
        };
        for case in &report.cases {
            report.results.push(complete_row(case, "qsv", 2.0));
            report.results.push(complete_row(case, "nvenc", 1.0));
        }
        // The CPU baseline is ten times faster and still cannot displace working hardware.
        assert_eq!(report.preferred_hardware(&caps), Some(Encoder::Nvenc));
        report
            .results
            .iter_mut()
            .find(|row| row.backend == "nvenc")
            .expect("find NVENC result")
            .completion = "failed".into();
        assert_eq!(report.preferred_hardware(&caps), Some(Encoder::Qsv));
        report
            .results
            .iter_mut()
            .find(|row| row.backend == "qsv")
            .expect("find QSV result")
            .runs
            .pop();
        assert_eq!(report.preferred_hardware(&caps), None);
        report.status = "aborted".into();
        assert_eq!(report.preferred_hardware(&caps), None);
    }
    #[tokio::test]
    async fn bounded_stderr_retains_early_fallback_evidence() {
        let mut output = b"Hardware acceleration failed\n".to_vec();
        output.extend(vec![b'x'; 200_000]);
        let tail = read_tail(std::io::Cursor::new(output))
            .await
            .expect("read bounded diagnostic stream");
        assert!(tail.len() < 65 * 1024);
        assert!(fallback_reported(&String::from_utf8_lossy(&tail)));
    }
    #[test]
    fn completion_requires_exact_frames_success_and_final_progress() {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            let output = |frames, success, fallback: &str| std::process::Output {
                status: std::process::ExitStatus::from_raw(if success { 0 } else { 256 }),
                stdout: format!("frame={frames}\nprogress=end\n").into_bytes(),
                stderr: format!("bench: utime=0.8s stime=0.2s rtime=1.0s\n{fallback}").into_bytes(),
            };
            let result = summarize_output(output(240, true, ""), 1.2, 240);
            assert!(result.completed);
            assert_eq!(result.cpu_seconds, Some(1.0));
            assert!(!summarize_output(output(239, true, ""), 1.2, 240).completed);
            assert!(!summarize_output(output(240, false, ""), 1.2, 240).completed);
            assert!(
                !summarize_output(output(240, true, "Hardware acceleration failed"), 1.2, 240)
                    .completed
            );
        }
    }
    #[test]
    fn median_excludes_warmup_and_missing_cpu_is_not_zero() {
        let mut row = ResultRow {
            case_id: "x".into(),
            backend: "software".into(),
            device_id: None,
            pipeline: "cpu".into(),
            decoder: "software".into(),
            processing_selection: None,
            command: Vec::new(),
            median_seconds: None,
            median_pipeline_seconds: None,
            median_startup_seconds: None,
            cpu_percent: None,
            realtime_ratio: None,
            relative_to_cpu: None,
            completion: "running".into(),
            fallback: false,
            warmup: None,
            runs: Vec::new(),
            error: None,
        };
        let sample = |seconds| Run {
            seconds,
            pipeline_seconds: None,
            startup_seconds: None,
            cpu_seconds: None,
            frames: 240,
            completed: true,
            fallback: false,
            error: None,
        };
        row.warmup = Some(sample(99.0));
        row.runs = vec![sample(1.0), sample(3.0), sample(2.0)];
        summarize(&mut row, &cases()[0]);
        assert_eq!(row.median_seconds, Some(2.0));
        assert_eq!(row.cpu_percent, None);
        assert_eq!(row.realtime_ratio, Some(5.0));
        row.runs.pop();
        summarize(&mut row, &cases()[0]);
        assert_eq!(row.completion, "failed");
    }
    #[tokio::test]
    async fn corrupt_cached_fixture_is_replaced_with_trusted_bytes() {
        let directory = tempfile::tempdir().expect("create fixture cache directory");
        let paths = materialize(directory.path())
            .await
            .expect("materialize trusted fixtures");
        tokio::fs::write(&paths[0], b"corrupted")
            .await
            .expect("corrupt cached fixture");
        materialize(directory.path())
            .await
            .expect("replace corrupted fixture");
        assert_eq!(
            tokio::fs::read(&paths[0])
                .await
                .expect("read restored fixture"),
            fixtures()[0].2
        );
    }
    #[test]
    fn replacing_executable_or_aborting_run_invalidates_authority() {
        let directory = tempfile::tempdir().expect("create identity test directory");
        let exe = directory.path().join("ffmpeg");
        std::fs::write(&exe, b"first").expect("write initial executable");
        let name = exe.to_str().expect("test executable path is UTF-8");
        let mut report = complete_comparison(&exe);
        assert!(report.valid_for(
            name,
            &EncoderCaps::default(),
            &PolicySnapshot::flags(false, false)
        ));
        report.status = "aborted".into();
        assert!(!report.valid_for(
            name,
            &EncoderCaps::default(),
            &PolicySnapshot::flags(false, false)
        ));
        report.status = "complete".into();
        report.executable_sha256 = "stale".into();
        assert!(!report.valid_for(
            name,
            &EncoderCaps::default(),
            &PolicySnapshot::flags(false, false)
        ));
        report = complete_comparison(&exe);
        report.cases[0].fixture_sha256 = "stale".into();
        assert!(!report.valid_for(
            name,
            &EncoderCaps::default(),
            &PolicySnapshot::flags(false, false)
        ));
        report = complete_comparison(&exe);
        report.hardware_identity = "previous driver".into();
        assert!(!report.valid_for(
            name,
            &EncoderCaps::default(),
            &PolicySnapshot::flags(false, false)
        ));
        report = complete_comparison(&exe);
        assert!(!report.valid_for(
            name,
            &EncoderCaps::default(),
            &PolicySnapshot::flags(true, false)
        ));
        report = complete_comparison(&exe);
        report.results.pop();
        assert!(!report.valid_for(
            name,
            &EncoderCaps::default(),
            &PolicySnapshot::flags(false, false)
        ));
        report = complete_comparison(&exe);
        std::fs::write(&exe, b"changed executable").expect("replace executable");
        assert!(!report.valid_for(
            name,
            &EncoderCaps::default(),
            &PolicySnapshot::flags(false, false)
        ));
    }
    /// Run alone with explicit PLURX_FFMPEG/PLURX_FFPROBE and optional
    /// PLURX_COMPARISON_RECEIPT=/absolute/path.json; never contacts a daemon.
    #[tokio::test]
    #[ignore = "requires the node's production media tools and actual GPU"]
    async fn real_media_production_tools_comparison() {
        let ffmpeg = std::env::var("PLURX_COMPARISON_FFMPEG")
            .or_else(|_| std::env::var("PLURX_FFMPEG"))
            .expect("set an explicit production FFmpeg executable");
        assert_eq!(
            Path::new(&ffmpeg)
                .canonicalize()
                .expect("canonicalize explicit FFmpeg executable"),
            Path::new(&crate::ffmpeg::ffmpeg_bin())
                .canonicalize()
                .expect("canonicalize configured FFmpeg executable"),
            "pipeprobe and comparison must use the same executable; set PLURX_FFMPEG too"
        );
        let cache = crate::test_tempdir().expect("create secure comparison cache");
        let scratch = crate::test_tempdir().expect("create secure probe workspace");
        let caps = plurx_core::transcode::detect_encoders(&ffmpeg).await;
        let mut proven = Vec::new();
        let mut metal_parity = crate::pipeprobe::CpuParity::NotMeasured;
        for encoder in super::super::ENCODERS
            .into_iter()
            .filter(|encoder| caps.available(*encoder))
        {
            let pipe = if encoder == Encoder::Software {
                Pipeline::Cpu
            } else {
                let report = crate::pipeprobe::probe(scratch.path(), cache.path(), encoder).await;
                if encoder == Encoder::VideoToolbox {
                    metal_parity = report.parity(Pipeline::VtToneMapMetal);
                }
                report.selected()
            };
            proven.push((encoder, pipe));
        }
        let store: std::sync::Arc<dyn plurx_core::store::Store> = std::sync::Arc::new(
            plurx_core::store::SqliteStore::open_in_memory()
                .expect("open in-memory comparison store"),
        );
        let manager = crate::transcode::TranscodeManager::new(
            store,
            cache.path().join("work"),
            caps.clone(),
            Pipeline::Cpu,
        )
        .with_decoders(plurx_core::transcode::detect_video_decoders(&ffmpeg).await)
        .with_metal_tone_map_parity(metal_parity);
        manager.set_macos_video_processing_enabled(
            std::env::var("PLURX_COMPARISON_MACOS_PROCESSING").is_ok_and(|value| value == "1"),
        );
        manager
            .reprobe_macos_video(&tokio_util::sync::CancellationToken::new())
            .await;
        let paths = materialize(cache.path())
            .await
            .expect("materialize real-media fixtures");
        let probes = fixture_probes(&paths)
            .await
            .expect("probe actual fixture metadata");
        let qualification = manager.macos_video_report().diagnostics();
        let mut previews = Vec::new();
        for candidate in candidates(&caps, &proven) {
            for ((case, path), probe) in cases().iter().zip(&paths).zip(&probes) {
                let (_, pipeline, decoder, selection) =
                    arguments(case, &candidate, path, &caps, &manager, probe)
                        .expect("resolve production fixture plan");
                previews.push(serde_json::json!({"case_id":case.id,"backend":candidate.encoder.family_name(),"pipeline":pipeline,"decoder":decoder,"processing_selection":selection}));
            }
        }
        if let Ok(path) = std::env::var("PLURX_COMPARISON_RECEIPT") {
            std::fs::write(path,serde_json::to_vec_pretty(&serde_json::json!({"status":"planning","previews":previews,"macos_processing_qualification":qualification})).expect("serialize planning receipt")).expect("write planning receipt");
        }
        if manager.macos_video_processing_enabled() {
            for preview in &previews {
                if preview["backend"] != "videotoolbox" {
                    continue;
                }
                let hdr = preview["case_id"] == "4k-hevc-hdr10";
                let available = qualification[if hdr { "hdr10_metal" } else { "sdr_scale" }]
                    ["availability"]
                    == "available";
                if available {
                    assert_eq!(
                        preview["pipeline"],
                        if hdr {
                            Pipeline::VtToneMapMetal.name()
                        } else {
                            Pipeline::VtScaleSdr.name()
                        },
                        "qualified native production graph must resolve before timing: {preview}"
                    );
                }
            }
        }
        let result = tokio::time::timeout(
            Duration::from_secs(600),
            measure(&ffmpeg, &caps, cache.path(), &proven, &manager),
        )
        .await
        .expect("comparison deadline")
        .expect("complete real-media comparison");
        if let Ok(path) = std::env::var("PLURX_COMPARISON_RECEIPT") {
            let mut receipt = serde_json::to_value(&result).expect("serialize comparison receipt");
            receipt["macos_processing_qualification"] = manager.macos_video_report().diagnostics();
            std::fs::write(
                path,
                serde_json::to_vec_pretty(&receipt).expect("serialize pretty comparison receipt"),
            )
            .expect("write comparison receipt");
        }
        assert_eq!(result.status, "complete", "{:?}", result.error);
        assert!(result.valid_for(&ffmpeg, &caps, &PolicySnapshot::current(&manager)));
        assert!(result
            .results
            .iter()
            .filter(|row| row.backend == "software")
            .all(|row| row.completion == "complete"));
    }
}
