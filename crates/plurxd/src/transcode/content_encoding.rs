//! Bounded sample analysis owned by the existing speculative producer.
//!
//! There is no analysis task on Play. A durable producer performs this phase
//! while it owns its ordinary source, cancellation and CPU admission. Its
//! measured result is reusable by completed-cache lookups and later offline
//! requests with the same source and output policy. Absence, refusal and failed
//! measurements retain VBR.
use super::*;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

const VERSION: u32 = 1;
use plurx_core::store::CONTENT_ENCODING_PROBE_KEY as PROBE_KEY;
const MAX_SOURCE: u64 = 32 * 1024 * 1024 * 1024;
const MAX_MEDIA: u64 = 64 * 1024 * 1024;
struct Scorer {
    executable: crate::ffmpeg::EncodedExecutable,
    available: bool,
}
static SCORER_READY: tokio::sync::OnceCell<Scorer> = tokio::sync::OnceCell::const_new();

const QUALITIES: [u8; 3] = [18, 23, 28];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Context {
    version: u32,
    source: LocalSourceSnapshot,
    engine: String,
    scorer_digest: String,
    probe_digest: String,
    recipe_version: i64,
    width: i64,
    height: i64,
    bitrate: u32,
    threads: u32,
    /// Exact production arguments, including future changes to encoder policy.
    recipes: Vec<Vec<String>>,
    keyframe_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Measurement {
    bytes: u64,
    seconds: f64,
    mean: f64,
    p10: f64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Report {
    context: Context,
    source_sha256: String,
    outcome: String,
    /// Three windows, each baseline followed by the three candidates.
    windows: Vec<Vec<Measurement>>,
}

impl Report {
    fn rate_for(&self, context: &Context) -> Option<EffectiveRateControl> {
        if &self.context != context
            || self.outcome != "measured"
            || self.source_sha256.len() != 64
            || !self
                .source_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return None;
        }
        winner(&self.windows).map(|quality| EffectiveRateControl::Qvbr { quality })
    }
}

/// A cache reader may name the producer's thread recipe because it never
/// starts an encoder. A new offline encode must retain its own thread recipe.
/// The returned options remain provisional until the complete report context
/// has been validated and a completed artifact has passed ordinary admission.
fn content_lookup_options(
    options: &TranscodeOptions,
    producer_threads: Option<u32>,
) -> Option<TranscodeOptions> {
    if options.effective_rate_control != EffectiveRateControl::Vbr
        || options.normalized_geometry
        || options.auto_quality_rate_profile.is_some()
        || producer_threads.is_some_and(|threads| !(1..=6).contains(&threads))
    {
        return None;
    }
    let mut candidate = options.clone();
    if let Some(threads) = producer_threads {
        candidate.software_threads = Some(threads);
    }
    Some(candidate)
}

fn winner(windows: &[Vec<Measurement>]) -> Option<u8> {
    if windows.len() != 3 || windows.iter().any(|row| row.len() != 4) {
        return None;
    }
    let valid = |m: &Measurement| {
        m.bytes > 0
            && m.bytes <= MAX_MEDIA
            && m.seconds.is_finite()
            && m.seconds <= 180.0
            && m.seconds > 0.0
            && m.mean.is_finite()
            && (0.0..=100.0).contains(&m.mean)
            && m.p10.is_finite()
            && (0.0..=100.0).contains(&m.p10)
    };
    if windows.iter().flatten().any(|m| !valid(m)) {
        return None;
    }
    let base_bytes: f64 = windows.iter().map(|w| w[0].bytes as f64).sum();
    let base_seconds: f64 = windows.iter().map(|w| w[0].seconds).sum();
    QUALITIES
        .iter()
        .enumerate()
        .filter_map(|(index, quality)| {
            let candidate = index + 1;
            if windows.iter().any(|w| {
                w[candidate].mean < 93.0
                    || w[candidate].mean < w[0].mean
                    || w[candidate].p10 < w[0].p10
            }) {
                return None;
            }
            let bytes: f64 = windows.iter().map(|w| w[candidate].bytes as f64).sum();
            let seconds: f64 = windows.iter().map(|w| w[candidate].seconds).sum();
            (bytes <= base_bytes * 0.9 && seconds <= base_seconds * 1.1)
                .then_some((bytes as u64, *quality))
        })
        .min()
        .map(|(_, quality)| quality)
}

fn metric(document: &[u8], expected_frames: usize) -> Result<(f64, f64), String> {
    let value: serde_json::Value = serde_json::from_slice(document).map_err(|e| e.to_string())?;
    let frames = value["frames"].as_array().ok_or("missing VMAF frames")?;
    if frames.len() != expected_frames {
        return Err("incomplete VMAF sample".into());
    }
    let mut scores = Vec::with_capacity(frames.len());
    for (index, frame) in frames.iter().enumerate() {
        if frame["frameNum"].as_u64() != Some(index as u64) {
            return Err("nonsequential VMAF frames".into());
        }
        let score = frame["metrics"]["vmaf"]
            .as_f64()
            .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
            .ok_or("invalid VMAF value")?;
        scores.push(score);
    }
    let mean = scores.iter().sum::<f64>() / scores.len() as f64;
    scores.sort_by(f64::total_cmp);
    let p10 = scores[(scores.len().div_ceil(10)).saturating_sub(1)];
    Ok((mean, p10))
}

/// Where a content lookup gets the stored probe: held by the caller, or read.
enum HeldProbe<'a> {
    Held(Option<&'a str>),
    Read,
}

impl TranscodeManager {
    pub fn content_encoding_scorer_ready(&self) -> Option<bool> {
        SCORER_READY.get().map(|scorer| scorer.available)
    }

    /// Reports already-published facts; reading settings never launches a probe.
    pub fn content_encoding_applicability(&self, encoder_preference: &str) -> serde_json::Value {
        let policy = self.rate_control_snapshot();
        serde_json::json!({
            "selected_encoder": self.caps.choose(encoder_preference).family_name(),
            "explicit_rate_control": policy.requested_mode.is_some() || policy.requested_quality.is_some(),
            "software_quality_supported": policy.quality_rc.supported_by(Encoder::Software),
        })
    }

    pub async fn content_encoding_enabled(&self) -> bool {
        self.store
            .get_setting(keys::CONTENT_AWARE_ENCODING)
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some("1")
    }

    async fn content_context(
        &self,
        file: &plurx_core::domain::MediaFile,
        opts: &TranscodeOptions,
        source: LocalSourceSnapshot,
        probe: &serde_json::Value,
    ) -> Option<Context> {
        let (width, height) = transcode::output_size(file, opts.target_height)?;
        let threads = opts
            .software_threads
            .unwrap_or_else(|| Workload::of(file, opts.target_height).software_threads() as u32);
        if !(1..=6).contains(&threads) {
            return None;
        }
        if file.hdr.is_some()
            || opts.subtitle_burn.is_some()
            || !file.width.is_some_and(|v| (2..=4096).contains(&v))
            || !file.height.is_some_and(|v| (2..=2160).contains(&v))
            || !(2..=1920).contains(&width)
            || !(2..=1080).contains(&height)
            || file.size <= 0
            || file.size as u64 > MAX_SOURCE
        {
            return None;
        }
        if !crate::ffmpeg::fragment_index_engine_is_current().await {
            return None;
        }
        Some(Context {
            version: VERSION,
            source,
            engine: crate::ffmpeg::fragment_index_engine_digest().await,
            scorer_digest: SCORER_READY
                .get()
                .map(|scorer| scorer.executable.digest.clone())
                .unwrap_or_default(),
            probe_digest: hex::encode(Sha256::digest(probe["streams"].to_string().as_bytes())),
            recipe_version: CACHE_RECIPE_VERSION,
            width,
            height,
            bitrate: opts.video_bitrate_kbps,
            threads,
            keyframe_args: transcode::hls_keyframe_args(),
            recipes: std::iter::once(EffectiveRateControl::Vbr)
                .chain(QUALITIES.map(|quality| EffectiveRateControl::Qvbr { quality }))
                .map(|mode| {
                    Encoder::Software.encode_args_for(
                        OutputGrade::Sdr,
                        opts.video_bitrate_kbps,
                        mode,
                        true,
                        Some(threads),
                    )
                })
                .collect(),
        })
    }

    /// Reads metadata only. Never probes, scores or hashes media on a request path.
    pub(super) async fn measured_content_rate(
        &self,
        file: &plurx_core::domain::MediaFile,
        opts: &TranscodeOptions,
        encoder: Encoder,
    ) -> Option<EffectiveRateControl> {
        self.measured_content_options(file, opts, encoder, false)
            .await
            .map(|candidate| candidate.effective_rate_control)
    }

    /// Cache-only alternative: callers must retain the original live options on
    /// a miss. Report thread counts describe already-produced bytes, not a new
    /// admission request or permission to spend encoder capacity on Play. The
    /// switch and stored probe are the ones a rolling start already read in
    /// its one planning snapshot.
    pub(super) async fn measured_content_cache_options_from(
        &self,
        file: &plurx_core::domain::MediaFile,
        opts: &TranscodeOptions,
        encoder: Encoder,
        enabled: bool,
        stored_probe: Option<&str>,
    ) -> Option<TranscodeOptions> {
        self.measured_content_options_with(
            file,
            opts,
            encoder,
            true,
            enabled,
            HeldProbe::Held(stored_probe),
        )
        .await
    }

    async fn measured_content_options(
        &self,
        file: &plurx_core::domain::MediaFile,
        opts: &TranscodeOptions,
        encoder: Encoder,
        cache_only: bool,
    ) -> Option<TranscodeOptions> {
        let enabled = self.content_encoding_enabled().await;
        self.measured_content_options_with(
            file,
            opts,
            encoder,
            cache_only,
            enabled,
            HeldProbe::Read,
        )
        .await
    }

    async fn measured_content_options_with(
        &self,
        file: &plurx_core::domain::MediaFile,
        opts: &TranscodeOptions,
        encoder: Encoder,
        cache_only: bool,
        enabled: bool,
        probe: HeldProbe<'_>,
    ) -> Option<TranscodeOptions> {
        let policy = self.rate_control_snapshot();
        if encoder != Encoder::Software
            || policy.requested_mode.is_some()
            || policy.requested_quality.is_some()
            || policy.effective_for(encoder) != EffectiveRateControl::Vbr
            || !policy.quality_rc.supported_by(encoder)
            || !enabled
        {
            return None;
        }
        let scorer = SCORER_READY.get()?;
        if !scorer.available || !scorer_current(scorer).await {
            return None;
        }
        let path = file.path.clone();
        let snapshot = tokio::task::spawn_blocking(move || {
            let source = plurx_core::fs_secure::open_read_nofollow_blocking(&path).ok()?;
            LocalSourceSnapshot::from_file(&source).ok()
        })
        .await
        .ok()??;
        let raw = match probe {
            HeldProbe::Held(raw) => raw?.to_owned(),
            HeldProbe::Read => self
                .store
                .get_file_probe_json(file.id)
                .await
                .ok()
                .flatten()?,
        };
        let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
        let report: Report = serde_json::from_value(value.get(PROBE_KEY)?.clone()).ok()?;
        let mut candidate =
            content_lookup_options(opts, cache_only.then_some(report.context.threads))?;
        let context = self
            .content_context(file, &candidate, snapshot, &value)
            .await?;
        candidate.effective_rate_control = report.rate_for(&context)?;
        Some(candidate)
    }

    /// Returns a measured mode when it improves every sampled window. Ordinary
    /// unsupported/inconclusive results are persisted once and use the baseline.
    /// Cancellation and contention are never persisted as negative evidence.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn analyze_content_for_producer(
        &self,
        file: &plurx_core::domain::MediaFile,
        opts: &TranscodeOptions,
        encoder: Encoder,
        source: Option<&Arc<BoundPretranscodeSource>>,
        fence: Option<&PretranscodeFence>,
        cancelled: &tokio_util::sync::CancellationToken,
        deadline: Instant,
    ) -> Option<EffectiveRateControl> {
        let policy = self.rate_control_snapshot();
        if encoder != Encoder::Software
            || policy.requested_mode.is_some()
            || policy.requested_quality.is_some()
            || policy.effective_for(encoder) != EffectiveRateControl::Vbr
            || !policy.quality_rc.supported_by(encoder)
            || !self.content_encoding_enabled().await
        {
            return None;
        }
        let source = source?;
        if let Some(scorer) = SCORER_READY.get() {
            if !scorer_current(scorer).await {
                return None;
            }
        }
        let raw = self
            .store
            .get_file_probe_json(file.id)
            .await
            .ok()
            .flatten()?;
        let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
        let context = self
            .content_context(file, opts, source.snapshot, &value)
            .await?;
        if let Some(report) = value
            .get(PROBE_KEY)
            .and_then(|v| serde_json::from_value::<Report>(v.clone()).ok())
        {
            if report.context == context {
                return report.rate_for(&context);
            }
        }
        if self.admissions.background_must_yield() || cancelled.is_cancelled() {
            return None;
        }
        // Shared queue claims already hold a software CPU reservation. Legacy
        // direct producers reserve the matching count in the live playback pool.
        let _permit = if fence.is_some() {
            None
        } else {
            Some(self.admissions.try_admit_software(
                self.software_budget().await,
                context.threads as usize,
                Priority::Background,
            )?)
        };
        let _scratch = self
            .scratch_ledger
            .reserve(
                (MAX_MEDIA * 2 + 1024 * 1024) as i64,
                self.scratch_cap.load(Acquire),
            )
            .ok()?;
        let until = deadline.min(Instant::now() + Duration::from_secs(180));
        let _offset = tokio::select! {
            _ = cancelled.cancelled() => return None,
            permit = tokio::time::timeout_at(until.into(), source.offset_gate.acquire()) => permit.ok()?.ok()?,
        };
        let stop = tokio_util::sync::CancellationToken::new();
        let mut operation =
            Box::pin(self.measure_content(file, source, context.clone(), &value, &stop));
        let mut poll = tokio::time::interval(PRODUCER_POLL);
        let result = loop {
            tokio::select! {
                biased;
                _ = cancelled.cancelled() => { stop.cancel(); let _ = operation.await; return None; },
                _ = tokio::time::sleep_until(until.into()) => {
                    stop.cancel(); let _ = operation.as_mut().await;
                    break Err("analysis_budget_exhausted".into());
                },
                _ = poll.tick() => {
                    if self.admissions.background_must_yield() || !self.content_encoding_enabled().await {
                        stop.cancel(); let _ = operation.await; return None;
                    }
                }
                result = &mut operation => break result,
            }
        };
        drop(operation);
        if cancelled.is_cancelled()
            || !crate::ffmpeg::fragment_index_engine_is_current().await
            || bound_source_snapshot(Some(source)).await != Some(context.source)
        {
            return None;
        }
        let report = match result {
            Ok(report) => report,
            Err(reason) => Report {
                context: Context {
                    scorer_digest: SCORER_READY
                        .get()
                        .map(|scorer| scorer.executable.digest.clone())
                        .unwrap_or_default(),
                    ..context
                },
                source_sha256: String::new(),
                outcome: reason,
                windows: Vec::new(),
            },
        };
        let mode = (report.outcome == "measured")
            .then(|| winner(&report.windows))
            .flatten();
        let json = serde_json::to_string(&report).ok()?;
        if !self
            .store
            .merge_file_probe_content_encoding(file.id, file.size, file.mtime, &json)
            .await
            .ok()?
        {
            return None;
        }
        mode.map(|quality| EffectiveRateControl::Qvbr { quality })
    }

    async fn measure_content(
        &self,
        file: &plurx_core::domain::MediaFile,
        source: &BoundPretranscodeSource,
        mut context: Context,
        probe: &serde_json::Value,
        cancelled: &tokio_util::sync::CancellationToken,
    ) -> Result<Report, String> {
        let scorer = crate::process_control::bounded::cancellable(
            cancelled.clone(),
            SCORER_READY.get_or_try_init(|| async {
                let packaged = PathBuf::from("/usr/local/lib/plurx/vmaf-ffmpeg");
                let executable = if packaged.is_file() {
                    crate::ffmpeg::EncodedExecutable::capture_at(packaged).await?
                } else {
                    crate::ffmpeg::EncodedExecutable::capture().await?
                };
                let args = [
                    "-nostdin",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-filter_complex_threads",
                    "1",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=64x64:rate=10:duration=0.2",
                    "-lavfi",
                    "split[a][b];[a][b]libvmaf=model=version=vmaf_v0.6.1:n_threads=1",
                    "-f",
                    "null",
                    "-",
                ];
                let result = crate::process_control::bounded::output(
                    &executable.path,
                    &args,
                    Duration::from_secs(10),
                    16 * 1024,
                    crate::process_control::ChildWork::background(
                        "content-aware scorer capability",
                    ),
                )
                .await;
                if cancelled.is_cancelled() {
                    return Err("analysis_cancelled".to_owned());
                }
                Ok(Scorer {
                    executable,
                    available: result.is_ok_and(|output| output.status.success()),
                })
            }),
        )
        .await?;
        if cancelled.is_cancelled() {
            return Err("analysis_cancelled".into());
        }
        context.scorer_digest = scorer.executable.digest.clone();
        if !scorer.available {
            return Ok(Report {
                context,
                source_sha256: String::new(),
                outcome: "scorer_unavailable".into(),
                windows: Vec::new(),
            });
        }
        if !scorer_current(scorer).await {
            return Err("scorer_changed".into());
        }
        let streams = probe["streams"].as_array().ok_or("source_probe_missing")?;
        let video = streams
            .iter()
            .find(|s| s["codec_type"] == "video")
            .ok_or("source_video_missing")?;
        // Limit this first measured policy to the same unambiguous SDR pixels
        // as C1. Unknown color, interlace, rotation and anamorphic material keep
        // their ordinary recipes; no inferred color conversions are qualified.
        if ["color_transfer", "color_primaries", "color_space"]
            .iter()
            .any(|k| video[*k] != "bt709")
            || video["color_range"] != "tv"
            || video["sample_aspect_ratio"] != "1:1"
            || video["pix_fmt"] != "yuv420p"
            || video["field_order"]
                .as_str()
                .is_some_and(|v| v != "progressive")
            || video["side_data_list"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
        {
            return Err("source_outside_sdr_sample_contract".into());
        }
        let fps = video["avg_frame_rate"]
            .as_str()
            .and_then(|v| v.split_once('/'))
            .and_then(|(n, d)| Some(n.parse::<f64>().ok()? / d.parse::<f64>().ok()?))
            .filter(|v| v.is_finite() && (1.0..=60.0).contains(v))
            .ok_or("source_frame_rate_missing")?;
        let seconds = file
            .duration_ms
            .filter(|v| *v >= 6000)
            .ok_or("source_too_short")? as f64
            / 1000.0;
        let frames = (fps * 2.0).floor() as usize;
        let temp = tempfile::Builder::new()
            .prefix("content-encoding-")
            .tempdir_in(&self.work_dir)
            .map_err(|e| e.to_string())?;
        let reference = temp.path().join("reference.mkv");
        let encoded = temp.path().join("encoded.mkv");
        let metrics = temp.path().join("metrics.json");
        let before = hash_source(&source.handle, cancelled).await?;
        let mut windows = Vec::with_capacity(3);
        for window in 0..3 {
            rewind_source(&source.handle).await?;
            let start = (window as f64 + 0.5) * seconds / 3.0 - 1.0;
            let mut command = sample_command();
            crate::ffmpeg::inherit_file_descriptors(&mut command, &[(&source.handle, 3)]);
            #[cfg(unix)]
            let input = PathBuf::from("/dev/fd/3");
            #[cfg(windows)]
            let input = crate::ffmpeg::windows_source_path(&source.handle)?;
            #[cfg(windows)]
            crate::ffmpeg::verify_windows_source_path(&source.handle, &input)?;
            command
                .args(["-ss", &format!("{start:.9}"), "-i"])
                .arg(input)
                .args([
                    "-map",
                    "0:v:0",
                    "-an",
                    "-sn",
                    "-dn",
                    "-vf",
                    &format!(
                        "scale={}:{},fps={fps},format=yuv420p,setsar=1,setpts=PTS-STARTPTS",
                        context.width, context.height
                    ),
                    "-frames:v",
                    &frames.to_string(),
                    "-c:v",
                    "ffv1",
                    "-threads",
                    "1",
                    "-color_primaries",
                    "bt709",
                    "-color_trc",
                    "bt709",
                    "-colorspace",
                    "bt709",
                    "-color_range",
                    "tv",
                    "-f",
                    "matroska",
                    "pipe:1",
                ]);
            run_media(command, &reference, cancelled).await?;
            verify_sample(&reference, frames, &context, cancelled).await?;
            let mut row = Vec::with_capacity(4);
            for recipe in &context.recipes {
                let mut command = sample_command();
                command
                    .arg("-i")
                    .arg(&reference)
                    .args(["-map", "0:v:0", "-an", "-sn", "-dn"])
                    .args(recipe)
                    .args(&context.keyframe_args)
                    .args([
                        "-color_primaries",
                        "bt709",
                        "-color_trc",
                        "bt709",
                        "-colorspace",
                        "bt709",
                        "-color_range",
                        "tv",
                        "-f",
                        "matroska",
                        "pipe:1",
                    ]);
                let started = Instant::now();
                run_media(command, &encoded, cancelled).await?;
                let encode_seconds = started.elapsed().as_secs_f64();
                verify_sample(&encoded, frames, &context, cancelled).await?;
                let bytes = tokio::fs::metadata(&encoded)
                    .await
                    .map_err(|e| e.to_string())?
                    .len();
                let mut command = sample_command_for(&scorer.executable.path);
                command.current_dir(temp.path()).arg("-i").arg(&encoded).args(["-threads", "1", "-i"]).arg(&reference)
                    .args(["-lavfi", "[0:v]setpts=PTS-STARTPTS[d];[1:v]setpts=PTS-STARTPTS[r];[d][r]libvmaf=model=version=vmaf_v0.6.1:n_threads=1:n_subsample=1:log_fmt=json:log_path=metrics.json:shortest=1:repeatlast=0", "-an", "-f", "null", "-"]);
                let (status, _) = crate::ffmpeg::BoundedDiagnosticChild::spawn(
                    &mut command,
                    crate::process_control::ChildWork::background("content-aware VMAF scoring"),
                )
                .map_err(|e| e.to_string())?
                .output_cancellable(cancelled)
                .await
                .map_err(|e| e.to_string())?;
                if !status.success() {
                    return Err("scorer_unavailable_or_failed".into());
                }
                let mut scores = Vec::new();
                tokio::fs::File::open(&metrics)
                    .await
                    .map_err(|e| e.to_string())?
                    .take(1024 * 1024 + 1)
                    .read_to_end(&mut scores)
                    .await
                    .map_err(|e| e.to_string())?;
                if scores.len() > 1024 * 1024 {
                    return Err("metric_output_exceeded".into());
                }
                let (mean, p10) = metric(&scores, frames)?;
                row.push(Measurement {
                    bytes,
                    seconds: encode_seconds,
                    mean,
                    p10,
                });
                tokio::fs::remove_file(&encoded)
                    .await
                    .map_err(|e| e.to_string())?;
                tokio::fs::remove_file(&metrics)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            windows.push(row);
            tokio::fs::remove_file(&reference)
                .await
                .map_err(|e| e.to_string())?;
        }
        if !scorer_current(scorer).await {
            return Err("scorer_changed".into());
        }
        if hash_source(&source.handle, cancelled).await? != before {
            return Err("source_content_changed".into());
        }
        Ok(Report {
            context,
            source_sha256: before,
            outcome: "measured".into(),
            windows,
        })
    }
}

async fn scorer_current(scorer: &Scorer) -> bool {
    crate::ffmpeg::engine_objects_are_current_batch(
        None,
        vec![scorer.executable.attestation_object()].into(),
    )
    .await
    .0
}

fn sample_command() -> tokio::process::Command {
    sample_command_for(ffmpeg_bin())
}

fn sample_command_for(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-threads",
            "1",
            "-filter_threads",
            "1",
            "-filter_complex_threads",
            "1",
        ])
        .stdin(std::process::Stdio::null());
    command
}

async fn run_media(
    mut command: tokio::process::Command,
    path: &std::path::Path,
    cancelled: &tokio_util::sync::CancellationToken,
) -> Result<(), String> {
    let (status, _) = crate::ffmpeg::BoundedDiagnosticChild::spawn_piped_output(
        &mut command,
        crate::process_control::ChildWork::background("content-aware sample encode"),
    )
    .map_err(|e| e.to_string())?
    .output_to_bounded_file_cancellable(path, MAX_MEDIA, cancelled)
    .await
    .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("sample_encode_failed".into())
    }
}

async fn sample_probe_document(
    path: &std::path::Path,
    cancelled: &tokio_util::sync::CancellationToken,
) -> Result<serde_json::Value, String> {
    let mut args = [
        "-v",
        "error",
        "-count_frames",
        "-select_streams",
        "v:0",
        "-show_streams",
        "-of",
        "json",
    ]
    .into_iter()
    .map(std::ffi::OsString::from)
    .collect::<Vec<_>>();
    args.push(path.as_os_str().to_owned());
    let output = crate::process_control::bounded::cancellable(
        cancelled.clone(),
        crate::process_control::bounded::output(
            crate::ffmpeg::ffprobe_bin(),
            &args,
            Duration::from_secs(10),
            64 * 1024,
            crate::process_control::ChildWork::background("content-aware sample verification"),
        ),
    )
    .await
    .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("sample_probe_failed".into());
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

async fn verify_sample(
    path: &std::path::Path,
    frames: usize,
    context: &Context,
    cancelled: &tokio_util::sync::CancellationToken,
) -> Result<(), String> {
    let value = sample_probe_document(path, cancelled).await?;
    let stream = &value["streams"][0];
    for (field, expected) in [
        ("width", serde_json::json!(context.width)),
        ("height", serde_json::json!(context.height)),
        ("nb_read_frames", serde_json::json!(frames.to_string())),
        ("pix_fmt", serde_json::json!("yuv420p")),
        ("color_range", serde_json::json!("tv")),
        ("color_transfer", serde_json::json!("bt709")),
        ("color_primaries", serde_json::json!("bt709")),
        ("color_space", serde_json::json!("bt709")),
    ] {
        if stream[field] != expected {
            return Err(format!("sample_contract_mismatch:{field}"));
        }
    }
    Ok(())
}

async fn rewind_source(source: &std::fs::File) -> Result<(), String> {
    let mut file =
        tokio::fs::File::from_std(source.try_clone().map_err(|error| error.to_string())?);
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn hash_source(
    source: &std::fs::File,
    cancelled: &tokio_util::sync::CancellationToken,
) -> Result<String, String> {
    let mut file = tokio::fs::File::from_std(source.try_clone().map_err(|e| e.to_string())?);
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if cancelled.is_cancelled() {
            return Err("analysis_cancelled".into());
        }
        let read = file.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        bytes += read as u64;
        if bytes > MAX_SOURCE {
            return Err("source_byte_ceiling".into());
        }
        hash.update(&buffer[..read]);
    }
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(|error| error.to_string())?;
    Ok(hex::encode(hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn windows() -> Vec<Vec<Measurement>> {
        vec![
            vec![
                Measurement {
                    bytes: 1000,
                    seconds: 1.0,
                    mean: 94.0,
                    p10: 90.0
                },
                Measurement {
                    bytes: 850,
                    seconds: 1.0,
                    mean: 95.0,
                    p10: 91.0
                },
                Measurement {
                    bytes: 750,
                    seconds: 1.0,
                    mean: 96.0,
                    p10: 92.0
                },
                Measurement {
                    bytes: 650,
                    seconds: 1.0,
                    mean: 93.0,
                    p10: 89.0
                }
            ];
            3
        ]
    }
    /// Run once in final qualification with an installed production encoder
    /// and either the packaged scorer or a native libvmaf-capable FFmpeg.
    #[tokio::test]
    #[ignore = "requires real FFmpeg and VMAF; final batch qualification"]
    async fn native_sample_phase_scores_all_windows_and_cleans_scratch() {
        use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
        use plurx_core::store::SqliteStore;
        let directory = tempfile::tempdir().expect("fixture directory");
        let path = directory.path().join("source.mkv");
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut command = sample_command();
        command.args([
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=24:duration=6",
            // FFV1 receives frame properties from lavfi. Set them on the
            // frames, not just encoder options: unknown input color would
            // otherwise survive into the fixture and fail the SDR contract.
            "-vf",
            "setparams=range=limited:color_primaries=bt709:color_trc=bt709:colorspace=bt709",
            "-c:v",
            "ffv1",
            "-threads",
            "1",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
            "-f",
            "matroska",
            "pipe:1",
        ]);
        run_media(command, &path, &cancel)
            .await
            .expect("materialize bounded fixture");
        let metadata = std::fs::metadata(&path).expect("source metadata");
        let stamp =
            LocalSourceSnapshot::from_file(&std::fs::File::open(&path).expect("source handle"))
                .expect("source stamp");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let library = store
            .create_library(&NewLibrary {
                name: "Fixture".into(),
                kind: LibraryKind::Movies,
                paths: vec![directory.path().to_owned()],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Synthetic sample".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        // The qualification must describe the bytes actually emitted by the
        // installed encoder, never an invented compliant probe document.
        let probe = sample_probe_document(&path, &cancel)
            .await
            .expect("probe actual fixture");
        assert_eq!(probe["streams"][0]["nb_read_frames"], "144");
        let file_id = store
            .upsert_file(
                item,
                path.to_str().expect("fixture path"),
                metadata.len() as i64,
                stamp.modified_secs,
                &ProbeResult {
                    duration_ms: Some(6000),
                    width: Some(320),
                    height: Some(180),
                    video_codec: Some("ffv1".into()),
                    raw_json: Some(probe.to_string()),
                    ..ProbeResult::default()
                },
            )
            .await
            .expect("file");
        let file = store
            .get_file(file_id)
            .await
            .expect("read file")
            .expect("file exists");
        let source = pretranscode_source_snapshot(&file, &[directory.path().to_owned()])
            .await
            .expect("held source");
        let manager = TranscodeManager::new(
            store,
            directory.path().to_owned(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let context = Context {
            version: VERSION,
            source: source.snapshot,
            engine: "synthetic qualification".into(),
            scorer_digest: String::new(),
            probe_digest: "synthetic fixture".into(),
            recipe_version: CACHE_RECIPE_VERSION,
            width: 320,
            height: 180,
            bitrate: 500,
            threads: 2,
            keyframe_args: transcode::hls_keyframe_args(),
            recipes: std::iter::once(EffectiveRateControl::Vbr)
                .chain(QUALITIES.map(|quality| EffectiveRateControl::Qvbr { quality }))
                .map(|mode| {
                    Encoder::Software.encode_args_for(OutputGrade::Sdr, 500, mode, true, Some(2))
                })
                .collect(),
        };
        let report = manager
            .measure_content(&file, &source, context, &probe, &cancel)
            .await
            .expect("bounded native scorer");
        assert_eq!(
            report.outcome, "measured",
            "scorer capability is required for this qualification"
        );
        assert_eq!(report.windows.len(), 3);
        assert!(report.windows.iter().all(|window| window.len() == 4));
        assert_eq!(report.source_sha256.len(), 64);
        assert!(std::fs::read_dir(directory.path())
            .expect("scratch inventory")
            .all(|entry| !entry
                .expect("scratch entry")
                .file_name()
                .to_string_lossy()
                .starts_with("content-encoding-")));
    }

    #[test]
    fn completed_cache_lookup_preserves_live_policy_and_uses_producer_thread_recipe() {
        let baseline = TranscodeOptions {
            effective_rate_control: EffectiveRateControl::Vbr,
            software_threads: Some(6),
            ..TranscodeOptions::default()
        };
        let lookup = content_lookup_options(&baseline, Some(2)).expect("cache candidate");
        assert_eq!(lookup.software_threads, Some(2));
        assert_eq!(baseline.software_threads, Some(6));
        assert_eq!(baseline.effective_rate_control, EffectiveRateControl::Vbr);
        assert_eq!(
            content_lookup_options(&baseline, None),
            Some(baseline.clone())
        );
        assert!(content_lookup_options(&baseline, Some(7)).is_none());
        let explicit = TranscodeOptions {
            effective_rate_control: EffectiveRateControl::Qvbr { quality: 18 },
            ..baseline.clone()
        };
        assert!(content_lookup_options(&explicit, Some(2)).is_none());
        let bound_candidate = TranscodeOptions {
            normalized_geometry: true,
            ..baseline
        };
        assert!(content_lookup_options(&bound_candidate, Some(2)).is_none());
    }

    #[test]
    fn measured_policy_requires_every_window_and_finite_metrics() {
        let mut evidence = windows();
        assert_eq!(winner(&evidence), Some(23));
        evidence[2][2].p10 = 89.9;
        assert_eq!(winner(&evidence), Some(18));
        evidence[0][1].mean = f64::NAN;
        assert_eq!(winner(&evidence), None);
    }
    #[test]
    fn measured_policy_rejects_slow_or_incomplete_evidence() {
        let mut evidence = windows();
        evidence[1][1].seconds = 2.0;
        evidence[1][2].seconds = 2.0;
        assert_eq!(winner(&evidence), None);
        assert_eq!(winner(&evidence[..2]), None);
    }
    #[test]
    fn measured_report_is_bound_to_source_engine_geometry_and_recipe() {
        let file = tempfile::tempfile().expect("fixture succeeds");
        let context = Context {
            version: VERSION,
            source: LocalSourceSnapshot::from_file(&file).expect("fixture succeeds"),
            engine: "engine-a".into(),
            scorer_digest: "scorer-a".into(),
            probe_digest: "probe-a".into(),
            recipe_version: CACHE_RECIPE_VERSION,
            width: 1280,
            height: 720,
            bitrate: 3000,
            threads: 3,
            recipes: vec![vec!["production-args".into()]],
            keyframe_args: transcode::hls_keyframe_args(),
        };
        let mut report = Report {
            context: context.clone(),
            source_sha256: "a".repeat(64),
            outcome: "measured".into(),
            windows: windows(),
        };
        assert_eq!(
            report.rate_for(&context),
            Some(EffectiveRateControl::Qvbr { quality: 23 })
        );
        let mut changed = context.clone();
        changed.engine = "engine-b".into();
        assert_eq!(report.rate_for(&changed), None);
        changed = context.clone();
        changed.threads = 2;
        assert_eq!(report.rate_for(&changed), None);
        changed = context.clone();
        changed.height = 1080;
        assert_eq!(report.rate_for(&changed), None);
        changed = context.clone();
        changed.recipes[0].push("changed".into());
        assert_eq!(report.rate_for(&changed), None);
        file.set_len(1).expect("fixture succeeds");
        changed = context.clone();
        changed.source = LocalSourceSnapshot::from_file(&file).expect("fixture succeeds");
        assert_eq!(report.rate_for(&changed), None);
        report.outcome = "scorer_unavailable".into();
        assert_eq!(report.rate_for(&context), None);
    }
    #[test]
    fn metric_requires_complete_sequential_frames() {
        assert!(metric(br#"{"frames":[{"frameNum":0,"metrics":{"vmaf":95}}]}"#, 2).is_err());
        assert!(metric(br#"{"frames":[{"frameNum":1,"metrics":{"vmaf":95}}]}"#, 1).is_err());
        assert_eq!(
            metric(br#"{"frames":[{"frameNum":0,"metrics":{"vmaf":95}}]}"#, 1)
                .expect("fixture succeeds"),
            (95.0, 95.0)
        );
    }
}
